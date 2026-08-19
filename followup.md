Yes. I’d split it at the **crate boundary first, features second**. That prevents an H1-only client from even compiling server/router/TLS/H2 code, rather than merely trusting `cfg`s inside one large crate.

The facade crate can still make every combination ergonomic.

# Extend completed `h12tiny` for practical Axum/Reqwest replacement without becoming a framework

The original `h12tiny` implementation plan is **already complete and correct**.

Do not revisit or redesign the completed transport work.

Assume we already have a production-shaped, Tokio-free, futures-lite-native stack providing:

* HTTP/1.1 client
* HTTP/2 client
* HTTP/1.1 server
* HTTP/2 server
* per-origin connection pooling
* mature ported `hyper-util::client::legacy` semantics
* DNS/TCP via `async-net`
* TLS via `rustls` + `futures-rustls`
* ALPN
* request normalization
* H1/H2 server auto-detection
* runtime-neutral executor/timer boundaries
* comprehensive interoperability and performance tests
* zero Tokio
* sibling:

  * `../hyper-futures-lite`
  * `../h2-futures-lite`

The project is called:

```text
h12tiny
```

We now want to evolve it into a **modular practical HTTP stack** that can replace much of the `axum + reqwest` usage in:

```text
../smolvm
```

while preserving the original minimalist architecture.

The goal is **not** source compatibility with Axum or Reqwest.

The goal is:

> make normal application migration mechanical rather than architectural, while keeping protocol, application, and convenience layers sharply separated.

A normal handler should mostly require changed imports and minor syntax changes.

Exceptional facilities such as WebSockets, OCI authentication policy, OpenAPI generation, and application-specific middleware should remain visibly outside the core transport.

---

# 1. First restructure the workspace around hard dependency boundaries

Do not keep everything in one increasingly feature-heavy crate.

Split the repository into a workspace whose crate topology enforces minimalism.

Target approximately:

```text
h12tiny/
├── crates/
│   ├── h12tiny-core/
│   ├── h12tiny-client/
│   ├── h12tiny-server/
│   ├── h12tiny-util/
│   └── h12tiny-web/
└── h12tiny/                  # facade package, or root package as facade
```

Exact paths may follow the existing repository conventions.

The important boundaries are conceptual.

## `h12tiny-core`

Contains only genuinely shared low-level facilities, for example:

* `FuturesIo<T>`
* runtime-neutral timer adapter
* shared low-level errors/types
* possibly common TLS/connection metadata types where genuinely shared

It must not contain:

* pooling
* router
* client-only connector policy
* server accept loops
* JSON
* SSE
* application abstractions

Keep this crate extremely small.

## `h12tiny-client`

Contains:

* completed client
* legacy pool
* request normalization
* connector
* DNS/TCP
* optional TLS
* H1/H2 protocol selection
* connector/dialer abstraction
* connection-establishment timeout if implemented
* safe request lifecycle

It depends only on what a client requires.

## `h12tiny-server`

Contains:

* H1 connection serving
* H2 connection serving
* H1/H2 auto detection
* optional TLS/ALPN
* raw HTTP/1 upgrade support
* listener/connection lifecycle primitives
* generic accept-loop helpers
* graceful draining primitives

It must not depend on the client.

## `h12tiny-util`

Protocol-neutral application conveniences:

* body constructors
* bounded body collection
* text/bytes helpers
* optional JSON
* streaming body adapters
* replayable request/body primitives
* body idle-timeout wrapper
* tiny header helpers where useful

It should be usable by client and server applications independently.

It must not become a client implementation.

## `h12tiny-web`

Optional thin application adapter:

* router
* method dispatch
* path parameters
* extractors
* `IntoResponse`
* route body limits
* route deadlines
* optional JSON
* SSE
* optional minimal CORS
* state/extensions

It depends on server/util facilities as required.

It must not contain protocol implementation, connection pooling, DNS, TLS, WebSocket framing, OpenAPI, Tower, or a middleware framework.

---

# 2. Retain a top-level `h12tiny` facade crate

Users should have two choices:

### Precision users

Depend directly on the smallest component:

```toml
h12tiny-client = { version = "...", default-features = false, features = ["http1"] }
```

### Convenience users

Depend on the facade:

```toml
h12tiny = {
    version = "...",
    default-features = false,
    features = ["client", "http1"]
}
```

The facade should largely consist of conditional reexports.

Do not hide substantial implementation in the facade.

---

# 3. Feature topology must support every useful protocol/role combination

A user must be able to compile exactly:

```text
HTTP/1 client only
HTTP/2 client only
HTTP/1 + HTTP/2 client

HTTP/1 server only
HTTP/2 server only
HTTP/1 + HTTP/2 server

client + server H1
client + server H2
client + server H1 + H2

plaintext only
TLS-enabled variants

everything
```

without pulling unrelated roles/protocols.

## Client crate

