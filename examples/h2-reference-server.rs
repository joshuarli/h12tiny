//! Direct Hyper HTTP/2 benchmark reference.
//!
//! This is intentionally not a production server. It shares the benchmark
//! service, futures-I/O adapter, runtime, and listener with `interop-server`,
//! while omitting h12tiny-server's auto-protocol selection and lifecycle layer.
//! Use it only to attribute that endpoint-layer overhead in a local H2 prior-
//! knowledge comparison.

mod support {
    pub mod benchmark;
}

use std::env;

use async_net::TcpListener;
use h12tiny::io::FuturesIo;
use h12tiny::runtime::BoxExecutor;
use hyper::server::conn::http2;

use support::benchmark::{BenchmarkService, SmolExecutor};

#[derive(Debug)]
struct Options {
    address: String,
}

fn usage() -> &'static str {
    "usage: cargo run --example h2-reference-server -- [--addr ADDR]\n\n\
default: --addr 127.0.0.1:3001\n\
endpoints: /0, /1k, /64k\n\n\
This is a direct-Hyper H2 prior-knowledge benchmark reference, not a production server."
}

fn options() -> Result<Options, String> {
    let mut address = "127.0.0.1:3001".to_owned();
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            "--addr" => address = args.next().ok_or("--addr needs ADDR".to_owned())?,
            other => return Err(format!("unknown option {other:?}\n\n{}", usage())),
        }
    }
    Ok(Options { address })
}

async fn serve(listener: TcpListener, executor: BoxExecutor) {
    loop {
        let (stream, peer) = listener.accept().await.expect("reference listener failed");
        let executor = executor.clone();
        smol::spawn(async move {
            if let Err(error) = http2::Builder::new(executor)
                .serve_connection(FuturesIo::new(stream), BenchmarkService)
                .await
            {
                eprintln!("direct Hyper H2 connection {peer} ended with error: {error}");
            }
        })
        .detach();
    }
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
        let listener = TcpListener::bind(&options.address).await?;
        eprintln!(
            "direct Hyper H2 reference listening on http://{}",
            listener.local_addr()?
        );
        serve(listener, BoxExecutor::new(SmolExecutor)).await;
        Ok::<(), Box<dyn std::error::Error>>(())
    })
    .unwrap_or_else(|error| {
        eprintln!("reference server failed: {error}");
        std::process::exit(1);
    });
}
