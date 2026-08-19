#![cfg(all(feature = "client", feature = "http1"))]

use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use async_net::TcpListener;
use bytes::Bytes;
use futures_lite::io::{AsyncReadExt, AsyncWriteExt};
use futures_util::future::{self, Either};
use h12tiny::client::Client;
use h12tiny::runtime::BoxSendFuture;
use http::{Request, StatusCode};
use http_body::{Body, Frame};

/// The client owns no task runtime: tests supply this narrow executor bridge.
#[derive(Clone)]
struct SmolExecutor;

impl hyper::rt::Executor<BoxSendFuture> for SmolExecutor {
    fn execute(&self, future: BoxSendFuture) {
        smol::spawn(future).detach();
    }
}

/// A zero-length request body without adding an HTTP-body helper dependency.
struct EmptyBody;

impl Body for EmptyBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Poll::Ready(None)
    }

    fn is_end_stream(&self) -> bool {
        true
    }
}

#[test]
fn direct_h1_uses_origin_form_and_synthesizes_host_on_the_wire() {
    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = smol::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 4096];
            let count = socket.read(&mut request).await.unwrap();
            request.truncate(count);
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            request
        });

        let client = Client::builder(SmolExecutor).build::<EmptyBody>();
        let uri = format!("http://{address}/foo?x=1");
        let response = client
            .request(Request::builder().uri(uri).body(EmptyBody).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        drop(response);

        let request = peer.await;
        let wire = std::str::from_utf8(&request).unwrap();
        assert!(
            wire.starts_with(&format!("GET /foo?x=1 HTTP/1.1\r\nHost: {address}\r\n")),
            "unexpected wire request: {wire:?}"
        );
        assert!(!wire.starts_with("GET http://"));
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

#[test]
fn sequential_h1_requests_reuse_one_direct_connection() {
    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = smol::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let first = read_head(&mut socket).await;
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            let second = read_head(&mut socket).await;
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            (first, second)
        });

        let client = Client::builder(SmolExecutor).build::<EmptyBody>();
        for path in ["/one", "/two"] {
            let response = client
                .request(
                    Request::builder()
                        .uri(format!("http://{address}{path}"))
                        .body(EmptyBody)
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            drop(response);
        }
        drop(client);

        let deadline = async_io::Timer::after(Duration::from_secs(2));
        let (first, second) = match future::select(peer, deadline).await {
            Either::Left((requests, _)) => requests,
            Either::Right(_) => panic!("second request did not reuse the open HTTP/1 connection"),
        };
        assert!(std::str::from_utf8(&first).unwrap().starts_with("GET /one HTTP/1.1"));
        assert!(std::str::from_utf8(&second).unwrap().starts_with("GET /two HTTP/1.1"));
    });
}

#[test]
fn closed_h1_session_is_not_reused_and_next_request_reconnects() {
    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = smol::spawn(async move {
            for response in [
                b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".as_slice(),
                b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".as_slice(),
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let _request = read_head(&mut socket).await;
                socket.write_all(response).await.unwrap();
            }
        });

        let client = Client::builder(SmolExecutor).build::<EmptyBody>();
        for path in ["/closed", "/reconnected"] {
            let response = client
                .request(
                    Request::builder()
                        .uri(format!("http://{address}{path}"))
                        .body(EmptyBody)
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            drop(response);
        }
        drop(client);

        let deadline = async_io::Timer::after(Duration::from_secs(2));
        match future::select(peer, deadline).await {
            Either::Left(((), _)) => {}
            Either::Right(_) => panic!("client did not reconnect after server closed HTTP/1"),
        }
    });
}
