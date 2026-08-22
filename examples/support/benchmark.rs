//! Shared deterministic service for the HTTP benchmark examples.
//!
//! Keep this module deliberately small and transport-agnostic: the
//! `interop-server` exercises h12tiny's auto server layer, while
//! `h2-reference-server` calls Hyper's H2 builder directly. Sharing this
//! service makes their response body and application work identical.

use std::convert::Infallible;
use std::future::{Ready, ready};
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use h12tiny::runtime::BoxSendFuture;
use http::{Request, Response, StatusCode};
use http_body::{Body, Frame};
use hyper::body::Incoming;
use hyper::service::Service;

static BODY_1K: [u8; 1024] = [b'x'; 1024];
static BODY_64K: [u8; 64 * 1024] = [b'x'; 64 * 1024];

/// The executor shared by benchmark endpoints.
#[derive(Clone, Copy, Debug)]
pub struct SmolExecutor;

impl hyper::rt::Executor<BoxSendFuture> for SmolExecutor {
    fn execute(&self, future: BoxSendFuture) {
        smol::spawn(future).detach();
    }
}

/// A response body that emits its complete payload in one frame.
#[derive(Debug)]
pub struct OneFrameBody(Option<Bytes>);

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

/// Deterministic payload service shared by benchmark endpoint variants.
#[derive(Clone, Copy, Debug)]
pub struct BenchmarkService;

impl Service<Request<Incoming>> for BenchmarkService {
    type Response = Response<OneFrameBody>;
    type Error = Infallible;
    type Future = Ready<Result<Self::Response, Self::Error>>;

    fn call(&self, request: Request<Incoming>) -> Self::Future {
        let (status, body) = match request.uri().path() {
            "/0" => (StatusCode::OK, Bytes::new()),
            "/1k" => (StatusCode::OK, Bytes::from_static(&BODY_1K)),
            "/64k" => (StatusCode::OK, Bytes::from_static(&BODY_64K)),
            _ => (
                StatusCode::NOT_FOUND,
                Bytes::from_static(b"use /0, /1k, or /64k\n"),
            ),
        };
        ready(Ok(Response::builder()
            .status(status)
            .header("content-type", "application/octet-stream")
            .body(OneFrameBody::from_bytes(body))
            .expect("benchmark response builder is valid")))
    }
}
