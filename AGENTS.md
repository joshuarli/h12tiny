# h12tiny project and infrastructure overview

## Purpose and boundaries

`h12tiny` is a Tokio-free, futures-I/O-native HTTP/1.1 and HTTP/2 endpoint
layer. It deliberately separates HTTP protocol machinery from endpoint policy
and optional application conveniences:

```text
application
  └── h12tiny-web (optional routing/extraction vocabulary)
      └── h12tiny-util (protocol-neutral body helpers)

client policy                         server policy
  └── h12tiny-client                    └── h12tiny-server
        └──────────── h12tiny-core ────────────┘
                       └── hyper-futures-lite
                             └── h2-futures-lite
```

Hyper and `h2-futures-lite` own HTTP framing, parsers, HTTP/2 state, HPACK,
flow control, streams, GOAWAY, SETTINGS, and request/response protocol
machinery. h12tiny owns direct-origin connection policy: DNS/TCP, Rustls/ALPN,
per-origin pooling, request normalization, runtime adaptation, lifecycle, and
server H1/H2 selection. Do not move protocol machinery upward into h12tiny.

The root `h12tiny` package is a conditional facade only. It has no default
features. Depend on a component crate when that is the real ownership boundary;
use the facade only for a convenient, explicitly selected combination.

## Workspace map

| Crate | Owns | Must not grow into |
| --- | --- | --- |
| `h12tiny-core` | `FuturesIo`/`HyperIo` bridges, runtime-neutral executor and timer adapters | pooling, connector policy, accept loops, TLS policy, JSON, routing, SSE |
| `h12tiny-client` | direct connector/dialer, DNS/TCP, Rustls/ALPN, normalization, H1/H2 handshakes, pool, safe retry/lifecycle | server code, routing, serde/matchit conveniences |
| `h12tiny-server` | H1/H2 serving, progressive cleartext selection, TLS ALPN dispatch, raw H1 upgrades, TCP/TLS/Unix lifecycle | client pool/connector, router, serde/matchit |
| `h12tiny-util` | bodies and streams, bounded collection, idle-body timeout, optional JSON, Bearer header, replay factories | DNS, TCP, TLS, accept loops, routing |
| `h12tiny-web` | small router, extractors, response conversion, per-route limits/deadlines, optional JSON/query/SSE/CORS/upgrade/WebSocket boundary | protocol implementation, pooling, TLS, a middleware/DI framework, OpenAPI, application session policy |
| `h12tiny` | feature-gated reexports | endpoint implementation |

Dependency direction is a product contract: client and server use core; util is
transport-free; web uses util and reaches server only through the optional
upgrade surface. Client never depends on server or web. Preserve this direction
when adding code or dependencies.

## Feature topology

- `h12tiny-client` and `h12tiny-server` default to no protocol. Their `http1`,
  `http2`, and `tls` features are independent.
- At the facade, `client`, `server`, `util`, and `web` select component crates;
  `http1`, `http2`, and `tls` forward only to roles already selected. A protocol
  flag must never instantiate a role by itself.
- `json` is optional in util/web; `query`, `sse`, and `cors` are web-only.
  Serde-bearing conveniences must end at those layers.
- `upgrade` provides raw HTTP/1 upgrade plumbing. It is the general escape
  hatch for application-selected upgrade protocols.
- `websocket` is an optional, deliberately narrow RFC 6455 boundary. It
  validates HTTP/1.1 handshakes, constructs the `101` response, and adapts the
  upgraded stream to the local futures-lite frame parser. Application message,
  session, authentication, and keepalive policy remain application-owned.
  HTTP/2 extended CONNECT is not implemented.
- `full` is the explicit all-components convenience set. It is never the
  implicit default.

Feature isolation is observable behavior, not an implementation detail. An
H1-only client/server must not acquire H2; client-only builds must not acquire
server/web; plain util must not acquire JSON; plain web must not acquire
JSON/query/WebSocket dependencies. Do not make a convenience feature contagious.

## Transport contracts

### Client and pool

- Pool identity is canonical `scheme + authority`; do not add HTTP/2
  cross-origin connection coalescing or arbitrary connector identity to pool
  keys without a demonstrated need.
- H1 connections are uniquely reserved for one active request exchange. Their
  reusability depends on response completion, EOF, upgrade state, cancellation,
  and driver state; never reduce this to "body finished, return socket".
- H2 connections are shared. Concurrent callers finding an empty H2 origin pool
  must share one logical establishment attempt, and a dropped/failed owner must
  release waiters so a later attempt can succeed. H1 may establish multiple
  concurrent connections.