Use something approximately like:

```toml
[features]
default = []

http1 = ["hyper/http1"]
http2 = ["hyper/http2"]
tls = [
    "dep:futures-rustls",
    "dep:rustls",
    "dep:webpki-roots",
]
```

Do not make H1 imply H2.

Do not make H2 imply H1 unless a demonstrated Hyper requirement forces it.

Most importantly:

> an H1-only client dependency graph must not contain `h2-futures-lite`.

## Server crate

Approximately:

```toml
[features]
default = []

http1 = ["hyper/http1"]
http2 = ["hyper/http2"]

tls = [
    "dep:futures-rustls",
    "dep:rustls",
]

upgrade = ["http1"]
```

Raw HTTP upgrade is inherently an HTTP/1 facility.

If enabling `upgrade` automatically enables `http1`, document that.

## Util crate

Approximately:

```toml
[features]
default = []

json = [
    "dep:serde",
    "dep:serde_json",
]

boxed-body = []
```

Do not make JSON mandatory.

## Web crate

Approximately:

```toml
[features]
default = []

json = [
    "h12tiny-util/json",
    "dep:serde",
]

query = [
    "dep:serde",
    "dep:serde_urlencoded",
]

sse = []

cors = []

upgrade = [
    "h12tiny-server/upgrade",
]
```

Adjust exact dependencies after implementation inspection.

Do not create features for every three-line helper.

Use features only where they meaningfully affect dependency graph or compiled surface.

---

# 4. Facade feature forwarding

The facade should provide ergonomic forwarding.

Conceptually:

```toml
[features]
default = []

client = ["dep:h12tiny-client"]
server = ["dep:h12tiny-server"]
util = ["dep:h12tiny-util"]
web = ["dep:h12tiny-web", "server", "util"]

http1 = [
    "h12tiny-client?/http1",
    "h12tiny-server?/http1",
]

http2 = [
    "h12tiny-client?/http2",
    "h12tiny-server?/http2",
]

tls = [
    "h12tiny-client?/tls",
    "h12tiny-server?/tls",
]

upgrade = [
    "server",
    "http1",
    "h12tiny-server/upgrade",
]

json = [
    "util",
    "h12tiny-util/json",
    "h12tiny-web?/json",
]

sse = [
    "web",
    "h12tiny-web/sse",
]

full = [
    "client",
    "server",
    "util",
    "web",
    "http1",
    "http2",
    "tls",
    "upgrade",
    "json",
    "sse",
]
```

Use Cargo's optional dependency feature forwarding cleanly.

Do not make `full` the default.

Prefer:

```toml
default = []
```

for the facade.

This is a deliberately explicit minimal stack.

---

# 5. Make feature isolation mechanically testable

Add:

```text
scripts/check-features.sh
```

or equivalent.

At minimum test direct component crates:

```sh
cargo check -p h12tiny-client --no-default-features --features http1
cargo check -p h12tiny-client --no-default-features --features http2
cargo check -p h12tiny-client --no-default-features --features http1,http2
cargo check -p h12tiny-client --no-default-features --features http1,tls
cargo check -p h12tiny-client --no-default-features --features http2,tls

cargo check -p h12tiny-server --no-default-features --features http1
cargo check -p h12tiny-server --no-default-features --features http2
cargo check -p h12tiny-server --no-default-features --features http1,http2
cargo check -p h12tiny-server --no-default-features --features http1,tls
cargo check -p h12tiny-server --no-default-features --features http2,tls

cargo check -p h12tiny --no-default-features --features client,http1
cargo check -p h12tiny --no-default-features --features client,http2
cargo check -p h12tiny --no-default-features --features server,http1
cargo check -p h12tiny --no-default-features --features server,http2
cargo check -p h12tiny --no-default-features --features client,server,http1,http2,tls
cargo check -p h12tiny --no-default-features --features full
```

Also inspect dependency graphs.

For H1 client only:

```sh
cargo tree \
    -p h12tiny-client \
    --no-default-features \
    --features http1 \
    -e normal
```

must contain:

```text
NO h2-futures
NO h12tiny-server
NO h12tiny-web
NO rustls unless TLS requested
NO serde
NO tokio
```

For H2 client only:

```text
NO HTTP/1-only application machinery
NO server
NO web
NO serde
NO tokio
```

Perform corresponding server checks.

If useful and already available, `cargo hack` may supplement this matrix.

Do not make an extra developer tool mandatory solely for CI.

---

# 6. Preserve the original h12tiny dependency contract

Across all normal production configurations:

```text
NO tokio
NO tokio-util
NO native-tls
NO reqwest
NO axum
NO hyper-util
NO tower
NO tower-layer
```

`h12tiny-web` must not quietly recreate Axum's Tower stack.

Continue using:

```text
../hyper-futures-lite
../h2-futures-lite
```

with protocol features forwarded precisely.

