# Build a minimal futures-lite-native HTTP/1.1 + HTTP/2 client/server stack

Build a proof of concept called `h12tiny` for a **small, serious, pure-Rust HTTP endpoint layer** providing:

* HTTP/1.1 client
* HTTP/2 client
* HTTP/1.1 server
* HTTP/2 server
* per-origin client connection pooling
* DNS + TCP via `async-net`
* TLS via `rustls` + `futures-rustls`
* ALPN negotiation between `h2` and `http/1.1`
* correct request normalization
* automatic H1/H2 server protocol selection
* runtime-neutral executor/timer boundaries

The goal is effectively:

> **the async/futures-lite equivalent of ureq's practical transport layer, supporting both HTTP/1.1 and HTTP/2, client and server, with the minimum novel correctness surface possible.**

This is not an HTTP framework and not a Reqwest replacement.

The optimization target is not minimum source LOC.

It is:

> **minimum novel correctness surface, minimal dependency graph, and preservation of mature protocol/pooling semantics.**

---

## Existing local protocol stack

Use these sibling repositories directly:

```text
../h2-futures-lite
../hyper-futures-lite
```

Known state:

* `../h2-futures-lite` provides the no-Tokio H2 implementation.
* its package is currently `h2-futures`.
* `../hyper-futures-lite` is Hyper 1.11-derived.
* its package remains `hyper`.
* it already path-depends on `../h2-futures-lite`.
* it contains no Tokio dependency.
* it retains Hyper's runtime abstractions:

  * `hyper::rt::Read`
  * `hyper::rt::Write`
  * `hyper::rt::Executor`
  * `hyper::rt::Timer`

Do not re-research whether Tokio-free H1/H2 is possible. That substrate already exists.

Do not change either sibling unless an actual blocker is demonstrated.

---

# Architecture

Target:

```text
                   this crate
          ┌────────────┴────────────┐
          │                         │
        client                    server
          │                         │
 per-origin pooling          H1/H2 connection policy
          │                         │
 request normalization       cleartext sniff / TLS ALPN
          │                         │
 DNS → TCP → rustls/ALPN             │
          └────────────┬────────────┘
                       │
              hyper-futures-lite
                 ┌─────┴─────┐
                H1           H2
                              │
                   h2-futures-lite
```

The division of responsibility is deliberate:

### Hyper / h2 own

* HTTP/1 framing/parser/state machine
* HTTP/2 framing
* HPACK
* flow control
* streams
* GOAWAY
* SETTINGS
* request/response protocol machinery

### This crate owns

* origin routing
* connection establishment
* DNS
* TCP
* TLS
* ALPN
* per-origin pooling
* H1/H2 endpoint policy
* Host/request-target normalization
* connection lifecycle
* server auto protocol selection

Do not move protocol machinery upward.

---

# 1. Port/amputate `hyper-util`, do not reinvent its mature semantics

Use current `hyper-util` as the source/reference implementation, especially:

```text
hyper_util::client::legacy
hyper_util::client::legacy::pool
hyper_util::server::conn::auto
```

Do not depend on `hyper-util` at runtime.

Do not vendor the whole crate permanently.

Instead:

1. pin the exact upstream version/tag/commit used;
2. copy only the relevant implementation;
3. amputate runtime/transport/generalization we do not need;
4. port relevant tests alongside the implementation.

Use `hyper-util 0.1.20` as the baseline unless inspection shows a materially newer version should be used.

Create:

```text
UPSTREAM.md
```

recording:

* upstream repository
* exact tag/commit
* upstream source path
* local destination path
* whether code is:

  * substantially ported,
  * rewritten,
  * intentionally omitted

Example:

```text
src/client/pool.rs
  ← hyper-util/src/client/legacy/pool.rs @ <commit>
  status: substantially ported

src/connect.rs
  ← conceptual replacement for HttpConnector
  status: rewritten for async-net + futures-rustls
```

Preserve license/provenance notices where appropriate.

The purpose is to make future upstream correctness/security diffs tractable.

---

# 2. Preserve `client::legacy` semantics

Do not design a new connection pool.

The `hyper-util::client::legacy` pool contains mature handling of:

* H1 unique connection ownership
* H2 shared connection ownership
* waiters
* checkout races
* connection establishment races
* stale pooled H1 connections
* H2 connection sharing
* idle expiration
* connection cleanup
* safe retry behavior

