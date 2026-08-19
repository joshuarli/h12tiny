#![cfg(all(
    feature = "client",
    feature = "server",
    feature = "http1",
    feature = "http2",
    feature = "tls"
))]

mod support;

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_net::TcpListener;
use bytes::Bytes;
use futures_rustls::TlsAcceptor;
use futures_util::future::{self, Either};
use h12tiny::client::{Client, Connector, ErrorKind};
use h12tiny::runtime::BoxExecutor;
use h12tiny::server::conn::auto;
use http::header::CONTENT_LENGTH;
use http::{Request, Response, StatusCode};
use hyper::body::Incoming;
use hyper::service::Service;
use support::{
    collect, fixture_client_config, fixture_server_config, ConnectionCounters, FullBody,
    SmolExecutor, YieldingBody,
};

#[derive(Clone)]
struct EchoService {
    counters: ConnectionCounters,
}

impl Service<Request<Incoming>> for EchoService {
    type Response = Response<FullBody>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, request: Request<Incoming>) -> Self::Future {
        self.counters.request_started();
        Box::pin(async move {
            let body = collect(request.into_body()).await;
            let length = b"echo:".len() + body.len();
            Ok(Response::builder()
                .header(CONTENT_LENGTH, length)
                .body(FullBody::from_bytes(Bytes::from(
                    [b"echo:".as_slice(), body.as_slice()].concat(),
                )))
                .unwrap())
        })
    }
}

#[test]
fn tls_alpn_http11_validates_fixture_certificate_and_streams_bodies() {
    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let counters = ConnectionCounters::default();
        let server_counters = counters.clone();
        let server = smol::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            server_counters.tcp_opened();
            let mut config = fixture_server_config();
            config.alpn_protocols = vec![b"http/1.1".to_vec()];
            let tls = TlsAcceptor::from(Arc::new(config)).accept(stream).await.unwrap();
            assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"http/1.1"[..]));
            server_counters.tls_completed();
            server_counters.h1_opened();
            auto::Builder::new(BoxExecutor::new(SmolExecutor))
                .serve_tls_connection(tls, EchoService { counters: server_counters.clone() })
                .unwrap()
                .await
                .unwrap();
        });

        let connector = Connector::with_tls_config(fixture_client_config());
        let mut builder = Client::builder(SmolExecutor);
        builder.connector(connector);
        let client = builder.build::<YieldingBody>();
        let response = client
            .request(
                Request::builder()
                    .method("POST")
                    .uri(format!("https://localhost:{port}/echo"))
                    .header(CONTENT_LENGTH, 6)
                    .body(YieldingBody::new([
                        Bytes::from_static(b"str"),
                        Bytes::from_static(b"eam"),
                    ]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(collect(response.into_body()).await, b"echo:stream");
        drop(client);
        server.await;
        assert_eq!(
            counters.snapshot(),
            support::CounterSnapshot {
                tcp_connections: 1,
                tls_handshakes: 1,
                h1_connections: 1,
                h2_sessions: 0,
                logical_requests: 1,
            }
        );
    });
}

#[test]
fn concurrent_first_tls_h2_requests_share_one_handshake_and_session() {
    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let counters = ConnectionCounters::default();
        let server_counters = counters.clone();
        let server = smol::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            server_counters.tcp_opened();
            let tls = TlsAcceptor::from(Arc::new(fixture_server_config()))
                .accept(stream)
                .await
                .unwrap();
            assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"h2"[..]));
            server_counters.tls_completed();
            server_counters.h2_opened();
            auto::Builder::new(BoxExecutor::new(SmolExecutor))
                .serve_tls_connection(tls, EchoService { counters: server_counters.clone() })
                .unwrap()
                .await
                .unwrap();
        });

        let connector = Connector::with_tls_config(fixture_client_config());
        let mut builder = Client::builder(SmolExecutor);
        builder.connector(connector).http2_only(true);
        let client = builder.build::<FullBody>();
        let requests = (0..100)
            .map(|request| {
                let request = client.clone().request(
                    Request::builder()
                        .uri(format!("https://localhost:{port}/stream/{request}"))
                        .body(FullBody::empty())
                        .unwrap(),
                );
                async move {
                    let response = request.await?;
                    let body = collect(response.into_body()).await;
                    Ok::<_, h12tiny::client::Error>(body)
                }
            })
            .collect::<Vec<_>>();
        let deadline = async_io::Timer::after(Duration::from_secs(5));
        let responses = match future::select(future::join_all(requests), deadline).await {
            Either::Left((responses, _)) => responses,
            Either::Right(_) => panic!("first TLS H2 requests did not converge on one session"),
        };
        for response in responses {
            assert_eq!(response.unwrap(), b"echo:");
        }
        drop(client);
        server.await;
        assert_eq!(
            counters.snapshot(),
            support::CounterSnapshot {
                tcp_connections: 1,
                tls_handshakes: 1,
                h1_connections: 0,
                h2_sessions: 1,
                logical_requests: 100,
            }
        );
    });
}

#[test]
fn untrusted_tls_certificate_surfaces_a_tls_error() {
    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = smol::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            // A client that rejects our untrusted fixture may close before the
            // server handshake completes. Either result proves no application
            // HTTP connection was accepted for this test.
            let _ = TlsAcceptor::from(Arc::new(fixture_server_config()))
                .accept(stream)
                .await;
        });

        let client = Client::builder(SmolExecutor).build::<FullBody>();
        let error = client
            .request(
                Request::builder()
                    .uri(format!("https://localhost:{port}/untrusted"))
                    .body(FullBody::empty())
                    .unwrap(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Tls);
        drop(client);
        server.await;
    });
}

#[test]
fn unexpected_tls_alpn_surfaces_an_alpn_error() {
    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = smol::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut config = fixture_server_config();
            config.alpn_protocols = vec![b"not-http".to_vec()];
            let tls = TlsAcceptor::from(Arc::new(config)).accept(stream).await.unwrap();
            assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"not-http"[..]));
        });

        let mut client_config = fixture_client_config();
        client_config.alpn_protocols = vec![b"not-http".to_vec()];
        let connector = Connector::with_tls_config(client_config);
        let mut builder = Client::builder(SmolExecutor);
        builder.connector(connector);
        let client = builder.build::<FullBody>();
        let error = client
            .request(
                Request::builder()
                    .uri(format!("https://localhost:{port}/unexpected-alpn"))
                    .body(FullBody::empty())
                    .unwrap(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Alpn);
        drop(client);
        server.await;
    });
}
