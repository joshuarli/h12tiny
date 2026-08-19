//! Raw HTTP/1 upgrade support.
//!
//! This module re-exports Hyper's low-level upgrade API without adding a
//! WebSocket or other application protocol.  A service checks the request,
//! returns a `101 Switching Protocols` response, and calls [`on`] with that
//! request to obtain an [`OnUpgrade`] future.  The future resolves to an
//! [`Upgraded`] bidirectional stream once the H1 connection driver has
//! completed the protocol transition.
//! [`h12tiny_core::io::HyperIo`] adapts that stream for application framing
//! libraries that use `futures_io` rather than Hyper's runtime traits.
//!
//! The re-export is available only with the `upgrade` feature.  It is tied to
//! the H1 connection path; this crate does not claim HTTP/2 extended CONNECT
//! or WebSocket support.

#[doc(inline)]
pub use hyper::upgrade::{on, OnUpgrade, Parts, Upgraded};

#[cfg(test)]
mod tests {
    use super::on;
    use bytes::Bytes;
    use futures_lite::prelude::*;
    use h12tiny_core::io::FuturesIo;
    use h12tiny_core::runtime::{BoxExecutor, BoxSendFuture};
    use http_body::{Body, Frame, SizeHint};
    use hyper::body::Incoming;
    use hyper::rt::{Read, ReadBuf, Write};
    use hyper::service::Service;
    use http::{Request, Response};
    use std::convert::Infallible;
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    #[derive(Debug)]
    struct EmptyBody;

    impl Body for EmptyBody {
        type Data = Bytes;
        type Error = Infallible;

        fn poll_frame(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            Poll::Ready(None)
        }

        fn is_end_stream(&self) -> bool {
            true
        }

        fn size_hint(&self) -> SizeHint {
            SizeHint::with_exact(0)
        }
    }

    #[derive(Clone, Copy, Debug)]
    struct UpgradeService;

    impl Service<Request<Incoming>> for UpgradeService {
        type Response = Response<EmptyBody>;
        type Error = Infallible;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn call(&self, mut request: Request<Incoming>) -> Self::Future {
            let on_upgrade = on(&mut request);
            Box::pin(async move {
                std::thread::spawn(move || {
                    futures_lite::future::block_on(async move {
                        let Ok(mut upgraded) = on_upgrade.await else {
                            return;
                        };
                        let mut storage = [std::mem::MaybeUninit::<u8>::uninit(); 64];
                        let mut read = ReadBuf::uninit(&mut storage);
                        let read_result = futures_lite::future::poll_fn(|cx| {
                            Pin::new(&mut upgraded).poll_read(cx, read.unfilled())
                        })
                        .await;
                        if read_result.is_err() || read.filled().is_empty() {
                            return;
                        }
                        let payload = read.filled().to_vec();
                        let mut written = 0;
                        while written < payload.len() {
                            let result = futures_lite::future::poll_fn(|cx| {
                                Pin::new(&mut upgraded).poll_write(cx, &payload[written..])
                            })
                            .await;
                            match result {
                                Ok(0) | Err(_) => return,
                                Ok(n) => written += n,
                            }
                        }
                    });
                });
                Ok(Response::builder()
                    .status(101)
                    .header("Connection", "Upgrade")
                    .header("Upgrade", "echo")
                    .body(EmptyBody)
                    .expect("valid upgrade response"))
            })
        }
    }

    #[derive(Clone, Copy, Debug)]
    struct TestExecutor;

    impl hyper::rt::Executor<BoxSendFuture> for TestExecutor {
        fn execute(&self, future: BoxSendFuture) {
            std::thread::spawn(move || futures_lite::future::block_on(future));
        }
    }

    #[test]
    fn raw_h1_tcp_upgrade_echoes_bytes() {
        let (address, server) = futures_lite::future::block_on(async {
            let listener = async_net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind test listener");
            let address = listener.local_addr().expect("listener address");
            let server = std::thread::spawn(move || {
                futures_lite::future::block_on(async move {
                    let (stream, _) = listener.accept().await.expect("accept test stream");
                    let builder = crate::conn::auto::Builder::new(BoxExecutor::new(TestExecutor))
                        .http1_only();
                    let connection = builder.serve_connection(
                        FuturesIo::new(stream),
                        UpgradeService,
                    );
                    connection.await.expect("serve upgraded connection");
                });
            });
            (address, server)
        });

        futures_lite::future::block_on(async {
            let mut client = async_net::TcpStream::connect(address)
                .await
                .expect("connect test listener");
            client
                .write_all(
                    b"GET /upgrade HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: echo\r\n\r\necho-payload",
                )
                .await
                .expect("write upgrade request");

            let mut received = Vec::new();
            let mut buffer = [0u8; 128];
            for _ in 0..32 {
                let count = client.read(&mut buffer).await.expect("read upgrade response");
                if count == 0 {
                    break;
                }
                received.extend_from_slice(&buffer[..count]);
                if received.windows(b"echo-payload".len()).any(|window| {
                    window == b"echo-payload"
                }) {
                    break;
                }
            }
            assert!(received.starts_with(b"HTTP/1.1 101"), "response: {received:?}");
            assert!(received
                .windows(b"echo-payload".len())
                .any(|window| window == b"echo-payload"));
        });

        server.join().expect("join upgrade server");
    }
}