Port those semantics closely enough that upstream fixes can still be compared mechanically.

## H1 invariant

H1 connections are uniquely reserved:

```text
origin
├── idle conn
├── idle conn
└── idle conn
```

One checked-out connection serves one active request exchange.

Do not invent new rules for deciding when an H1 connection becomes reusable.

**Preserve the legacy client's existing dispatch/pool lifecycle.**

Response body completion, EOF, upgrade state, cancellation, and connection-driver state interact subtly.

Do not simplify this to:

```text
response body finished → return socket
```

unless that is literally how the retained Hyper-util logic proves safety.

## H2 invariant

H2 connections are shared:

```text
origin
└── H2 connection
    ├── stream
    ├── stream
    ├── stream
    └── ...
```

Preserve the equivalent of:

```text
Reservation::Unique
Reservation::Shared
```

or the existing `Poolable` abstraction.

A shared H2 sender remains available for additional requests.

## H2 establishment invariant

When many requests concurrently hit an empty H2 origin pool:

```text
100 callers
    ↓
one logical H2 establishment owner
    ↓
one negotiated session
```

Do not permit a thundering herd of duplicate H2 sessions merely because all callers saw an empty pool.

Preserve the upstream "connecting" bookkeeping.

Dropping/failing the establishment owner must allow subsequent retries and must not strand waiters.

## H1 concurrency

Do not apply the H2 single-establishment rule blindly to H1.

Concurrent H1 requests may legitimately establish multiple connections.

---

# 3. Preserve request normalization

Port the mature `client::legacy` behavior around:

* absolute URI validation
* pool key derivation
* Host header synthesis
* origin-form H1 request targets
* CONNECT authority form
* explicit Host preservation
* default `/` path
* scheme/authority validation
* HTTP-version selection

The public client should accept:

```text
GET https://example.com/foo?x=1
```

and derive:

```text
pool key:
    scheme=https
    authority=example.com
```

while H1 emits:

```text
GET /foo?x=1 HTTP/1.1
Host: example.com
```

Do not add the `url` crate.

Use the existing `http`/Hyper URI types.

Test at minimum:

* root URI
* paths
* queries
* explicit ports
* default ports
* IPv4 literals
* IPv6 literals
* explicit Host header
* synthesized Host
* CONNECT
* missing scheme
* missing authority
* malformed URI forms

---

# 4. Rewrite the connector from scratch

Do not port Tokio `HttpConnector`.

Do not port Tokio's entire `socket2` machinery merely to establish sockets.
Platform support may use `libc`; direct and transitive `libc` are allowed by the
dependency policy.

Do not use `native-tls`.

The intended transport stack is:

```text
async-net
+
futures-rustls
+
rustls
+
webpki-roots
```

## HTTP

```text
URI
 ↓
resolve host
 ↓
async-net TcpStream
 ↓
Hyper
```

Default port 80.

## HTTPS

```text
URI
 ↓
resolve host
 ↓
async-net TcpStream
 ↓
futures-rustls TLS
 ↓
ALPN
 ↓
Hyper H1 or H2
```

Default port 443.

Configure:

```rust
config.alpn_protocols = vec![
    b"h2".to_vec(),
    b"http/1.1".to_vec(),
];
```

Interpret ALPN:

```text
"h2"       → H2
"http/1.1" → H1
none       → H1 unless explicitly configured H2-only
other      → error
```

Rustls handles:

* certificate verification
* SNI
* ALPN

No native platform TLS.

---

# 5. Keep connector metadata minimal

A successful connection fundamentally needs:

```text
transport
protocol capability / negotiated H2 flag
```

Retain additional metadata only when the ported legacy client actually requires it.

Delete abstractions existing solely for:

* system proxies
* SOCKS
* proxy authentication
* interface binding
* arbitrary socket metadata
* capture hooks
* system proxy discovery
* platform-specific proxy configuration

If retaining `tower-service` temporarily preserves the legacy connector contract cleanly, that tiny trait-only dependency is acceptable during the port.

Do not add:

```text
tower
tower-layer
```

If replacing `tower-service` with a tiny local trait later is completely mechanical and improves the result, do so only after correctness is established.

