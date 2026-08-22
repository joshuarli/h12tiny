#![cfg(all(
    feature = "client",
    feature = "server",
    feature = "web",
    feature = "http1",
    feature = "http2"
))]

mod support;

use async_net::TcpListener;
use h12tiny::client::Client;
use h12tiny::io::FuturesIo;
use h12tiny::runtime::BoxExecutor;
use h12tiny::server::conn::auto;
use h12tiny::util::{self, BoxBody, ResponseBodyExt};
use h12tiny::web::{Router, get};
use http::{Request, StatusCode};
use support::SmolExecutor;

async fn serve_once(h2: bool) -> (std::net::SocketAddr, smol::Task<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = smol::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let builder = auto::Builder::new(BoxExecutor::new(SmolExecutor));
        let builder = if h2 {
            builder.http2_only()
        } else {
            builder.http1_only()
        };
        let router = Router::<()>::new().route("/web", get(|| async { "router" }));
        builder
            .serve_connection(FuturesIo::new(stream), router)
            .await
            .unwrap();
    });
    (address, server)
}

async fn request_web(h2: bool) {
    let (address, server) = serve_once(h2).await;
    let mut builder = Client::builder(SmolExecutor);
    if h2 {
        builder.http2_only(true);
    }
    let client = builder.build::<BoxBody>();
    let response = client
        .request(
            Request::get(format!("http://{address}/web"))
                .body(util::boxed_body(util::empty_body()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.text_limited(64).await.unwrap(), "router");
    drop(client);
    server.await;
}

#[test]
fn router_is_protocol_neutral_over_h1_and_h2() {
    smol::block_on(async {
        request_web(false).await;
        request_web(true).await;
    });
}