- Preserve normalization semantics: absolute URI validation, scheme/authority
  routing, origin-form H1 targets, CONNECT authority form, explicit or
  synthesized `Host`, root paths, and IPv4/IPv6/default-port handling. Use the
  `http`/Hyper URI types; do not add a URL stack for this.
- The `Dialer` boundary owns transport selection. Do not bake proxy or
  URI-to-Unix-socket policy into the client. Rustls configuration belongs at the
  connector boundary; one client owns one TLS policy.

### Runtime, timeouts, and cancellation

- Production code is runtime-neutral. Use Hyper's executor boundary and the
  core timer adapter; `smol` is test-only. Do not add Tokio time or a runtime
  dependency.
- Keep timeout scopes distinct: pool idle eviction, connection establishment,
  application handler deadline, and body idle timeout have different contracts.
  Do not introduce an ambiguous whole-request timeout that breaks streaming.
- Cancellation safety is mandatory. Never remove the only resumable
  connection/session state before an await unless cancellation intentionally
  invalidates that state. Pool waits, establishment, TLS/Hyper handshakes,
  dispatch, and body streaming must leave later work viable.
- `FuturesIo` and `HyperIo` are the sole futures-I/O/Hyper adapter boundary.
  Keep unsafe minimal, documented, and covered by focused Miri tests.

### Server and upgrades

- Plaintext auto serving progressively matches the HTTP/2 preface. Divergence
  selects H1 immediately and replays every consumed byte in order; a complete
  preface selects H2 prior knowledge. Do not claim or add HTTP/1.1 `Upgrade:
  h2c` support.
- TLS serving uses Rustls ALPN: `h2` selects H2, `http/1.1` or no ALPN selects
  H1, and an unexpected value errors. Advertise `h2,http/1.1`.
- Lifecycle helpers own listener acceptance and executor-owned connection
  tasks, not process signals. Caller-provided shutdown stops acceptance; pending
  TLS handshakes are cancelled; established drivers drain and are awaited.
  TCP, TLS, and Unix sockets are supported where the platform permits.
- Raw H1 upgrade remains separate from ordinary routing and is the right API
  for non-WebSocket protocols. The optional WebSocket feature is the only
  standardized framing adaptation; it must remain a small transport boundary.

## Application-layer scope

`h12tiny-util` is built around, and reexports, `http-body-util` rather than
replacing it. Its bounded reads must enforce limits while streaming; its
replayability APIs must make body replay explicit rather than pretending any
stream can be retried.

`h12tiny-web` is protocol-agnostic: the same router/service must work over H1,
H2, auto selection, and TLS. It intentionally supplies only the common
application vocabulary: static/parameter/catch-all routes; method dispatch;
`State`, `Path`, `Query`, `Json` (including optional JSON), `Extension`,
`Bytes`, raw `Request`, and raw query extractors; a small `IntoResponse`;
streaming body limits (413); handler deadlines (408); SSE event framing; and
structural CORS. SSE includes `KeepAlive`, which inserts periodic comment
frames only while its upstream is idle; applications choose whether and how to
use that policy.

Do not turn these concepts into Axum compatibility, Tower layering, handler
reflection, proc-macro APIs, or dependency injection. Familiar names are
acceptable because they describe HTTP concepts, not because h12tiny clones a
framework. Likewise, keep client APIs based on `http::Request`/`Response` and
`http_body::Body`, not a Reqwest-like request-builder universe.

The sibling `../smolvm` is a compatibility and integration target, never an
h12tiny dependency. Its OCI policy, redirects, URL/realm validation, token
cache, upload/mount/range behavior, tracing, OpenAPI, WebSocket session/PTY
behavior, and choice of SSE keepalive policy belong in the application.

## Dependency and scope policy

The normal production dependency graph must remain free of:

```text
tokio, tokio-util, native-tls, hyper-util, reqwest, tower, tower-layer, axum,
async-trait, url, mime, cookie, socket2
```

`libc` is deliberately allowed directly or transitively for platform support.
Serde is allowed only behind explicit util/web convenience features.

Do not add HTTP/3/QUIC, h2c upgrade, proxies/SOCKS/system proxy discovery,
cookies, automatic redirects/decompression, compression stacks, multipart or
form frameworks, generic authentication, resolver/cache ecosystems, Alt-Svc,
HSTS, certificate pinning, metrics/OpenTelemetry, OpenAPI, application tracing,
or a general middleware framework unless the project contract is intentionally
changed. Prefer a small reusable primitive proven by a concrete application
fixture; otherwise keep policy application-side.