---

# 6. Implement one futures-I/O ↔ Hyper-I/O adapter

Create something like:

```rust
pub struct FuturesIo<T>(pub T);
```

adapting:

```text
futures_io::AsyncRead
futures_io::AsyncWrite
```

to:

```text
hyper::rt::Read
hyper::rt::Write
```

It must work with:

```text
async_net::TcpStream
futures_rustls::client::TlsStream<_>
futures_rustls::server::TlsStream<_>
```

Requirements:

* correct `ReadBufCursor` handling
* correct EOF semantics
* correct flush/shutdown
* vectored writes where available
* no unnecessary copying
* minimal unsafe
* safety comments for every unsafe block

Add dedicated adapter tests.

Add a focused:

```text
cargo +nightly miri test
```

target for this module or its relevant tests where practical.

The I/O bridge is one of the few areas where memory-safety-sensitive adapter code may exist; Miri coverage is disproportionately valuable.

---

# 7. Preserve runtime neutrality

Do not make this crate depend on a task runtime.

Use:

```text
hyper::rt::Executor
```

as the executor boundary.

The public client/server builders should remain generic over the executor required by Hyper.

Tests may use:

```text
smol
```

as a dev dependency.

That does not make `smol` part of the production stack.

Likewise implement a Hyper `Timer` using:

```text
async_io::Timer
```

for:

* pool idle expiration
* H2 timer hooks where necessary

Do not use Tokio time.

---

# 8. Be conservative with timeout semantics

Do not invent a sprawling timeout API during the POC.

Distinguish clearly between:

```text
pool idle timeout
connect timeout
request deadline
header timeout
body read timeout
body write timeout
```

These have different semantics.

For the initial implementation:

* preserve pool idle timeout behavior;
* a simple connection-establishment timeout is acceptable if clean;
* otherwise leave request deadlines to caller-side future racing.

Do not implement a generic "request timeout" that ambiguously terminates streaming bodies.

Cancellation caused by timeout must obey the same lifecycle invariants as ordinary future cancellation.

---

# 9. Cancellation safety is a hard correctness requirement

Endpoint-layer futures must remain cancellation-safe wherever callers may reasonably drop them.

In particular:

> Do not destructively remove the only resumable state from a connection/session before an `.await` unless cancellation intentionally invalidates that connection/session.

Audit state machines for patterns equivalent to:

```rust
let state = self.state.take().unwrap();
some_future.await?;
```

where dropping during the await leaves `self` irreparably empty.

Exercise cancellation during:

* pool wait
* connection establishment
* TLS handshake
* Hyper handshake
* request dispatch
* response headers
* response body streaming

Verify afterward:

* no stranded waiter
* no permanently poisoned pool entry
* unrelated H2 streams survive
* later requests succeed
* cancelled H1 sessions are not incorrectly reused

---

# 10. Server support: port `server::conn::auto`

Support:

```text
plaintext HTTP/1.1
plaintext H2 prior knowledge
TLS HTTP/1.1
TLS HTTP/2
```

Explicitly **do not implement HTTP/1.1 Upgrade: h2c** in the POC.

"h2c support" here means:

> HTTP/2 prior knowledge over plaintext.

## Plaintext detection

Recognize:

```text
PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n
```

Port the mature progressive-detection behavior:

1. start matching the H2 preface;
2. read incrementally;
3. as soon as bytes cannot match H2, choose H1;
4. preserve all bytes already consumed;
5. replay them into the H1 parser;
6. if all 24 bytes match, use H2.

Do not wait unnecessarily for all 24 bytes for obvious H1 requests.

Do not lose or reorder sniffed bytes.

## TLS server

Use:

```text
async-net TcpListener
 ↓
futures-rustls TlsAcceptor
 ↓
ALPN
```

Advertise:

```text
h2
http/1.1
```

Dispatch:

```text
h2        → H2
http/1.1  → H1
no ALPN   → H1
unexpected → error
```

---

# 11. Keep server scope deliberately low-level

Expose something approximately like:

```rust
auto::Builder::new(executor)
    .serve_connection(io, service)
    .await
```

Do not add:

* router
* extractors
* middleware framework
* cookies
* sessions
* JSON helpers
* application-level graceful-shutdown framework
* WebSockets API
* compression