---

# 7. Add raw HTTP/1 upgrade support to `h12tiny-server`

This is the one protocol-level facility required for the `smolvm` PTY WebSocket migration.

Expose the raw HTTP upgrade lifecycle cleanly.

Requirements:

* detect/permit upgrade requests;
* allow application code to construct a `101 Switching Protocols` response;
* expose an `OnUpgrade`/`Upgraded`-equivalent future/stream;
* return the underlying upgraded bidirectional I/O;
* preserve cancellation/lifecycle correctness;
* work through the existing H1 server connection driver.

Do not implement WebSocket framing.

Do not add a WebSocket crate as a production dependency.

Expected layering:

```text
HTTP/1 request
    ↓
h12tiny-server upgrade
    ↓
raw upgraded stream
    ↓
application
    ↓
fastwebsockets-futures-lite
```

If `../fastwebsockets-futures-lite` exists, it may be used in an optional/dev interoperability test.

Do not make it part of the core dependency graph.

Do not claim HTTP/2 WebSocket/extended-CONNECT support.

---

# 8. Expose connector/dialer customization in `h12tiny-client`

The completed default connector remains:

```text
DNS
→ async-net TCP
→ optional rustls
→ ALPN
```

Add a clean low-level boundary that allows callers to replace connection establishment without replacing pooling or request normalization.

The goal is to support:

* tests
* Unix-socket-like transports
* custom address resolution
* peer-specific connections
* future Happy Eyeballs improvements

Do not add a giant connector ecosystem.

A small trait is enough.

Keep the default connector first-class and easy.

Do not add proxy support now.

---

# 9. TLS customization should stay Rustls-native

Do not create a Reqwest-like TLS API surface.

The existing ability to inject:

```rust
Arc<rustls::ClientConfig>
```

should be sufficient for client-side:

* custom CA roots
* client certificates
* mTLS
* ALPN customization where appropriate

Likewise expose/inject:

```rust
Arc<rustls::ServerConfig>
```

for server TLS.

That covers the important `smolvm` peer mTLS use case without adding:

```text
.client_certificate()
.add_root_certificate()
.danger_accept_invalid_certs()
```

style wrapper APIs.

Application code that requires custom trust should construct a Rustls config explicitly.

---

# 10. Add connection-establishment timeout only at the connector layer

A connect timeout is a meaningful transport primitive.

Implement it, if not already present, around:

```text
DNS/connect/TLS establishment
```

with an explicit contract.

Do not confuse this with response/body timeout.

Avoid giant timeout configuration structs.

Something roughly equivalent to:

```rust
Connector::builder()
    .connect_timeout(Duration::from_secs(5))
```

is enough.

Keep it runtime-neutral using existing timer facilities.

---

# 11. Add body idle timeout to `h12tiny-util`

Do **not** implement "socket read timeout" as the general HTTP response timeout.

That is incorrect for multiplexed H2 because unrelated streams share one TCP/TLS connection.

Instead create a protocol-neutral body wrapper:

```rust
IdleTimeoutBody<B>
```

which wraps:

```rust
B: http_body::Body
```

and resets an idle timer whenever the body yields a frame.

The timeout is therefore:

> maximum permitted inactivity for this logical HTTP body stream.

This works correctly for H1 and H2.

Make cancellation semantics explicit.

Do not poison unrelated H2 streams.

Possible API:

```rust
response
    .into_body()
    .with_idle_timeout(Duration::from_secs(30))
```

or equivalent extension trait.

Do not implement a vague all-purpose `.timeout()` API.

---

# 12. Build `h12tiny-util` around `http-body-util`, not instead of it

Do not invent a parallel body ecosystem.

Use/reexport the smallest useful `http-body-util` primitives.

Provide ergonomic constructors for:

```text
empty body
Bytes body
static bytes
streaming body
boxed/erased body when needed
```

The key practical issue is that an application using one pooled client often needs requests with different body implementations.

Provide an intentionally named erased convenience body, for example:

```rust
pub type BoxBody = ...;
```

or a tiny wrapper around the relevant `http-body-util` boxed body.

This belongs in `h12tiny-util`, not core.

Users who care about zero boxing can continue using concrete body types directly.

---

# 13. Add safe bounded response collection

Never make unbounded response buffering the easiest API.

Provide extension methods approximately like:

```rust
response.bytes_limited(max).await?
response.text_limited(max).await?
```

and under `json`:

```rust
response.json_limited::<T>(max).await?
```

Semantics:

* stream frames incrementally;
* enforce the cap before growing beyond it;
* return a clear limit error;
* do not trust `Content-Length` as the only enforcement mechanism;
* handle H1/H2 bodies uniformly;
* preserve trailers behavior sensibly;
* dropping after a limit violation must leave connection lifecycle correct.

A convenience unbounded collection method may exist only if clearly named as such.

Prefer bounded APIs in examples.

