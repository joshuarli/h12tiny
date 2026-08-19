//! Small loopback server for the external interoperability and benchmark
//! harnesses.
//!
//! The endpoint body is selected only by the path:
//! `/0` is empty, `/1k` is 1 KiB, and `/64k` is 64 KiB.  The TLS listener
//! uses the committed localhost fixture and advertises both `h2` and
//! `http/1.1` through ALPN.  This is intentionally a low-level example, not
//! a general-purpose HTTP server or router.

use std::convert::Infallible;
use std::env;
use std::future::{ready, Ready};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use async_net::TcpListener;
use bytes::Bytes;
use futures_rustls::TlsAcceptor;
use h12tiny::io::FuturesIo;
use h12tiny::runtime::{BoxExecutor, BoxSendFuture};
use h12tiny::server::conn::auto;
use http::{Request, Response, StatusCode};
use http_body::{Body, Frame};
use hyper::body::Incoming;
use hyper::service::Service;

static BODY_1K: [u8; 1024] = [b'x'; 1024];
static BODY_64K: [u8; 64 * 1024] = [b'x'; 64 * 1024];

#[derive(Clone, Copy, Debug)]
struct SmolExecutor;

impl hyper::rt::Executor<BoxSendFuture> for SmolExecutor {
    fn execute(&self, future: BoxSendFuture) {
        smol::spawn(future).detach();
    }
}

#[derive(Debug)]
struct OneFrameBody(Option<Bytes>);

impl OneFrameBody {
    fn from_bytes(body: Bytes) -> Self {
        Self((!body.is_empty()).then_some(body))
    }
}

impl Body for OneFrameBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Poll::Ready(self.0.take().map(|body| Ok(Frame::data(body))))
    }

    fn is_end_stream(&self) -> bool {
        self.0.is_none()
    }
}

#[derive(Clone, Copy, Debug)]
struct BenchmarkService;

impl Service<Request<Incoming>> for BenchmarkService {
    type Response = Response<OneFrameBody>;
    type Error = Infallible;
    type Future = Ready<Result<Self::Response, Self::Error>>;

    fn call(&self, request: Request<Incoming>) -> Self::Future {
        let (status, body) = match request.uri().path() {
            "/0" => (StatusCode::OK, Bytes::new()),
            "/1k" => (StatusCode::OK, Bytes::from_static(&BODY_1K)),
            "/64k" => (StatusCode::OK, Bytes::from_static(&BODY_64K)),
            _ => (StatusCode::NOT_FOUND, Bytes::from_static(b"use /0, /1k, or /64k\n")),
        };
        ready(Ok(Response::builder()
            .status(status)
            .header("content-type", "application/octet-stream")
            .body(OneFrameBody::from_bytes(body))
            .expect("benchmark response builder is valid")))
    }
}

fn tls_config() -> rustls::ServerConfig {
    use futures_rustls::pki_types::{CertificateDer, PrivateKeyDer};

    let certificate = CertificateDer::from(include_bytes!("../tests/fixtures/tls/cert.der").to_vec());
    let key = PrivateKeyDer::try_from(include_bytes!("../tests/fixtures/tls/key.der").to_vec())
        .expect("the committed TLS fixture key must be PKCS#8 DER");
    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![certificate], key)
        .expect("the committed TLS fixture certificate and key must match");
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    config
}

async fn serve_plain(listener: TcpListener, executor: BoxExecutor) {
    loop {
        let (stream, peer) = listener.accept().await.expect("plain listener failed");
        let executor = executor.clone();
        smol::spawn(async move {
            let result = auto::Builder::new(executor)
                .serve_connection(FuturesIo::new(stream), BenchmarkService)
                .await;
            if let Err(error) = result {
                eprintln!("plain connection {peer} ended with error: {error}");
            }
        })
        .detach();
    }
}

async fn serve_tls(listener: TcpListener, executor: BoxExecutor, acceptor: TlsAcceptor) {
    loop {
        let (stream, peer) = listener.accept().await.expect("TLS listener failed");
        let executor = executor.clone();
        let acceptor = acceptor.clone();
        smol::spawn(async move {
            let tls = match acceptor.accept(stream).await {
                Ok(tls) => tls,
                Err(error) => {
                    eprintln!("TLS handshake from {peer} failed: {error}");
                    return;
                }
            };
            let connection = match auto::Builder::new(executor)
                .serve_tls_connection(tls, BenchmarkService)
            {
                Ok(connection) => connection,
                Err(error) => {
                    eprintln!("TLS protocol selection for {peer} failed: {error}");
                    return;
                }
            };
            if let Err(error) = connection.await {
                eprintln!("TLS connection {peer} ended with error: {error}");
            }
        })
        .detach();
    }
}

#[derive(Debug)]
struct Options {
    plain: Option<String>,
    tls: Option<String>,
}

fn usage() -> &'static str {
    "usage: cargo run --example interop-server -- [--plain ADDR] [--tls ADDR]\n\n\
defaults: --plain 127.0.0.1:3000 --tls 127.0.0.1:3443\n\
endpoints: /0, /1k, /64k"
}

fn options() -> Result<Options, String> {
    let mut plain = Some("127.0.0.1:3000".to_owned());
    let mut tls = Some("127.0.0.1:3443".to_owned());
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            "--plain" => plain = Some(args.next().ok_or("--plain needs ADDR".to_owned())?),
            "--tls" => tls = Some(args.next().ok_or("--tls needs ADDR".to_owned())?),
            "--no-plain" => plain = None,
            "--no-tls" => tls = None,
            other => return Err(format!("unknown option {other:?}\n\n{}", usage())),
        }
    }
    if plain.is_none() && tls.is_none() {
        return Err("at least one listener must be enabled".to_owned());
    }
    Ok(Options { plain, tls })
}

fn main() {
    let options = match options() {
        Ok(options) => options,
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(2);
        }
    };

    smol::block_on(async move {
        let executor = BoxExecutor::new(SmolExecutor);
        let mut listeners = Vec::new();
        if let Some(address) = options.plain {
            let listener = TcpListener::bind(&address).await?;
            eprintln!("h12tiny plaintext server listening on http://{}", listener.local_addr()?);
            listeners.push(smol::spawn(serve_plain(listener, executor.clone())));
        }
        if let Some(address) = options.tls {
            let listener = TcpListener::bind(&address).await?;
            eprintln!("h12tiny TLS server listening on https://{}", listener.local_addr()?);
            let acceptor = TlsAcceptor::from(Arc::new(tls_config()));
            listeners.push(smol::spawn(serve_tls(listener, executor, acceptor)));
        }
        for listener in listeners {
            listener.await;
        }
        Ok::<(), Box<dyn std::error::Error>>(())
    })
    .unwrap_or_else(|error| {
        eprintln!("server failed: {error}");
        std::process::exit(1);
    });
}