This crate owns connection policy, not application policy.

---

# 12. Pool key contract

For the POC, pool identity is:

```text
scheme + authority
```

with canonical/default-port handling matching the ported legacy client.

Do not implement HTTP/2 origin coalescing.

That means:

```text
https://a.example
https://b.example
```

do not share an H2 session even if:

* DNS resolves identically,
* certificates authorize both,
* RFC rules might permit it.

Cross-origin coalescing is deferred.

Likewise one `Client` owns one TLS policy; do not complicate pool keys with arbitrary connector identity until a real use case exists.

---

# 13. Happy Eyeballs is separable

Use `async-net`'s normal resolution/connect machinery first.

Required:

* multi-address resolution works correctly
* IPv4 works
* IPv6 works

Do not block the POC on reproducing all of Tokio `HttpConnector`'s address racing/socket tuning.

If measurements show the connector needs stronger RFC 8305 behavior, add a small Happy Eyeballs implementation later behind the connector boundary.

Keep that possible without touching pool semantics.

---

# 14. Feature design

Use a small feature matrix:

```toml
[features]
default = ["client", "server", "http1", "http2", "tls"]

client = [...]
server = [...]
http1 = ["hyper/http1"]
http2 = ["hyper/http2"]
tls = [...]
```

Do not over-feature the crate.

Verify at least:

```sh
cargo check --no-default-features --features client,http1
cargo check --no-default-features --features client,http2
cargo check --no-default-features --features client,http1,http2

cargo check --no-default-features --features server,http1
cargo check --no-default-features --features server,http2
cargo check --no-default-features --features server,http1,http2

cargo check --no-default-features --features client,http1,http2,tls
cargo check --no-default-features --features server,http1,http2,tls

cargo check --all-features
```

H1-only builds must not pull H2 machinery unnecessarily.

Client-only builds must not pull server-only machinery unnecessarily.

---

# 15. Dependency budget

Expected normal dependency family:

```text
../hyper-futures-lite
    └── ../h2-futures-lite

async-net
async-io
futures-lite
futures-io
futures-rustls
rustls
webpki-roots

bytes
http
http-body

possibly:
futures-channel
pin-project-lite
tower-service
```

Some may already be transitively supplied by Hyper; depend directly only when used directly.

Forbidden production dependencies:

```text
tokio
tokio-util
native-tls
hyper-util
reqwest
tower
tower-layer
axum
async-trait
url
serde
serde_json
mime
cookie
compression stacks
socket2
system proxy crates
```

unless a genuinely unavoidable requirement is demonstrated.

`libc` is allowed directly or transitively.

Most importantly:

> **zero Tokio in the normal production graph.**

Verify mechanically:

```sh
cargo tree -e normal
cargo tree -i tokio
cargo tree -d
```

Also provide a small CI-friendly dependency check that rejects forbidden packages from the enabled normal graph.

Do not simply grep `Cargo.lock`; dev/optional dependencies can legitimately appear there.

---

# 16. TLS fixtures

Prefer committed DER fixtures:

```text
tests/fixtures/tls/cert.der
tests/fixtures/tls/key.der
```

with a localhost SAN.

Tests should build an explicit client root store trusting the fixture certificate.

Avoid adding certificate-generation dependencies such as `rcgen` unless dynamically generated certificates are genuinely needed.

Avoid `rustls-pemfile` if DER fixtures suffice.

---

# 17. Dogfood integration suite

Our client and server should heavily test each other.

Required normal test matrix:

```text
our client H1  → our server H1
our client H1  → our TLS server ALPN H1
our client H2  → our TLS server ALPN H2
our client H2  → our plaintext H2 prior-knowledge server
```

Test:

* GET
* HEAD
* POST
* empty body
* small body
* large body
* streaming request
* streaming response
* Content-Length
* H1 keep-alive
* H1 server close
* H1 stale pooled socket
* Host synthesis
* explicit Host
* URI normalization
* H2 multiplexing
* H2 cancellation isolation
* reconnect after closed H2 session
* GOAWAY behavior where practical

---

# 18. Instrument connection behavior explicitly

Do not infer pooling from successful requests.

Use test-only counters.

Track:

```text
TCP connections
TLS handshakes
H1 connections
H2 sessions
logical requests
```

Required proofs:

## Sequential H1

Many requests should reuse established connections.

## Concurrent H1

Concurrent requests may establish multiple connections, which should subsequently become reusable where safe.

## Concurrent H2

For e.g.:

```text
100 concurrent requests
```

expect approximately:

```text
1 TCP connection
1 TLS handshake
1 H2 session
100 streams
```

unless peer limits legitimately dictate otherwise.

## Empty-pool H2 race

Launch:

```text
100 simultaneous first requests
```

against a fresh origin.

Verify there is no H2 establishment stampede.

Repeat enough times to make race bugs visible.

---

# 19. Raw-wire H1 tests

Use a tiny raw TCP peer to assert exact request bytes.

Verify:

```text
GET /foo?x=1 HTTP/1.1
Host: example.test
```

rather than absolute-form for ordinary direct-origin requests.

Cover:

* root
* query
* Host insertion
* explicit Host
* explicit port
* IPv6
* CONNECT authority-form
* malformed URI rejection

These tests directly prove the endpoint layer rather than merely dogfooding Hyper against itself.

---

# 20. Adversarial HTTP/1 corpus

Create a compact raw-wire regression suite for framing/parser ambiguity.

Exercise at least:

```text
duplicate Content-Length
conflicting Content-Length
Content-Length + Transfer-Encoding
invalid Transfer-Encoding ordering
malformed chunk size
truncated chunks
bare LF
whitespace before header colon
invalid header names
premature EOF
ambiguous framing
extra bytes after framed body
```

Expected behavior should preserve Hyper's safe parser behavior.

The purpose is not to reimplement HTTP Garden.

The purpose is to ensure the endpoint layer never weakens Hyper's existing HTTP/1 correctness/security properties.

---

# 21. Port tests together with ported code

For relevant upstream `hyper-util` tests, maintain a small inventory:

```text
ported
not applicable
deferred
```

Do not silently drop tests.

Especially preserve evidence around:

```text
H1 unique reservation
H2 shared reservation
H2 connecting marker
waiter cancellation
idle timeout
max idle per host
closed connection removal
stale H1 retry
Host synthesis
absolute URI checks
request target normalization
server H1/H2 sniff + rewind
```

The core rule is:

> **subtract dependencies without subtracting evidence.**

---

# 22. External interoperability harness

These tools are already installed:

```text
curl
nghttp
nghttpd
h2load
oha
```

Do not install replacements.

Create simple scripts, e.g.:

```text
scripts/interop.sh
scripts/bench.sh
```

External tools are not required for `cargo test`.

## External client → our server

Verify:

```text
curl --http1.1 → our server
curl --http2   → our TLS server
nghttp         → our H2 server
```

Check:

* response body
* HTTP version
* successful TLS negotiation

## Our client → independent server

Use:

```text
our H2 client → nghttpd
```

for independent H2 interoperability.

If Go is already installed, optionally include a tiny `net/http` fixture for independent H1/H2 testing:

```text
tests/interop/go-server/
```

Do not make Go mandatory.

---

# 23. `h2load` remains part of the performance harness

Use `h2load` specifically for deep H2 load testing.

Use `oha` for a uniform H1/H2 comparison.

Do not treat `h2spec`, if available, as the sole modern HTTP/2 correctness oracle.

If installed, it can remain a useful extra regression suite.

The primary correctness chain is:

```text
h2-futures-lite mature protocol implementation
+
dogfood tests
+
nghttp/nghttpd interoperability
+
h2load stress
```

---

# 24. Performance methodology

Benchmark only after correctness passes.

Pin conditions:

* same Rust toolchain
* `--release`
* same compiler flags
* same machine
* same TLS certificate
* same rustls crypto provider
* tracing disabled
* same body sizes
* same connection counts
* same concurrency
* same HTTP protocol
* loopback unless specifically testing network effects

Do not interpret results from mismatched connection behavior.

Before calling a benchmark valid, verify:

```text
HTTP version
request count
error count
TCP connection count
TLS handshake count
body correctness
```

---

# 25. Server benchmarks

Expose deterministic endpoints returning:

```text
0 B
1 KiB
64 KiB
```

with essentially no application work.

## H1

Use `oha`, forced HTTP/1.1.

Sweep roughly:

```text
connections:
1
16
64
```