---

# 14. Add streaming helpers

Expose a simple way to turn an HTTP body into a data-byte stream suitable for:

* file downloads
* hashing
* OCI layer processing
* streaming consumers

For example:

```rust
response.into_data_stream()
```

or a thin reexport of the appropriate `http-body-util` mechanism.

Likewise make it easy to turn an application stream into a request body.

Support async file streaming without depending on Tokio FS.

Do not add a filesystem abstraction to h12tiny.

---

# 15. Add optional JSON conveniences

Under `h12tiny-util/json` only:

```text
serde
serde_json
```

Provide:

* JSON body creation
* `Content-Type: application/json`
* bounded response deserialization
* JSON response convenience for server-side use

Examples:

```rust
json_body(&value)?
response.json_limited::<T>(MAX).await?
json_response(&value)?
```

Do not put serialization into `h12tiny-core`, client, or server.

Do not require JSON for `h12tiny-web` unless its `json` feature is enabled.

---

# 16. Add a tiny Bearer header helper, but no auth framework

OCI and application APIs often need:

```text
Authorization: Bearer ...
```

A helper that safely constructs this header without extra dependencies is useful.

For example:

```rust
bearer(token) -> Result<HeaderValue, InvalidHeaderValue>
```

Do not implement:

* OAuth
* token refresh
* OCI auth policy
* authentication middleware
* credential storage

Do not add Basic auth unless a real consumer requires it.

---

# 17. Make replayability explicit

Retries are policy.

Replayability is transport/application capability.

Do not add generic automatic retries.

Provide primitives that make correct retry implementations straightforward.

Distinguish:

### Naturally replayable body

Examples:

```text
empty
Bytes
static JSON bytes
```

### Regeneratable streaming body

Example:

```text
open file
→ seek/start from known point
→ produce a fresh request body
```

### Non-replayable body

Example:

```text
one-shot arbitrary stream
```

Provide a small abstraction such as:

```rust
ReplayableRequest
```

and/or:

```rust
BodyFactory
RequestFactory
```

Do not over-generalize.

The intended OCI flow remains application controlled:

```text
construct replayable request
       ↓
send
       ↓
401 Bearer challenge
       ↓
application obtains token
       ↓
recreate request/body
       ↓
retry
```

h12tiny enables this.

h12tiny does not decide when to retry.

---

# 18. Build `h12tiny-web` as a tiny application substrate

This is the highest-leverage server migration facility.

Do not dismiss it as optional polish.

The existing `smolvm` API handlers mostly use a small vocabulary:

```text
State
Path
Query
Json
Extension
Bytes
raw Request
StatusCode
response tuples
Result<T, E>
SSE
one WebSocket endpoint
```

Support this vocabulary directly.

The test for success is:

> ordinary smolvm handlers should require mostly changed imports, not a wholesale rewrite around manually parsing `Request`.

---

# 19. Router scope

Implement a deliberately small:

```rust
Router<S>
```

with:

```text
route
nest
merge
with_state
fallback, if trivial
```

and method helpers:

```text
get
post
put
patch
delete
head
options
```

Use a small proven route matcher such as `matchit` unless inspection demonstrates that a smaller local matcher is clearly preferable.

A dependency such as `matchit` is acceptable **only in `h12tiny-web`**.

Do not expose routing from `h12tiny-server`.

Support:

```text
static paths
/{name}
/{*rest}
```

or the minimum syntax needed by smolvm plus obvious general use.

Do not build host routing, regex routing, route-ranking DSLs, or application framework machinery.

---

# 20. Minimal extractor model

Provide only the extraction model required for straightforward handlers.

Required:

```rust
State<T>
Path<T>
Query<T>
Json<T>
Extension<T>
Bytes
Request<_>
```

Prefer native Rust async traits / explicit futures.

Do not add `async-trait`.

A small internal equivalent of:

```text
FromRequestParts<S>
FromRequest<S>
```

is acceptable if it materially simplifies handler composition.

Keep these traits private or narrowly public until their stability is justified.

Do not attempt Axum API compatibility.

---

# 21. Path extraction

Support the actual shapes needed by normal APIs:

* `String`
* numeric scalar IDs
* tuples where useful
* small serde structs/maps if reasonable

If generic `Path<T>` needs a small custom serde deserializer over route parameter pairs, implement it carefully or port the minimal mature logic required.

Do not import half of Axum to get typed path extraction.

Keep parsing failures explicit and return a normal rejection response.

---

# 22. Query extraction

Put typed query deserialization behind the `query` feature.

A small dependency such as:

```text
serde_urlencoded
```

is acceptable **only in `h12tiny-web` with the feature enabled**.

Do not require query deserialization for users who only need raw HTTP.

Expose raw query access regardless.

---

# 23. Handler glue

Allow handler shapes approximately like:

```rust
async fn create(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<CreateRequest>,
) -> Result<Json<CreateResponse>, ApiError>
```

