#![cfg(all(
    feature = "server",
    feature = "web",
    feature = "upgrade",
    feature = "http1"
))]

use async_net::{TcpListener, TcpStream};
use fastwebsockets::{after_handshake_split, Frame, OpCode, Role};
use futures_lite::io::{AsyncReadExt as LiteAsyncReadExt, AsyncWriteExt as LiteAsyncWriteExt};
use h12tiny::io::{FuturesIo, HyperIo};
use h12tiny::runtime::{BoxExecutor, BoxSendFuture};
use h12tiny::server::conn::auto;
use h12tiny::util;
use h12tiny::web::{get, HttpUpgrade, Router};
use http::{Response, StatusCode};

#[derive(Clone, Copy, Debug)]
struct TestExecutor;

impl hyper::rt::Executor<BoxSendFuture> for TestExecutor {
    fn execute(&self, future: BoxSendFuture) {
        std::thread::spawn(move || futures_lite::future::block_on(future));
    }
}

fn websocket_echo(upgrade: HttpUpgrade) -> Response<util::BoxBody> {
    std::thread::spawn(move || {
        futures_lite::future::block_on(async move {
            let upgraded = upgrade
                .on_upgrade
                .await
                .expect("the H1 upgrade must resolve for the websocket route");
            let (read, write) = futures_util::io::AsyncReadExt::split(HyperIo::new(upgraded));
            let (mut reader, mut writer) = after_handshake_split(read, write, Role::Server);
            reader.set_auto_close(false);
            reader.set_auto_pong(false);
            let frame = reader
                .read_frame(&mut |_| async { Ok::<(), std::io::Error>(()) })
                .await
                .expect("the sibling websocket parser must read the client frame");
            assert_eq!(frame.opcode, OpCode::Text);
            assert_eq!(&*frame.payload, b"hello");
            writer
                .write_frame(Frame::new(frame.fin, frame.opcode, None, frame.payload))
                .await
                .expect("the sibling websocket parser must write the echo frame");
        });
    });

    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        // RFC 6455's documented sample-key result. The fixture client below
        // uses this fixed key so the h12tiny route owns only HTTP upgrade
        // mechanics while the application owns WebSocket validation/framing.
        .header("Sec-WebSocket-Accept", "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=")
        .body(util::boxed_body(util::empty_body()))
        .unwrap()
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
                    get(|upgrade: HttpUpgrade| async move { websocket_echo(upgrade) }),
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