Capture:

```text
requests/sec
p50
p95
p99
errors
```

## H2

Use `oha` forced H2 for generic comparison.

Use `h2load` for specialist H2 measurements.

Vary:

```text
connections
concurrent streams
request count / duration
```

Include at least one workload that does not simply run with giant effectively-unconstrained flow-control windows.

---

# 26. Client benchmarks

Write a small internal load binary for this client.

Do not use Criterion unless it genuinely helps.

Measure:

```text
requests/sec
elapsed
errors
TCP connections
TLS handshakes
```

For H2 additionally verify:

```text
H2 sessions
concurrent logical streams
```

Workloads:

## H1

* cold
* warm sequential keepalive
* 16 concurrent
* 64 concurrent
* small body
* streaming/larger body

## H2

* cold
* warm
* 1 stream
* 16 streams
* 100 streams
* small body
* larger/streaming body

Use an independent local reference server where practical.

---

# 27. Optional upstream comparison

The scientifically useful comparison is:

```text
our futures-lite stack
vs
upstream Hyper + hyper-util + Tokio
```

because protocol lineage remains similar while runtime/endpoint machinery differs.

Do not contaminate this crate's dependency graph with Tokio to do it.

If implemented, put the upstream comparison in:

```text
bench/reference-upstream/
```

or another excluded/non-workspace project.

Document how to run it.

This comparison is optional after the POC succeeds.

---

# 28. Error model

Expose useful distinctions for:

```text
invalid URI
unsupported scheme
DNS/connect failure
TLS failure
ALPN failure
Hyper handshake failure
request dispatch failure
pool cancellation/connection loss
```

Preserve underlying causes via `Error::source()` where practical.

Do not add `anyhow` or `thiserror` merely for library convenience.

---

# 29. Debuggability

During development it should be possible to inspect:

```text
connection opened
connection pooled
pool checkout
pool return
pool eviction
ALPN result
H1/H2 selection
stale retry
connection close
```

Avoid making a logging framework mandatory.

If upstream `tracing` integration survives initially, feature-gate it and reassess after correctness.

---

# 30. Explicit non-goals

Do not add:

```text
HTTP/3
QUIC
HTTP/1.1 Upgrade: h2c
H2 cross-origin connection coalescing
WebSocket convenience API
proxies
SOCKS
system proxy discovery
cookies
redirects
compression
automatic decompression
JSON
multipart
forms
authentication helpers
DNS cache framework
custom resolver ecosystem
Alt-Svc
HSTS
certificate pinning
metrics framework
OpenTelemetry
Tower middleware stack
application router/framework
generic request timeout semantics
```

These can be layered later.

The POC proves the transport architecture first.

---

# 31. Recommended implementation order

## Phase A — substrate

* scaffold crate
* path dependencies
* features
* `FuturesIo`
* executor test adapter
* `FuturesTimer`
* Miri coverage for adapter

Acceptance:

```text
H1/H2 Hyper connection can run over futures I/O
zero Tokio
```

## Phase B — legacy pool

Port the pool before networking.

Get tests passing for:

```text
H1 unique reservation
H2 shared reservation
waiters
connecting marker
cancellation
idle expiration
closed-connection cleanup
```

Acceptance:

> mature pooling semantics work independently of async-net/rustls.

## Phase C — legacy Client

Port:

```text
Client
Builder
request dispatch
normalization
safe retry
```

Keep connector generic initially.

Acceptance:

> client semantics work against a controlled test connector.

## Phase D — async-net + rustls connector

Implement:

```text
HTTP → TCP
HTTPS → TCP → TLS → ALPN
```

Acceptance:

```text
H1 HTTPS
H2 HTTPS
SNI
certificate validation
ALPN
```

## Phase E — server auto

Port:

```text
preface detection
rewind buffer
H1/H2 dispatch
TLS ALPN dispatch
```

Acceptance:

```text
plaintext H1
plaintext H2 prior knowledge
TLS H1
TLS H2
```

## Phase F — dogfood + lifecycle

Add:

```text
integration matrix
pool counters
cancellation stress
first-request race
stale H1 tests
raw-wire normalization
adversarial H1 corpus
```

## Phase G — external interop

Verify:

```text
curl H1
curl H2
nghttp H2
our client → nghttpd
```