without forcing application code to manually implement `Service`.

A small set of generated `Handler` implementations for common arities is acceptable.

For example:

```text
0–8 extractors
```

is enough.

This is boring compile-time glue with high migration value.

Do not create:

* arbitrary middleware layers
* Tower compatibility
* handler reflection
* dependency injection framework
* procedural macro system

---

# 24. Add a small `IntoResponse`

Support the common response vocabulary:

```text
Response
StatusCode
Bytes
String
&'static str
Json<T>
(StatusCode, R)
(HeaderMap, R)
(StatusCode, HeaderMap, R)
Result<T, E> where T/E implement IntoResponse
```

Do not reproduce every Axum response combination.

Keep it sufficient for application APIs.

This should allow an application error like:

```rust
ApiError
```

to centralize:

```text
status
headers
JSON error body
```

without coupling that error type to Hyper internals.

---

# 25. Request extensions and shared state

Support:

```text
State<T>
Extension<T>
```

without building a DI framework.

State should be router-owned shared application data.

Extensions should use normal `http::Extensions`.

This allows application-side request IDs, trace IDs, auth context, etc. without h12tiny knowing what they mean.

Do not add a tracing subsystem.

---

# 26. Route-level body limits

Provide request body limits directly at the application routing layer.

Something approximately like:

```rust
route("/upload", post(upload))
    .body_limit(128 * 1024 * 1024)
```

or equivalent router configuration.

Requirements:

* enforce while streaming;
* do not buffer first and check afterward;
* do not trust only `Content-Length`;
* produce a clear 413 response;
* allow route-specific overrides;
* no global middleware framework required.

This directly replaces a common Tower/Axum body-limit use case.

---

# 27. Route-level deadlines

Provide a deliberately narrow per-route request deadline facility.

Something approximately like:

```rust
route("/ordinary", get(handler))
    .timeout(Duration::from_secs(30))
```

Important:

* this is an application handler deadline;
* it is not the same as connector timeout;
* it is not the same as body idle timeout;
* SSE and upgraded/WebSocket routes must be able to disable it;
* cancellation must correctly drop the handler/request future.

Do not create a generic timeout middleware system.

---

# 28. Add tiny SSE support

Under:

```text
h12tiny-web/sse
```

provide:

```rust
Event
Sse<S>
```

supporting standard fields:

```text
data
event
id
retry
```

and correct:

```text
\n\n
```

event framing.

Optionally support keepalive comments if trivial.

SSE must remain ordinary streaming HTTP.

Do not add:

* reconnection client
* event bus
* broadcast abstraction
* Tokio stream dependency

Use generic streams/bodies from the futures ecosystem.

---

# 29. Raw upgrades remain separate from WebSockets

Do not add:

```text
WebSocket
WebSocketUpgrade
WebSocketStream
```

to h12tiny.

Instead make the migration path explicit:

```text
h12tiny-web route
      ↓
raw Request / upgrade extraction
      ↓
h12tiny-server raw upgrade
      ↓
Upgraded IO
      ↓
fastwebsockets-futures-lite
```

If a tiny `HttpUpgrade` extractor that exposes the raw upgrade future makes this mechanical, add it under the `upgrade` feature.

It should know HTTP upgrade mechanics, not WebSocket framing.

---

# 30. Minimal CORS is acceptable, but keep it structural

`smolvm` currently benefits from CORS behavior.

Under an optional:

```text
cors
```

feature, a small router-level CORS policy is acceptable.

It may:

* recognize preflight OPTIONS;
* validate/allow configured origins;
* set allowed-method/header response fields;
* add normal CORS response headers.

Do not model this as Tower middleware.

Do not build a general middleware stack just to implement CORS.

If the implementation becomes large or policy-heavy, leave CORS application-side instead.

---

# 31. Add listener/lifecycle helpers to `h12tiny-server`

The low-level completed API already serves one connection.

Add a small, generic accept-loop/lifecycle layer.

It should support:

* TCP listeners
* TLS listeners
* Unix listeners on Unix where practical
* generic accepted stream types
* executor-owned connection tasks
* stop accepting on shutdown signal
* drain active connections
* configurable connection limit if simple
* propagation/logging hook for connection errors

Do not own process signals.

Accept a caller-provided shutdown future/token.

Conceptually:

```rust
serve(listener, service, executor)
    .shutdown_on(shutdown_future)
    .await
```

or an equivalently small API.

The implementation must have a clear task-lifecycle story:

* no detached immortal connections;
* shutdown stops accepting;
* existing connection drivers may drain;
* completion can be awaited.

Do not make this an application server framework.

---

# 32. Unix sockets should fall out naturally

On Unix, server lifecycle helpers should work with Unix listeners/streams where the underlying async-net ecosystem permits it.

For clients, do not bake URI-to-Unix-socket conventions into core.

