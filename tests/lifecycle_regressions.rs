#![cfg(feature = "client")]

mod support;

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use async_net::TcpListener;
use futures_channel::oneshot;
use futures_lite::io::{AsyncReadExt, AsyncWriteExt};
use futures_util::future::{self, Either};
use h12tiny::client::{Client, DebugEvent, DebugEventLog, ErrorKind};
use http::{Request, Response, StatusCode};
use hyper::body::Incoming;
use hyper::service::Service;
use support::{ConnectionCounters, FullBody, SmolExecutor, collect};

#[cfg(all(feature = "server", feature = "http2"))]
#[derive(Clone)]
struct H2Service {
    counters: ConnectionCounters,
}

#[cfg(all(feature = "server", feature = "http2"))]
impl Service<Request<Incoming>> for H2Service {
    type Response = Response<FullBody>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, request: Request<Incoming>) -> Self::Future {
        self.counters.request_started();
        Box::pin(async move {
            let _ = collect(request.into_body()).await;
            Ok(Response::new(FullBody::from_static(b"ok")))
        })
    }
}

#[cfg(all(feature = "server", feature = "http2"))]
#[test]
fn failed_h2_establishment_releases_multiple_waiters_for_later_success() {
    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let counters = ConnectionCounters::default();
        let server_counters = counters.clone();
        let (accepted_tx, accepted_rx) = oneshot::channel();
        let (replacement_accepted_tx, replacement_accepted_rx) = oneshot::channel();
        let (fail_tx, fail_rx) = oneshot::channel();
        let server = smol::spawn(async move {
            let (first, _) = listener.accept().await.unwrap();
            server_counters.tcp_opened();
            accepted_tx.send(()).unwrap();
            // Keep the first H2 handshake pending until every client has
            // reached the pool checkout. This makes the failed owner/waiter
            // transition deterministic instead of relying on scheduler luck.
            fail_rx.await.unwrap();
            drop(first);

            let (second, _) = listener.accept().await.unwrap();
            server_counters.tcp_opened();
            server_counters.h2_opened();
            replacement_accepted_tx.send(()).unwrap();
            let _ = hyper::server::conn::http2::Builder::new(h12tiny::runtime::BoxExecutor::new(
                SmolExecutor,
            ))
            .serve_connection(
                h12tiny::io::FuturesIo::new(second),
                H2Service {
                    counters: server_counters.clone(),
                },
            )
            .await;
        });

        let events = DebugEventLog::default();
        let mut builder = Client::builder(SmolExecutor);
        builder.http2_only(true);
        builder.debug_event_log(events.clone());
        let client = builder.build::<FullBody>();

        let mut requests = Vec::new();
        for path in ["/one", "/two", "/three"] {
            requests.push(smol::spawn(
                client.clone().request(
                    Request::builder()
                        .uri(format!("http://{address}{path}"))
                        .body(FullBody::empty())
                        .unwrap(),
                ),
            ));
        }

        accepted_rx.await.unwrap();
        let mut checkouts = 0;
        let mut observed = Vec::new();
        for _ in 0..200 {
            let mut drained = events.drain();
            checkouts += drained
                .iter()
                .filter(|event| matches!(event, DebugEvent::PoolCheckout { .. }))
                .count();
            observed.append(&mut drained);
            if checkouts >= 3 {
                break;
            }
            async_io::Timer::after(Duration::from_millis(1)).await;
        }
        assert_eq!(checkouts, 3, "not all H2 requests reached the pool wait");
        let _ = fail_tx.send(());
        replacement_accepted_rx.await.unwrap();

        let request_counters = counters.clone();
        let all_requests = async move {
            let mut owner_failures = 0;
            let mut waiter_successes = 0;
            for result in future::join_all(requests).await {
                match result {
                    Ok(response) => {
                        assert_eq!(response.status(), StatusCode::OK);
                        assert_eq!(collect(response.into_body()).await, b"ok");
                        waiter_successes += 1;
                    }
                    Err(_) => owner_failures += 1,
                }
            }
            // The owner has reached the failed peer and cannot safely replay
            // its request. Every waiter parked behind that owner must instead
            // be released to retry on the replacement H2 session.
            assert_eq!(owner_failures, 1, "events before failure: {observed:?}");
            assert_eq!(
                waiter_successes,
                2,
                "counters: {:?}",
                request_counters.snapshot()
            );
        };
        match future::select(
            Box::pin(all_requests),
            Box::pin(async_io::Timer::after(Duration::from_secs(3))),
        )
        .await
        {
            Either::Left(((), _)) => {}
            Either::Right(_) => panic!("H2 waiters remained stranded after failed establishment"),
        }

        // The requests parked behind the failed owner all completed. A new
        // request after that batch must still find the replacement session.
        let later = client
            .request(
                Request::builder()
                    .uri(format!("http://{address}/later"))
                    .body(FullBody::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(collect(later.into_body()).await, b"ok");

        assert_eq!(
            counters.snapshot(),
            support::CounterSnapshot {
                tcp_connections: 2,
                tls_handshakes: 0,
                h1_connections: 0,
                h2_sessions: 1,
                logical_requests: 3,
            }
        );

        drop(client);
        server.await;
    });
}

