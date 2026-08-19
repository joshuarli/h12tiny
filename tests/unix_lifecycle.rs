#![cfg(all(unix, feature = "server", feature = "http1"))]

mod support;

use std::convert::Infallible;
use std::future::{ready, Ready};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use async_net::unix::{UnixListener, UnixStream};
use futures_channel::oneshot;
use futures_lite::io::{AsyncReadExt, AsyncWriteExt};
use h12tiny::runtime::BoxExecutor;
use h12tiny::server::serve;
use http::{Request, Response};
use hyper::body::Incoming;
use hyper::service::Service;
use support::{FullBody, SmolExecutor};

#[derive(Clone, Copy)]
struct UnixService;

impl Service<Request<Incoming>> for UnixService {
    type Response = Response<FullBody>;
    type Error = Infallible;
    type Future = Ready<Result<Self::Response, Self::Error>>;

    fn call(&self, _request: Request<Incoming>) -> Self::Future {
        ready(Ok(Response::new(FullBody::from_static(b"unix"))))
    }
}

struct SocketPath {
    path: PathBuf,
}

impl SocketPath {
    fn new() -> io::Result<Self> {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

        let base = std::env::temp_dir();
        for attempt in 0..32 {
            let directory = base.join(format!(
                "h12u-{}-{}-{attempt}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&directory) {
                Ok(()) => return Ok(Self { path: directory.join("server.sock") }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique Unix socket directory",
        ))
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SocketPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        if let Some(directory) = self.path.parent() {
            let _ = std::fs::remove_dir(directory);
        }
    }
}

#[test]
fn unix_listener_serves_h1_and_drains_on_shutdown() {
    smol::block_on(async {
        let socket = SocketPath::new().expect("allocate unique Unix socket path");
        let listener = UnixListener::bind(socket.path()).expect("bind Unix listener");
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let server = smol::spawn(async move {
            serve(
                listener,
                UnixService,
                BoxExecutor::new(SmolExecutor),
            )
            .shutdown_on(async move {
                let _ = shutdown_rx.await;
            })
            .await
        });

        let mut client = UnixStream::connect(socket.path())
            .await
            .expect("connect Unix client");
        client
            .write_all(b"GET /unix HTTP/1.1\r\nHost: localhost\r\nConnection: keep-alive\r\n\r\n")
            .await
            .expect("write request");

        let mut response = Vec::new();
        let mut buffer = [0_u8; 128];
        while !response.windows(4).any(|window| window == b"unix") {
            let count = client.read(&mut buffer).await.expect("read response");
            assert!(count > 0, "server closed before the H1 response body");
            response.extend_from_slice(&buffer[..count]);
        }
        assert!(response.starts_with(b"HTTP/1.1 200"), "response: {response:?}");

        shutdown_tx.send(()).expect("signal shutdown");
        server.await.expect("Unix lifecycle completed");

        let count = client
            .read(&mut buffer)
            .await
            .expect("read drained Unix connection");
        assert_eq!(count, 0, "connection remained open after graceful drain");
    });
}