A custom connector/dialer should make Unix-domain client transport possible when an application needs it.

Keep HTTP URI semantics distinct from transport selection.

---

# 33. OpenAPI remains application-side

Do not add OpenAPI generation or Swagger UI integration.

The migration should break the current framework coupling rather than recreate it.

Applications may:

* generate OpenAPI independently;
* serve the JSON using an ordinary route;
* serve Swagger UI static assets using ordinary responses.

No OpenAPI crate belongs in h12tiny.

---

# 34. Tracing remains application-side

Do not add request tracing infrastructure.

Provide enough low-level hooks that application code can wrap:

```text
router/service
connection accept
request extensions
```

and implement tracing itself.

Request extensions are sufficient for trace IDs and contextual data.

No OpenTelemetry.

No tracing middleware stack.

---

# 35. Explicitly do not add proxy support yet

Do not add:

```text
HTTP proxy
CONNECT proxy
SOCKS
system proxy discovery
NO_PROXY
proxy auth
```

There is no need to burden this phase with those facilities.

The connector abstraction should make future proxy support possible without rewriting the pool.

That is enough.

---

# 36. Explicitly do not add generic redirects

OCI upload/location behavior is application policy, not browser redirect policy.

Do not add:

```text
follow redirects automatically
redirect count
cross-origin redirect auth policy
```

to h12tiny during this phase.

Applications can inspect:

```text
Location
```

and decide.

Likewise do not add a URL-resolution subsystem solely for OCI.

---

# 37. smolvm is the compatibility target, not a dependency

Inspect:

```text
../smolvm
```

read-only.

Inventory current uses of:

```text
axum
reqwest
tower
tower-http
```

Classify each call site into:

```text
covered directly by h12tiny component
covered by small h12tiny-util/web helper
application-specific and should remain in smolvm
true missing primitive
```

Do not modify `../smolvm` in this task unless explicitly requested later.

Do not make any h12tiny crate depend on smolvm.

Use it as a real-world pressure test for API design.

---

# 38. smolvm client compatibility scenarios

Create h12tiny-local tests/fixtures that model these application patterns without importing smolvm.

## JSON APIs

Model:

```text
construct JSON request
send
bounded JSON response
status handling
```

## OCI Bearer retry

Model:

```text
request
→ 401
→ WWW-Authenticate
→ obtain simulated bearer token
→ recreate replayable request
→ resend
```

Verify h12tiny supplies the primitives but does not own the retry policy.

## Streaming download

Model:

```text
response body
→ data stream
→ incremental hash / sink
```

without buffering the full response.

## Streaming upload

Model:

```text
fresh body factory
→ request
→ retry requiring freshly recreated stream
```

Prove a consumed non-replayable stream is never implicitly retried.

## Bounded response

Return a body larger than the configured cap.

Verify:

* deterministic limit error;
* no over-allocation;
* subsequent client requests remain healthy.

## Peer mTLS

Use test certificates to prove:

```text
custom client rustls config
→ client certificate
→ custom CA
→ successful TLS request
```

No Reqwest/native-tls semantics.

---

# 39. smolvm server compatibility scenarios

Build fixture handlers using `h12tiny-web` that look structurally like normal smolvm handlers.

Exercise:

```rust
State(...)
Path(...)
Query(...)
Json(...)
Extension(...)
Bytes
Result<Json<_>, ApiError>
```

Also test:

* nested routes
* method dispatch
* 404
* 405 if supported
* body limit
* route deadline
* SSE
* raw upgrade
* shared state
* request extensions
* TLS
* H1
* H2

The objective is to prove a framework migration can remain mostly mechanical.

---

# 40. Interop test WebSocket upgrades without adding WebSockets to production

If the sibling:

```text
../fastwebsockets-futures-lite
```

exists and is usable, add a dev-only interoperability test:

```text
HTTP/1 request
→ h12tiny-web route
→ h12tiny raw upgrade
→ fastwebsockets-futures-lite
→ echo frame
```

This proves the composition boundary.

Do not move framing code into h12tiny.

If the sibling is unavailable, a raw upgraded byte echo test is sufficient for this phase.

---

# 41. Preserve protocol feature isolation through `h12tiny-web`

`h12tiny-web` must remain protocol agnostic.

The same router/service should be usable over:

```text
H1
H2
H1 + H2 auto
TLS H1
TLS H2
```

without router code knowing which protocol is active.

Do not condition application semantics on protocol version except where HTTP itself requires it, such as H1 upgrades.

This is an important architectural invariant.

---

# 42. Dependency budgets by crate

Treat each crate independently.

## `h12tiny-core`

Expected to be tiny.

No:

```text
serde
matchit
rustls unless genuinely shared
h2 unless protocol feature needs it indirectly
```

## `h12tiny-client`

No:

```text
server
router
serde
matchit
SSE
```

