#![cfg(all(
    feature = "server",
    feature = "web",
    feature = "upgrade",
    feature = "http1"
))]

use async_net::{TcpListener, TcpStream};
use futures_lite::prelude::*;
use h12tiny::io::FuturesIo;
use h12tiny::runtime::{BoxExecutor, BoxSendFuture};
use h12tiny::server::conn::auto;
use h12tiny::util;
use h12tiny::web::{get, HttpUpgrade, Router};
use http::Response;
use hyper::rt::{Read, ReadBuf, Write};
use std::pin::Pin;

#[derive(Clone, Copy, Debug)]
struct TestExecutor;

impl hyper::rt::Executor<BoxSendFuture> for TestExecutor {
    fn execute(&self, future: BoxSendFuture) {
        std::thread::spawn(move || futures_lite::future::block_on(future));
    }
}

fn echo_upgrade(upgrade: HttpUpgrade) -> Response<util::BoxBody> {
    std::thread::spawn(move || {
        futures_lite::future::block_on(async move {
            let Ok(mut upgraded) = upgrade.on_upgrade.await else {
                return;
            };
            let mut storage = [std::mem::MaybeUninit::<u8>::uninit(); 64];
            let mut read = ReadBuf::uninit(&mut storage);
            let result = futures_lite::future::poll_fn(|cx| {
                Pin::new(&mut upgraded).poll_read(cx, read.unfilled())
            })
            .await;
            if result.is_err() || read.filled().is_empty() {
                return;
            }
            let payload = read.filled().to_vec();
            let mut written = 0;
            while written < payload.len() {
                match futures_lite::future::poll_fn(|cx| {
                    Pin::new(&mut upgraded).poll_write(cx, &payload[written..])
                })
                .await
                {
                    Ok(0) | Err(_) => return,
                    Ok(count) => written += count,
                }
            }
        });
    });
    Response::builder()
        .status(101)
        .header("Connection", "Upgrade")
        .header("Upgrade", "echo")
        .body(util::boxed_body(util::empty_body()))
        .unwrap()
}

#[test]
fn web_http_upgrade_composes_with_the_raw_server_upgrade() {
    let (address, server) = futures_lite::future::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            futures_lite::future::block_on(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let router = Router::<()>::new().route(
                    "/raw",
                    get(|upgrade: HttpUpgrade| async move { echo_upgrade(upgrade) }),
                );
                auto::Builder::new(BoxExecutor::new(TestExecutor))
                    .http1_only()
                    .serve_connection(FuturesIo::new(stream), router)
                    .await
                    .unwrap();
            });
        });
        (address, server)
    });

    futures_lite::future::block_on(async {
        let mut client = TcpStream::connect(address).await.unwrap();
        client
            .write_all(
                b"GET /raw HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: echo\r\n\r\nweb-echo",
            )
            .await
            .unwrap();
        let mut received = Vec::new();
        let mut buffer = [0_u8; 128];
        for _ in 0..32 {
            let count = client.read(&mut buffer).await.unwrap();
            if count == 0 {
                break;
            }
            received.extend_from_slice(&buffer[..count]);
            if received
                .windows(b"web-echo".len())
                .any(|window| window == b"web-echo")
            {
                break;
            }
        }
        assert!(
            received.starts_with(b"HTTP/1.1 101"),
            "response: {received:?}"
        );
        assert!(received
            .windows(b"web-echo".len())
            .any(|window| window == b"web-echo"));
    });

    server.join().unwrap();
}
