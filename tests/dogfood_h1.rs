#![cfg(all(feature = "client", feature = "server", feature = "http1"))]

use std::convert::Infallible;
use std::future::{ready, Ready};
use std::pin::Pin;
use std::task::{Context, Poll};

use async_net::TcpListener;
use bytes::Bytes;
use h12tiny::client::Client;
use h12tiny::io::FuturesIo;
use h12tiny::runtime::{BoxExecutor, BoxSendFuture};
use h12tiny::server::conn::auto;
use http::{Request, Response, StatusCode};
use http_body::{Body, Frame};
use hyper::body::Incoming;
use hyper::service::Service;

#[derive(Clone)]
struct SmolExecutor;

impl hyper::rt::Executor<BoxSendFuture> for SmolExecutor {
    fn execute(&self, future: BoxSendFuture) {
        smol::spawn(future).detach();
    }
}

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

#[derive(Clone)]
struct EmptyService;

impl Service<Request<Incoming>> for EmptyService {
    type Response = Response<EmptyBody>;
    type Error = Infallible;
    type Future = Ready<Result<Self::Response, Self::Error>>;

    fn call(&self, _: Request<Incoming>) -> Self::Future {
        ready(Ok(Response::new(EmptyBody)))
    }
}

#[test]
fn h1_client_and_auto_server_interoperate_over_plaintext() {
    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = smol::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            auto::Builder::new(BoxExecutor::new(SmolExecutor))
                .serve_connection(FuturesIo::new(stream), EmptyService)
                .await
                .unwrap();
        });

        let client = Client::builder(SmolExecutor).build::<EmptyBody>();
        let response = client
            .request(
                Request::builder()
                    .uri(format!("http://{address}/http1"))
                    .body(EmptyBody)
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        drop(response);
        drop(client);
        server.await;
    });
}