unless explicitly enabled through a convenience crate—which should normally not happen.

## `h12tiny-server`

No:

```text
client pool
DNS client connector
web router
serde
matchit
```

## `h12tiny-util`

No:

```text
DNS
TCP
TLS
router
server accept loop
```

JSON dependencies only under `json`.

## `h12tiny-web`

May contain optional:

```text
matchit
serde
serde_json
serde_urlencoded
```

only behind relevant features.

Still no:

```text
tokio
tower
axum
reqwest
native-tls
```

---

# 43. Add dependency-contract tests

Provide a script that asserts important absences.

For example:

```text
H1-only client:
    no h2-futures
    no rustls without tls
    no server
    no web
    no serde

H2-only client:
    h2-futures expected
    no server
    no web
    no serde

H1-only server:
    no h2-futures
    no client
    no connector
    no serde

H2-only server:
    h2-futures expected
    no client

util without json:
    no serde
    no serde_json

web without json/query:
    no serde_json
    no serde_urlencoded
```

Fail CI on regressions.

Dependency isolation is a product feature.

---

# 44. Track compile footprint

For representative configurations, record:

```text
normal dependency count
clean release build wall time, if practical
release example binary size
```

At minimum:

```text
H1 client plaintext
H1 client TLS
H2 client TLS
H1 server plaintext
H2 server TLS
full
```

Do not optimize benchmark numbers prematurely.

This is primarily a regression baseline proving modularity has real effect.

---

# 45. Keep public APIs orthogonal

Avoid one builder that knows everything.

Bad:

```rust
h12tiny::Builder
    .client(...)
    .server(...)
    .router(...)
    .tls(...)
    .json(...)
    .sse(...)
```

Prefer independent layers:

```rust
h12tiny_client::Client
h12tiny_server::Server/auto::Builder
h12tiny_web::Router
h12tiny_util::BodyExt
```

The facade merely reexports them.

This is critical.

The crate graph should mirror conceptual ownership.

---

# 46. Do not make convenience APIs contagious

An application using:

```text
h12tiny-web/json
```

may pull `serde_json`.

An application using only:

```text
h12tiny-client/http1
```

must not.

An application using:

```text
h12tiny-util
```

for bounded body collection must not acquire TLS, DNS, server, router, or JSON dependencies.

Every convenience feature should terminate at the appropriate layer.

---

# 47. Documentation examples must demonstrate modularity

README should show several independent configurations.

## Minimal H1 client

```toml
h12tiny = {
    version = "...",
    default-features = false,
    features = ["client", "http1"]
}
```

## H2 TLS client

```toml
h12tiny = {
    version = "...",
    default-features = false,
    features = ["client", "http2", "tls"]
}
```

## H1 + H2 TLS server

```toml
h12tiny = {
    version = "...",
    default-features = false,
    features = ["server", "http1", "http2", "tls"]
}
```

## Full application server

```toml
h12tiny = {
    version = "...",
    default-features = false,
    features = [
        "server",
        "http1",
        "http2",
        "tls",
        "web",
        "json",
        "sse",
        "upgrade",
    ]
}
```

Also show direct component-crate dependencies for users who want maximum graph precision.

---

# 48. Do not optimize for Axum API compatibility

Some names may intentionally look familiar:

```text
Router
State
Path
Query
Json
IntoResponse
```

because those are good HTTP application concepts.

But do not reproduce:

* Axum module hierarchy
* every rejection type
* every tuple implementation
* every extractor
* Tower layering
* middleware APIs
* extension ecosystem
* procedural macros

The objective is:

> mechanical application migration through a small common vocabulary.

Not:

> a clone of Axum.

---

# 49. Do not optimize for Reqwest API compatibility either

Do not build:

```rust
client
    .post(url)
    .bearer_auth(...)
    .json(...)
    .timeout(...)
    .send()
```

as the primary architecture.

The canonical transport API should remain built on:

```text
http::Request<B>
http::Response<B>
http_body::Body
```

`h12tiny-util` should make those types pleasant enough that Reqwest migration is concise.

Prefer composable primitives over a second request-builder universe.

---

# 50. Recommended implementation order

## Phase A — workspace split

Restructure completed h12tiny into:

```text
core
client
server
util
web
facade
```

without behavioral changes.

Run all existing tests after every move.

Acceptance:

* all original tests pass;
* no functionality lost;
* client and server no longer depend on each other;
* facade reproduces prior full functionality.

## Phase B — protocol/role feature isolation

Wire:

```text
client/http1
client/http2
server/http1
server/http2
tls
```

and facade forwarding.

Acceptance:

* all required combinations compile;
* H1-only graph contains no H2 crate;
* client-only graph contains no server;
* server-only graph contains no client;
* zero Tokio everywhere.

## Phase C — core migration primitives

Add:

```text
custom connector/dialer boundary
raw H1 upgrades
server lifecycle/accept-loop support
connect timeout
```

