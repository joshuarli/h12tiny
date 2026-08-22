# h12tiny

`h12tiny` is a Tokio-free HTTP stack with deliberately separate protocol,
transport, body, and application layers. The futures-I/O client is built on
Hyper and the sibling `h2-futures-lite`; the blocking client is a deliberately
small direct-origin HTTP/1.1 codec. h12tiny owns direct-origin transport
policy, pooling where applicable, runtime adaptation, and the optional small
application vocabulary.

The facade has no default features. Select only the roles and protocols an
application needs, or depend on a component crate directly for the most
precise normal dependency graph.

## Workspace layers

```text
h12tiny-core    futures-I/O bridge and runtime-neutral executor/timer
h12tiny-client  direct client, normalization, pool, dialer, TLS/ALPN
h12tiny-client-sync blocking direct HTTP/1.1 client, std I/O, optional Rustls
h12tiny-server  H1/H2 serving, ALPN, raw H1 upgrade, listener lifecycle
h12tiny-util    bodies, bounded collection, streams, idle timeout, JSON
h12tiny-web     optional router, extractors, response conversion, SSE
h12tiny         conditional facade reexports only
```

The dependencies point downward only: web uses util and (only for raw upgrade)
server; client and server use core; core uses the local Hyper substrate. Client
never depends on server or web, and util never depends on transport.

## Facade configurations

Minimal HTTP/1 client:

```toml
h12tiny = { version = "0.1", default-features = false, features = ["client", "http1"] }
```

HTTP/2 TLS client:

```toml
h12tiny = { version = "0.1", default-features = false, features = ["client", "http2", "tls"] }
```

Blocking HTTPS/HTTP/1.1 client:

```toml
h12tiny = { version = "0.1", default-features = false, features = ["client-sync", "tls"] }
```

HTTP/1 + HTTP/2 TLS server:

```toml
h12tiny = { version = "0.1", default-features = false, features = ["server", "http1", "http2", "tls"] }
```

Full small application server:

```toml
h12tiny = { version = "0.1", default-features = false, features = [
  "server", "http1", "http2", "tls", "web", "json", "query", "sse", "upgrade",
] }
```

The facade forwards features only to already-selected component crates. For
example, `http1` does not instantiate a client or server on its own, and
enabling `web` does not enable H1, H2, or TLS. `client-sync` is intrinsically
HTTP/1.1-only; it does not participate in `http1` or `http2` forwarding.

## Direct component dependencies

Use component crates when their ownership boundary is the application boundary:

```toml
h12tiny-client = { version = "0.1", default-features = false, features = ["http1"] }
h12tiny-client-sync = { version = "0.1", default-features = false, features = ["tls"] }
h12tiny-util = { version = "0.1" }
```

`h12tiny-util/json`, `h12tiny-web/json`, and `h12tiny-web/query` are optional;
an H1-only client does not acquire JSON, routing, server, TLS, or H2 code.
`h12tiny-client-sync` does not acquire Hyper, futures, an async runtime, or
HTTP/2. Its response body implements `std::io::Read` and owns one connection;
there is no pool, proxy policy, redirect policy, or upgrade support.

## Intentional scope

The default stack does not include Tokio, native TLS, Reqwest, Axum, Tower,
proxy or redirect policy, OpenAPI generation, HTTP/3, or an application
tracing framework. The optional `websocket` feature adds RFC 6455 HTTP/1.1
validation, `101` response construction, and futures-lite server-role framing;
message policy remains application-owned. Raw H1 upgrade remains available for
other protocols, and HTTP/2 extended CONNECT is not implemented.

TLS uses `rustls` with the pure-Rust Graviola provider from
`rustls-graviola`. No `ring`, `aws-lc-rs`, OpenSSL, or
native-tls backend is enabled. The built-in client selects Graviola explicitly.
`h12tiny_client::ClientTlsConfigBuilder` and
`h12tiny_client_sync::ClientTlsConfigBuilder` expose those same defaults for
custom root stores, mutual TLS, and ALPN, without installing or consulting a
process-global Rustls provider or disabling an embedding application's default
provider. Applications that construct their own
`rustls::ClientConfig` or `rustls::ServerConfig` should likewise select a
provider per configuration:

```rust,no_run
let provider = std::sync::Arc::new(rustls_graviola::default_provider());
let builder = rustls::ClientConfig::builder_with_provider(provider)
    .with_safe_default_protocol_versions()
    .expect("Graviola supports Rustls' safe default protocol versions");
```

`rustls-webpki` remains as Rustls' certificate-validation implementation; it
is not a separate crypto backend. The selected Graviola release currently
supports `x86_64` and `aarch64`; TLS builds for other architectures remain an
upstream limitation rather than falling back to another crypto backend.

Run the enforced feature/dependency matrix with:

```sh
scripts/check-features.sh
```

The examples require explicit facade features because the default is empty:

```sh
cargo run --release --features server,http1,http2,tls --example interop-server
cargo run --release --features client,http1,http2,tls --example client-load -- http://127.0.0.1:3000/1k
```
