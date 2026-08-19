# h12tiny

`h12tiny` is a Tokio-free, futures-I/O-native HTTP stack with deliberately
separate protocol, transport, body, and application layers. Hyper and the
sibling `h2-futures-lite` own HTTP framing and state machines; h12tiny owns
direct-origin transport policy, pooling, runtime adaptation, and the optional
small application vocabulary.

The facade has no default features. Select only the roles and protocols an
application needs, or depend on a component crate directly for the most
precise normal dependency graph.

## Workspace layers

```text
h12tiny-core    futures-I/O bridge and runtime-neutral executor/timer
h12tiny-client  direct client, normalization, pool, dialer, TLS/ALPN
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
enabling `web` does not enable H1, H2, or TLS.

## Direct component dependencies

Use component crates when their ownership boundary is the application boundary:

```toml
h12tiny-client = { version = "0.1", default-features = false, features = ["http1"] }
h12tiny-util = { version = "0.1" }
```

`h12tiny-util/json`, `h12tiny-web/json`, and `h12tiny-web/query` are optional;
an H1-only client does not acquire JSON, routing, server, TLS, or H2 code.

## Intentional scope

The default stack does not include Tokio, native TLS, Reqwest, Axum, Tower,
proxy or redirect policy, OpenAPI generation, HTTP/3, or an application
tracing framework. The optional `websocket` feature adds RFC 6455 HTTP/1.1
validation, `101` response construction, and futures-lite server-role framing;
message policy remains application-owned. Raw H1 upgrade remains available for
other protocols, and HTTP/2 extended CONNECT is not implemented.

Run the enforced feature/dependency matrix with:

```sh
scripts/check-features.sh
```

The examples require explicit facade features because the default is empty:

```sh
cargo run --release --features server,http1,http2,tls --example interop-server
cargo run --release --features client,http1,http2,tls --example client-load -- http://127.0.0.1:3000/1k
```