Acceptance:

* no framework/application features required;
* raw upgraded byte echo works;
* custom connector test works.

## Phase D — util

Add:

```text
body constructors
erased convenience body
bounded collection
text helpers
stream adapters
idle timeout body
JSON feature
Bearer helper
replayability primitives
```

Acceptance:

* no JSON deps without `json`;
* streaming remains streaming;
* bounded reads enforce limits safely.

## Phase E — web substrate

Implement:

```text
Router
methods
State
Path
Query
Json
Extension
Bytes
IntoResponse
body limits
route deadlines
```

Acceptance:

* representative smolvm-shaped handlers require no manual Request parsing;
* router remains protocol-neutral.

## Phase F — SSE and upgrade composition

Add:

```text
SSE
raw upgrade extractor/helper
```

Test upgraded raw I/O and optional fastwebsockets composition.

## Phase G — smolvm compatibility fixtures

Implement the client/server scenarios described above.

Do not modify smolvm yet.

## Phase H — dependency/size audit

Run all feature/dependency checks and amputate accidental cross-crate coupling.

---

# 51. Acceptance criteria

The extension is complete only when all of the following are true.

## Modularity

A user can independently select:

```text
H1 client
H2 client
H1+H2 client

H1 server
H2 server
H1+H2 server

any combination
```

with or without TLS where meaningful.

## Hard isolation

H1-only client:

```text
NO h2-futures
NO server
NO router
NO serde by default
NO Tokio
```

H1-only server:

```text
NO h2-futures
NO client pool
NO client connector
NO serde by default
NO Tokio
```

## Client application primitives

Available without Reqwest:

```text
bounded bytes
bounded text
optional bounded JSON
streaming download
streaming upload
body idle timeout
connect timeout
custom Rustls config
mTLS
Bearer header construction
explicit replayability
```

## Server application primitives

Available without Axum:

```text
Router
methods
State
Path
Query
Json
Extension
Bytes
IntoResponse
body limits
route deadlines
SSE
raw H1 upgrade
serve/drain lifecycle
```

## Explicit absences

Still no:

```text
Tokio
native-tls
Reqwest
Axum
Tower stack
proxy framework
redirect framework
WebSocket framing
OpenAPI
application tracing framework
HTTP/3
```

## Compatibility proof

Tests model:

```text
OCI Bearer retry
streaming OCI-style upload
streaming download
mTLS
bounded reads
Axum-shaped handlers
SSE
raw HTTP upgrade
H1
H2
TLS
Unix transport where applicable
timeouts
```

---

# 52. Final report

When finished, provide an engineering report with:

## Workspace graph

Show each crate and its responsibilities.

## Feature graph

Document:

```text
client
server
http1
http2
tls
util
web
json
query
sse
upgrade
full
```

and which component features they forward.

## Dependency evidence

For each representative minimal build, show:

```sh
cargo tree -e normal
```

or summarized equivalent.

Especially report whether `h2-futures` appears in H1-only builds.

## Size evidence

For representative configurations report:

```text
normal dependency count
release example binary size
```

## Migration coverage

Inventory `../smolvm` and classify remaining Axum/Reqwest usage after these APIs hypothetically exist:

```text
mechanical port
small application adapter
intentionally application-specific
still-uncovered primitive
```

The objective is to determine how close the resulting h12tiny surface gets to making removal of Axum/Reqwest boring.

## Novel LOC

Report production LOC by crate:

```text
h12tiny-core
h12tiny-client
h12tiny-server
h12tiny-util
h12tiny-web
facade
```

Call out how much new framework-like code exists specifically in `h12tiny-web`.

If that crate starts becoming large or conceptually broad, identify why before adding further features.

---

# Guiding principle

The completed transport layer was designed around:

> minimum novel correctness surface.

Preserve that principle here by adding **orthogonal layers rather than convenience creep**.

The desired final system is:

```text
                     application
                          │
               ┌──────────┴──────────┐
               │                     │
         h12tiny-web            h12tiny-util
        tiny app layer         body conveniences
               │                     │
               └──────────┬──────────┘
                          │
            ┌─────────────┴─────────────┐
            │                           │
     h12tiny-client              h12tiny-server
     pool / dial / TLS           conn / TLS / serve
            │                           │
            └─────────────┬─────────────┘
                          │
                    h12tiny-core
                          │
               hyper-futures-lite
                          │
                 h2-futures-lite
```

But an H1-only client user should see conceptually:

```text
application
    │
h12tiny-client [http1]
    │
h12tiny-core
    │
hyper-futures-lite [http1]
```

and **nothing else**.

That minimal graph is a first-class product requirement.

The stack should scale upward through composition:

```text
tiny H1 client
→ H1/H2 TLS client
→ H1/H2 server
→ practical application HTTP
```

without forcing lower-level users to pay for anything above the layer they selected.