## Phase H — dependency amputation

Only now aggressively remove any remaining transitional abstractions.

Re-run all upstream-derived tests after each removal.

## Phase I — performance

Run:

```text
oha H1
oha H2
h2load H2
internal client benchmark
```

Do not optimize before this phase unless profiling exposes an obvious regression.

---

# 32. Acceptance criteria

The POC is complete only when:

## Protocols

* H1 client works
* H2 client works
* H1 server works
* H2 server works
* plaintext H1 works
* plaintext H2 prior knowledge works
* TLS H1 works
* TLS H2 works

## TLS

* rustls only
* correct certificate verification
* SNI works
* ALPN selects H1/H2 correctly

## Pool

* sequential H1 reuse proven by counters
* concurrent H1 lifecycle correct
* concurrent H2 multiplexing proven
* empty-pool H2 race does not stampede
* stale H1 connections handled safely
* waiter cancellation does not poison pool
* later requests succeed after cancellation/failure
* H2 closed/GOAWAY session can be replaced correctly

## Request semantics

* absolute URI routing works
* Host insertion works
* explicit Host works
* H1 request-target forms are correct
* CONNECT form is correct
* malformed unsupported URI forms fail clearly

## Server detection

* H1 divergence detected progressively
* sniffed bytes replay exactly
* H2 preface recognized
* no `Upgrade: h2c` claim

## Dependencies

Normal graph contains:

```text
NO tokio
NO tokio-util
NO native-tls
NO hyper-util
NO reqwest
NO tower
NO tower-layer
NO socket2
```

`libc` may remain in the normal graph, directly or transitively.

`tower-service` only if deliberately retained and justified.

## Interoperability

Pass:

```text
curl H1 → us
curl H2 → us
nghttp → us
us → nghttpd
```

## Evidence

For every meaningful retained upstream behavior, tests exist or the omission is explicitly classified.

---

# 33. Final engineering report

When finished, report:

## Architecture

What was ported versus newly written.

## Upstream provenance

Exact `hyper-util` tag/commit.

List retained modules.

## Amputations

What was removed and why.

## Dependency graph

Include:

```sh
cargo tree -e normal
```

Explicitly state whether these remain:

```text
tokio
native-tls
hyper-util
tower
tower-service
socket2
```

`libc` is allowed directly or transitively; state its observed presence and
ownership in the report.

## Correctness tests

Summarize:

```text
ported upstream tests
dogfood tests
raw-wire tests
adversarial H1 tests
cancellation tests
pool stress tests
Miri tests
```

## Interoperability

Results for:

```text
curl H1
curl H2
nghttp
our client → nghttpd
```

## Pool evidence

Report actual observed counts for:

```text
N sequential H1 requests
N concurrent H1 requests
100 concurrent H2 requests
100 simultaneous first H2 requests
```

including:

```text
TCP connections
TLS handshakes
H2 sessions
```

## Performance

Representative:

```text
oha H1
oha H2
h2load H2
client benchmark
```

Do not overinterpret minor differences.

## Code size

Report:

```text
total production LOC
ported production LOC
newly written production LOC
normal dependency count
minimal release client binary size
minimal release server binary size
```

The **ported LOC vs novel LOC** distinction matters.

## Remaining gaps

Only concrete issues, such as:

```text
Happy Eyeballs quality
timeout policy
specific lifecycle edge cases
upstream divergence
performance regressions
```

---

# Final engineering principle

This project should answer:

> **Once Hyper and h2 are runtime-neutral, how little additional machinery is actually necessary to provide a correct, practical HTTP/1.1 + HTTP/2 client and server?**

The expected answer is a small missing endpoint layer:

```text
async-net
+
rustls
+
mature hyper-util legacy pooling semantics
+
tiny H1/H2 endpoint policy
+
hyper-futures-lite
+
h2-futures-lite
```

Do not optimize for cleverness.

Do not rewrite mature state machines merely to reduce LOC.

Preserve mature correctness.

Rewrite only the pieces fundamentally tied to Tokio or unnecessary generality.

Optimize relentlessly for:

```text
minimal novel correctness surface
small dependency graph
runtime neutrality
explicit ownership
explicit state transitions
cancellation safety
interoperability
measurable pooling behavior
future upstream diffability
maintainability
```
