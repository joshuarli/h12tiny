#![cfg(all(
    feature = "client",
    feature = "server",
    feature = "http1",
    feature = "http2",
    feature = "tls",
    feature = "json"
))]

mod support;

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

use async_net::TcpListener;
use bytes::Bytes;
use futures_rustls::TlsAcceptor;
use futures_util::{stream, StreamExt};
use h12tiny::client::{Client, Connector};
use h12tiny::io::FuturesIo;
use h12tiny::runtime::BoxExecutor;
use h12tiny::server::conn::auto;
use h12tiny::util::{self, BodyExt, BodyFactory, BoxBody, ReplayableRequest, ResponseBodyExt};
use http::header::{AUTHORIZATION, CONTENT_TYPE, WWW_AUTHENTICATE};
use http::{Request, Response, StatusCode};
use hyper::body::Incoming;
use hyper::service::Service;
use support::SmolExecutor;

#[derive(Debug, Clone, PartialEq, Eq)]
struct RequestRecord {
    path: String,
    authorization: Option<String>,
    body: Vec<u8>,
}

#[derive(Default)]
struct CompatState {
    requests: Mutex<Vec<RequestRecord>>,
}

#[derive(Clone)]
struct CompatService {
    state: Arc<CompatState>,
}

impl Service<Request<Incoming>> for CompatService {
    type Response = Response<BoxBody>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, request: Request<Incoming>) -> Self::Future {
        let state = self.state.clone();
        Box::pin(async move {
            let (parts, body) = request.into_parts();
            let path = parts.uri.path().to_owned();
            let authorization = parts
                .headers
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let body = util::collect_bytes_limited(body, 2 * 1024 * 1024)
                .await
                .expect("compatibility request body is bounded");
            state.requests.lock().unwrap().push(RequestRecord {
                path: path.clone(),
                authorization: authorization.clone(),
                body: body.to_vec(),
            });

            let response = match path.as_str() {
                "/bearer-json" => {
                    if authorization.as_deref() == Some("Bearer test-token") {
                        let response = util::json_response(&("authorized", 2_u64)).unwrap();
                        let (parts, body) = response.into_parts();
                        Response::from_parts(parts, util::boxed_body(body))
                    } else {
                        Response::builder()
                            .status(StatusCode::UNAUTHORIZED)
                            .header(WWW_AUTHENTICATE, "Bearer realm=\"compat\"")
                            .body(boxed_bytes(Bytes::from_static(b"token required")))
                            .unwrap()
                    }
                }
                "/upload" => {
                    let status = if authorization.as_deref() == Some("Bearer test-token") {
                        StatusCode::OK
                    } else {
                        StatusCode::UNAUTHORIZED
                    };
                    let mut response = Response::builder()
                        .status(status)
                        .body(boxed_bytes(Bytes::from_static(b"upload received")))
                        .unwrap();
                    if status == StatusCode::UNAUTHORIZED {
                        response.headers_mut().insert(
                            WWW_AUTHENTICATE,
                            http::HeaderValue::from_static("Bearer realm=\"compat\""),
                        );
                    }
                    response
                }
                "/download" => {
                    let body = util::stream_body(stream::iter([
                        Ok::<_, Infallible>(Bytes::from_static(b"download-")),
                        Ok(Bytes::from_static(b"streaming-")),
                        Ok(Bytes::from_static(b"payload")),
                    ]));
                    Response::new(util::boxed_body(body))
                }
                "/oversized" => boxed_response(Bytes::from(vec![b'x'; 64 * 1024])),
                "/healthy" => boxed_response(Bytes::from_static(b"healthy")),
                "/mtls" => boxed_response(Bytes::from_static(b"mtls-ok")),
                _ => Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(boxed_bytes(Bytes::from_static(b"not found")))
                    .unwrap(),
            };
            Ok(response)
        })
    }
}

fn boxed_bytes(bytes: impl Into<Bytes>) -> BoxBody {
    util::boxed_body(util::bytes_body(bytes))
}

fn boxed_response(bytes: impl Into<Bytes>) -> Response<BoxBody> {
    Response::new(boxed_bytes(bytes))
}

async fn spawn_plaintext_server(service: CompatService) -> (std::net::SocketAddr, smol::Task<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = smol::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(connection) => connection,
                Err(_) => break,
            };
            let service = service.clone();
            smol::spawn(async move {
                let _ = auto::Builder::new(BoxExecutor::new(SmolExecutor))
                    .http1_only()
                    .serve_connection(FuturesIo::new(stream), service)
                    .await;
            })
            .detach();
        }
    });
    (address, server)
}