When porting upstream behavior, preserve useful error distinctions (URI,
scheme, DNS/connect, TLS/ALPN, handshake, dispatch, and pool loss/cancellation)
and their sources where practical. Do not add an error framework solely for
library ergonomics. Keep diagnostics optional rather than making a logging
framework mandatory.

## Upstream provenance

The transport baseline is `hyperium/hyper-util` tag `v0.1.20`, commit
`b23a13e2b7ee73e15ba008cd9b19dcd2d3861957`, under MIT. This is provenance, not
a claim of byte-for-byte identity or a fresh network verification. Preserve
applicable attribution and license information beside any future substantial
copying.

| Local code | Upstream relationship | Local responsibility |
| --- | --- | --- |
| `crates/h12tiny-client/src/normalize.rs` | substantially ported from `client/legacy/client.rs` | origin, Host, request-target, CONNECT normalization |
| `crates/h12tiny-client/src/pool.rs` | substantially ported from `client/legacy/pool.rs` | H1/H2 reservations, waiters, expiry, idle capping |
| `crates/h12tiny-client/src/lib.rs` | substantially ported from `client/legacy/client.rs` | dispatch, handshakes, pool lifecycle, retry boundaries |
| `crates/h12tiny-client/src/connect.rs` | rewritten from the `client/legacy/connect` boundary | async-net DNS/TCP, Rustls/ALPN, dialer, establishment timeout |
| `crates/h12tiny-server/src/conn/auto.rs` | substantially ported from `server/conn/auto` and `common/rewind` | progressive H1/H2 selection and replay |
| `crates/h12tiny-core/src/io.rs` | rewritten from `rt/io` | futures-I/O/Hyper I/O bridges and raw-upgrade adaptation |
| `crates/h12tiny-core/src/runtime.rs` | rewritten from `common/{exec,timer}` | runtime-neutral executor and timer |

The workspace split, util/web, listener lifecycle, raw H1 upgrades, optional
WebSocket boundary, and compatibility fixtures are local extensions rather than
copied Hyper-util code.

## Verification infrastructure

Run the narrowest relevant check first, then broaden for boundary changes. Do
not treat `Cargo.lock` as a dependency-policy check: optional and dev packages
may legitimately appear there.

| Command | What it protects |
| --- | --- |
| `scripts/check-features.sh` | component/facade compile matrix and normal-graph feature isolation |
| `sh scripts/check-normal-dependencies.sh [cargo feature args]` | forbidden packages in the enabled normal graph; defaults to facade `full` |
| `scripts/miri-io.sh` | the unsafe core I/O adapter boundary (requires nightly Miri) |
| `cargo test --workspace --all-features` | unit, transport, lifecycle, web, and compatibility fixtures |
| `scripts/interop.sh` | curl/nghttp and independent `nghttpd` interoperability; configure its endpoint environment variables |
| `scripts/bench.sh` | repeatable loopback H1/H2 comparison and h2load; not a substitute for correctness testing |

Important integration coverage lives in `tests/`: raw-wire and adversarial H1;
H2 lifecycle and cancellation regressions; plaintext/TLS H1/H2 dogfood;
client compatibility for bounded/streaming bodies, bearer retry, and mTLS;
web transport/TLS; Unix lifecycle; raw upgrades; and WebSocket interop. Extend
the nearest existing fixture when changing one of these contracts.

The deterministic interop server listens on `127.0.0.1:3000` (HTTP) and
`127.0.0.1:3443` (HTTPS) when run with
`cargo run --release --features server,http1,http2,tls --example interop-server`.
The committed TLS certificate is localhost-only and suitable for local
`curl --insecure` / `nghttp -y` checks, not production. See
`scripts/README.md` for endpoint variables, benchmark controls, and expected
body sizes.

## Change checklist

Before changing an API, feature, transport invariant, dependency, or lifecycle
state machine:

1. Identify the owning crate and preserve the dependency direction.
2. State the feature-graph and normal-dependency impact; update the mechanical
   checks for any intentional contract change.
3. Add or update an observable regression/integration test, especially around
   pool ownership, cancellation, request normalization, byte replay, ALPN, or
   stream limits.
4. Keep application policy at the application boundary and document any
   intentional scope expansion here and in public-facing docs.
5. Run the focused test/check first, then the relevant workspace/feature gate.