async fn read_head(stream: &mut async_net::TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let mut byte = [0; 1];
    loop {
        let count = stream.read(&mut byte).await.unwrap();
        assert_ne!(count, 0, "peer closed before completing request headers");
        request.extend_from_slice(&byte[..count]);
        if request.ends_with(b"\r\n\r\n") {
            return request;
        }
    }
}

#[cfg(feature = "http1")]
#[test]
fn stale_idle_h1_socket_is_evicted_before_or_during_next_dispatch() {
    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (response_sent_tx, response_sent_rx) = oneshot::channel();
        let (close_tx, close_rx) = oneshot::channel();
        let (closed_tx, closed_rx) = oneshot::channel();
        let (replacement_tx, replacement_rx) = oneshot::channel();
        let server = smol::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            let _ = read_head(&mut first).await;
            first
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            response_sent_tx.send(()).unwrap();
            close_rx.await.unwrap();
            drop(first);
            closed_tx.send(()).unwrap();

            let (mut replacement, _) = listener.accept().await.unwrap();
            loop {
                let request = read_head(&mut replacement).await;
                replacement
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                    .await
                    .unwrap();
                if request.starts_with(b"GET /replacement HTTP/1.1") {
                    replacement_tx.send(request).unwrap();
                    break;
                }
            }
        });

        let events = DebugEventLog::default();
        let mut builder = Client::builder(SmolExecutor);
        builder.debug_event_log(events.clone());
        let client = builder.build::<FullBody>();
        let first = client
            .request(
                Request::builder()
                    .uri(format!("http://{address}/first"))
                    .body(FullBody::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        drop(first);
        response_sent_rx.await.unwrap();

        // The initial response must have returned its H1 sender to the idle
        // pool before the peer is closed. This is the stale-idle boundary.
        let mut pooled = false;
        for _ in 0..200 {
            pooled |= events.drain().into_iter().any(|event| {
                matches!(event, DebugEvent::ConnectionPooled { origin } if origin == format!("http://{address}"))
            });
            if pooled {
                break;
            }
            async_io::Timer::after(Duration::from_millis(1)).await;
        }
        assert!(pooled, "first H1 sender never reached the idle pool");

        close_tx.send(()).unwrap();
        closed_rx.await.unwrap();

        let stale = client
            .request(
                Request::builder()
                    .uri(format!("http://{address}/stale"))
                    .body(FullBody::empty())
                    .unwrap(),
            )
            .await;
        match stale {
            // The close reached Hyper after it accepted the request for
            // serialization. The endpoint must surface that indeterminate
            // dispatch rather than replay an arbitrary method/body.
            Err(error) => assert_eq!(error.kind(), ErrorKind::SendRequest),
            // The driver observed the peer close before checkout. Reopening a
            // connection before serializing this request is equally correct;
            // it is the race-free form of stale-idle eviction.
            Ok(response) => {
                assert_eq!(response.status(), StatusCode::OK);
                drop(response);
            }
        }

        let replacement = client
            .request(
                Request::builder()
                    .uri(format!("http://{address}/replacement"))
                    .body(FullBody::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(replacement.status(), StatusCode::OK);
        drop(replacement);

        let request = replacement_rx.await.unwrap();
        assert!(
            String::from_utf8(request)
                .unwrap()
                .starts_with("GET /replacement HTTP/1.1")
        );

        drop(client);
        server.await;
    });
}