fn request_template(uri: String) -> Request<()> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(CONTENT_TYPE, "application/json")
        .body(())
        .unwrap()
}

fn recorded(state: &CompatState) -> Vec<RequestRecord> {
    state.requests.lock().unwrap().clone()
}

#[test]
fn json_request_bounded_response_and_application_bearer_retry() {
    smol::block_on(async {
        let state = Arc::new(CompatState::default());
        let (address, server) = spawn_plaintext_server(CompatService {
            state: state.clone(),
        })
        .await;
        let client = Client::builder(SmolExecutor).build::<BoxBody>();
        let calls = Arc::new(AtomicUsize::new(0));
        let factory = {
            let calls = calls.clone();
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                let body = util::json_body(&("pull", 1_u64)).unwrap();
                util::boxed_body(body)
            }
        };
        let initial = ReplayableRequest::new(
            request_template(format!("http://{address}/bearer-json")),
            factory.clone(),
        );

        // A 401 is an application decision point. The first response is
        // consumed before the application recreates the request with a token.
        let first = client.request(initial.build()).await.unwrap();
        assert_eq!(first.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(first.headers()[WWW_AUTHENTICATE], "Bearer realm=\"compat\"");
        first.bytes_limited(128).await.unwrap();

        let mut authenticated_template = initial.template().clone();
        authenticated_template
            .headers_mut()
            .insert(AUTHORIZATION, util::bearer("test-token").unwrap());
        let authenticated = ReplayableRequest::new(authenticated_template, factory);
        let response = client.request(authenticated.build()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let result: (String, u64) = response.json_limited(128).await.unwrap();
        assert_eq!(result, ("authorized".to_owned(), 2));
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        let records = recorded(&state);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].path, "/bearer-json");
        assert_eq!(records[0].authorization, None);
        assert_eq!(records[0].body, br#"["pull",1]"#);
        assert_eq!(
            records[1].authorization.as_deref(),
            Some("Bearer test-token")
        );
        assert_eq!(records[1].body, records[0].body);

        drop(client);
        drop(server);
    });
}

#[test]
fn streaming_upload_requires_fresh_factory_and_is_not_implicitly_retried() {
    smol::block_on(async {
        let state = Arc::new(CompatState::default());
        let (address, server) = spawn_plaintext_server(CompatService {
            state: state.clone(),
        })
        .await;
        let client = Client::builder(SmolExecutor).build::<BoxBody>();
        let calls = Arc::new(AtomicUsize::new(0));
        let factory = BodyFactory::new({
            let calls = calls.clone();
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                let body = util::stream_body(stream::iter([
                    Ok::<_, Infallible>(Bytes::from_static(b"upload-")),
                    Ok(Bytes::from_static(b"stream")),
                ]));
                util::boxed_body(body)
            }
        });

        let initial = request_template(format!("http://{address}/upload"));
        let first = client
            .request(Request::from_parts(initial.into_parts().0, factory.make()))
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::UNAUTHORIZED);
        first.bytes_limited(128).await.unwrap();

        let mut authenticated_template = request_template(format!("http://{address}/upload"));
        authenticated_template
            .headers_mut()
            .insert(AUTHORIZATION, util::bearer("test-token").unwrap());
        let second = client
            .request(Request::from_parts(
                authenticated_template.into_parts().0,
                factory.make(),
            ))
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::OK);
        second.bytes_limited(128).await.unwrap();

        // Only the two application-built streams reached the server. In
        // particular, the 401 did not cause h12tiny to replay the consumed
        // one-shot stream on its own.
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let records = recorded(&state);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].body, b"upload-stream");
        assert_eq!(records[1].body, b"upload-stream");
        assert_eq!(
            records[1].authorization.as_deref(),
            Some("Bearer test-token")
        );

        drop(client);
        drop(server);
    });
}

#[test]
fn streaming_download_writes_incrementally_to_application_sink() {
    smol::block_on(async {
        let state = Arc::new(CompatState::default());
        let (address, server) = spawn_plaintext_server(CompatService { state }).await;
        let client = Client::builder(SmolExecutor).build::<BoxBody>();
        let response = client
            .request(
                Request::get(format!("http://{address}/download"))
                    .body(boxed_bytes(Bytes::new()))
                    .unwrap(),
            )
            .await
            .unwrap();

        let expected = [b"download-".as_slice(), b"streaming-", b"payload"];
        let mut data = response.into_body().into_data_stream();
        let mut sink_bytes = 0;
        let mut sink_checksum = 0_u64;
        let mut chunks = 0;
        while let Some(chunk) = data.next().await {
            let chunk = chunk.unwrap();
            assert!(
                chunks < expected.len(),
                "download emitted an unexpected frame"
            );
            assert_eq!(chunk.as_ref(), expected[chunks]);
            chunks += 1;
            sink_bytes += chunk.len();
            sink_checksum = chunk.iter().fold(sink_checksum, |sum, byte| {
                sum.wrapping_add(u64::from(*byte))
            });
        }
        assert!(chunks >= 3, "expected one sink write per response frame");
        assert_eq!(
            sink_bytes,
            expected.iter().map(|chunk| chunk.len()).sum::<usize>()
        );
        assert_eq!(
            sink_checksum,
            expected
                .iter()
                .flat_map(|chunk| chunk.iter())
                .fold(0_u64, |sum, byte| sum.wrapping_add(u64::from(*byte)))
        );

        drop(client);
        drop(server);
    });
}

