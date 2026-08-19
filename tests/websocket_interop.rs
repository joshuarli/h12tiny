#![cfg(all(
    feature = "server",
    feature = "web",
    feature = "upgrade",
    feature = "websocket",
    feature = "http1"
))]

use async_net::{TcpListener, TcpStream};
use futures_lite::io::{AsyncReadExt as LiteAsyncReadExt, AsyncWriteExt as LiteAsyncWriteExt};
use h12tiny::io::FuturesIo;
use h12tiny::runtime::{BoxExecutor, BoxSendFuture};
use h12tiny::server::conn::auto;
use h12tiny::util;
use h12tiny::web::{get, Router, WebSocketFrame, WebSocketOpCode, WebSocketUpgrade};
use http::Response;

#[derive(Clone, Copy, Debug)]
struct TestExecutor;

impl hyper::rt::Executor<BoxSendFuture> for TestExecutor {
    fn execute(&self, future: BoxSendFuture) {
        std::thread::spawn(move || futures_lite::future::block_on(future));
    }
}

fn websocket_echo(upgrade: WebSocketUpgrade) -> Response<util::BoxBody> {
    let response = upgrade.response();
    std::thread::spawn(move || {
        futures_lite::future::block_on(async move {
            let connection = upgrade
                .into_connection()
                .await
                .expect("the H1 upgrade must resolve for the websocket route");
            let (mut reader, mut writer) = connection.split();
            let frame = reader
                .read_frame(&mut |_| async { Ok::<(), std::io::Error>(()) })
                .await
                .expect("the sibling websocket parser must read the client frame");
            assert_eq!(frame.opcode, WebSocketOpCode::Text);
            assert_eq!(&*frame.payload, b"hello");
            writer
                .write_frame(WebSocketFrame::new(
                    frame.fin,
                    frame.opcode,
                    None,
                    frame.payload,
                ))
                .await
                .expect("the sibling websocket parser must write the echo frame");
        });
    });

    response
}

async fn read_response_headers(stream: &mut TcpStream) -> Vec<u8> {
    let mut response = Vec::new();
    let mut byte = [0_u8; 1];
    while !response.ends_with(b"\r\n\r\n") {
        let count = stream.read(&mut byte).await.unwrap();
        assert_ne!(count, 0, "peer closed before switching protocols");
        response.extend_from_slice(&byte[..count]);
    }
    response
}

#[test]
fn web_route_composes_with_the_futures_lite_websocket_parser() {
    let (address, server) = futures_lite::future::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            futures_lite::future::block_on(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let router = Router::<()>::new().route(
                    "/ws",
                    get(|upgrade: WebSocketUpgrade| async move { websocket_echo(upgrade) }),
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
                b"GET /ws HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
            )
            .await
            .unwrap();
        let response = read_response_headers(&mut client).await;
        assert!(response.starts_with(b"HTTP/1.1 101"), "response: {response:?}");
        assert!(
            response
                .windows(b"sec-websocket-accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=".len())
                .any(|value| value == b"sec-websocket-accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo="),
            "response: {response:?}"
        );

        // FIN/text, 5-byte masked payload, a fixed mask, then "hello" XORed
        // with that mask. The server-side sibling parser unmasks it and emits
        // an unmasked server text frame.
        client
            .write_all(&[0x81, 0x85, 1, 2, 3, 4, 0x69, 0x67, 0x6f, 0x68, 0x6e])
            .await
            .unwrap();
        let mut echo = [0_u8; 7];
        client.read_exact(&mut echo).await.unwrap();
        assert_eq!(echo, *b"\x81\x05hello");
    });

    server.join().unwrap();
}