#[test]
fn oversized_bounded_response_does_not_poison_the_next_pooled_request() {
    smol::block_on(async {
        let state = Arc::new(CompatState::default());
        let (address, server) = spawn_plaintext_server(CompatService {
            state: state.clone(),
        })
        .await;
        let client = Client::builder(SmolExecutor).build::<BoxBody>();

        let oversized = client
            .request(
                Request::get(format!("http://{address}/oversized"))
                    .body(boxed_bytes(Bytes::new()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let error = oversized.bytes_limited(1024).await.unwrap_err();
        assert!(error.is_limit_exceeded());
        let limit = error.limit_error().unwrap();
        assert_eq!(limit.limit(), 1024);
        assert!(limit.received() > limit.limit());

        let healthy = client
            .request(
                Request::get(format!("http://{address}/healthy"))
                    .body(boxed_bytes(Bytes::new()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(healthy.status(), StatusCode::OK);
        assert_eq!(
            healthy.bytes_limited(64).await.unwrap(),
            Bytes::from_static(b"healthy")
        );

        let records = recorded(&state);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].path, "/oversized");
        assert_eq!(records[1].path, "/healthy");
        drop(client);
        drop(server);
    });
}

#[test]
fn custom_rustls_config_uses_client_certificate_and_custom_root() {
    smol::block_on(async {
        use futures_rustls::pki_types::{CertificateDer, PrivateKeyDer};
        use rustls::server::WebPkiClientVerifier;

        let certificate = CertificateDer::from(include_bytes!("fixtures/tls/cert.der").to_vec());
        let key = PrivateKeyDer::try_from(include_bytes!("fixtures/tls/key.der").to_vec())
            .expect("fixture key is valid DER");
        let mut client_roots = rustls::RootCertStore::empty();
        client_roots
            .add(certificate.clone())
            .expect("fixture certificate is a valid client root");
        let mut client_config = rustls::ClientConfig::builder()
            .with_root_certificates(client_roots)
            .with_client_auth_cert(vec![certificate.clone()], key.clone_key())
            .expect("fixture cert and key are a client identity");
        client_config.alpn_protocols = vec![b"http/1.1".to_vec()];

        let mut server_roots = rustls::RootCertStore::empty();
        server_roots
            .add(certificate.clone())
            .expect("fixture certificate is a valid server root");
        let verifier = WebPkiClientVerifier::builder(Arc::new(server_roots))
            .build()
            .expect("fixture client root builds a verifier");
        let server_key = PrivateKeyDer::try_from(include_bytes!("fixtures/tls/key.der").to_vec())
            .expect("fixture key is valid DER");
        let mut server_config = rustls::ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(vec![certificate], server_key)
            .expect("fixture cert and key are a server identity");
        server_config.alpn_protocols = vec![b"http/1.1".to_vec()];

        let state = Arc::new(CompatState::default());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server_state = state.clone();
        let server = smol::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let tls = TlsAcceptor::from(Arc::new(server_config))
                .accept(stream)
                .await
                .expect("client certificate must satisfy the server verifier");
            assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"http/1.1"[..]));
            auto::Builder::new(BoxExecutor::new(SmolExecutor))
                .serve_tls_connection(
                    tls,
                    CompatService {
                        state: server_state,
                    },
                )
                .unwrap()
                .await
                .unwrap();
        });

        let connector = Connector::builder().tls_config(client_config).build();
        let mut builder = Client::builder(SmolExecutor);
        builder.connector(connector);
        let client = builder.build::<BoxBody>();
        let response = client
            .request(
                Request::get(format!("https://localhost:{}/mtls", address.port()))
                    .body(boxed_bytes(Bytes::new()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.bytes_limited(64).await.unwrap(),
            Bytes::from_static(b"mtls-ok")
        );

        drop(client);
        server.await;
    });
}
