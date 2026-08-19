# Local engineering audit report

Audit date: 2026-08-19. Scope: the `h12tiny` working tree, `plan.md`, the
implementation and tests present at audit time, the two local Hyper siblings,
and the repository's verification scripts. This report records evidence; it is
not a completion claim. The audit changed documentation only.

## Current shape

The endpoint layer is split as the plan describes:

| Boundary | Current implementation | Evidence |
| --- | --- | --- |
| Protocol machinery | Path dependency on `../hyper-futures-lite` (`hyper` 1.11.0), which path-depends on `../h2-futures-lite` | `Cargo.toml`, `cargo tree -e normal` |
| Futures I/O | `src/io.rs::FuturesIo` | 3 focused unit tests and `sh scripts/miri-io.sh` |
| Runtime boundary | `src/runtime.rs::BoxExecutor` and `AsyncIoTimer` | 3 focused unit tests; no task-runtime production dependency |
| Client routing/normalization | `src/client/normalize.rs`, `src/client/mod.rs` | unit, raw-wire, dogfood, and TLS tests |
| Pooling | `src/client/pool.rs` | H1/H2 reservation, marker, waiter, expiry, closed-value, and max-idle unit tests |
| Transport | `src/client/connect.rs` | `async-net` TCP; Rustls/futures-rustls TLS and ALPN tests |
| Server selection | `src/server/conn/auto.rs` | progressive preface/replay unit tests, H1/H2 dogfood, and TLS ALPN tests |

The port/rewrite/omission inventory and the exact Hyper-util baseline are in
[`UPSTREAM.md`](UPSTREAM.md). The local code does not vendor all of
`hyper-util`, and no claim is made here that its complete test suite was ported.

## Feature matrix

The declared default is `client,server,http1,http2,tls`. All of the following
feature checks exited successfully during this audit:

| Check | Result |
| --- | --- |
| `cargo check --no-default-features --features client,http1` | pass |
| `cargo check --no-default-features --features client,http2` | pass |
| `cargo check --no-default-features --features client,http1,http2` | pass |
| `cargo check --no-default-features --features server,http1` | pass |
| `cargo check --no-default-features --features server,http2` | pass |
| `cargo check --no-default-features --features server,http1,http2` | pass |
| `cargo check --no-default-features --features client,http1,http2,tls` | pass |
| `cargo check --no-default-features --features server,http1,http2,tls` | pass |
| `cargo check --all-features` | pass |

The normal tree for `client,http1` contains no `h2-futures`; the `client,http2`
tree does. This is evidence that the protocol feature gates are effective for
those two builds. These are compile checks, not a claim that every feature
combination has a complete integration-test run.

Behavior currently demonstrated by local tests:

| Capability | Local evidence | Status |
| --- | --- | --- |
| Direct H1 client/server | `tests/dogfood_h1.rs`, `tests/dogfood_h1_matrix.rs` | verified |
| H1 origin-form, Host, root/path/query, ports, IPv4/IPv6, CONNECT | `src/client/normalize.rs`, `tests/raw_h1.rs` | verified for the covered cases |
| Plaintext H2 prior knowledge | `tests/dogfood_h2_prior.rs` | verified |
| TLS H1, fixture validation, streaming body | `tests/dogfood_tls.rs::tls_alpn_http11_validates_fixture_certificate_and_streams_bodies` | verified |
| TLS H2, ALPN, 100 first requests | `tests/dogfood_tls.rs::concurrent_first_tls_h2_requests_share_one_handshake_and_session` | verified for this workload |
| H1 sequential reuse and peer-close reconnect | `tests/raw_h1.rs` | verified for these cases |
| H2 shared reservation and first-request convergence | pool unit tests; cleartext 16-request and TLS 100-request dogfood | verified for these workloads |
| H1 cancellation closes its affected session | `tests/raw_h1.rs::cancelling_h1_request_closes_that_session_before_a_later_request` | verified |
| H2 cancellation isolation and H2 close/reconnect | `tests/h2_lifecycle.rs` | verified; 2 tests pass |
| Concurrent H1 establishment and warm reuse | `tests/dogfood_h1_matrix.rs::concurrent_h1_requests_get_unique_connections_then_reuse_them` | verified for 16 concurrent requests |
| Adversarial H1 framing | `tests/adversarial_h1.rs` | verified for the listed corpus |
| HTTP/1.1 Upgrade-to-h2c | none | intentionally omitted |
| Independent external interoperability | `scripts/interop.sh`; reported loopback probes with curl, nghttp, and the h12tiny client | verified at tool level for the recorded endpoints |
| Performance/load harness | `scripts/bench.sh`; reported loopback oha and h2load runs | harness completed; no numeric results recorded |

## TLS fixtures

`tests/fixtures/tls/cert.der` and `tests/fixtures/tls/key.der` are present. The
certificate is DER, self-signed for the local test harness, and has `localhost`
DNS and `127.0.0.1` IP subject alternative names. Tests construct an explicit
client root store from that certificate, so the tests do not rely on a machine
trust store.

At audit time `git status --short` reports `?? tests/fixtures/`. The fixture
files are therefore present in the working tree but are not yet tracked; do not
describe them as committed until they are added to the repository.

## Dependency budget

The default normal graph has 15 direct dependencies:

`async-io`, `async-net`, `bytes`, `futures-channel`, `futures-io`,
`futures-lite`, `futures-rustls`, `futures-util`, `http`, `http-body`, `hyper`,
`pin-project-lite`, `rustls`, and `webpki-roots`.

The current `cargo tree -e normal --prefix none` contains 58 unique package
names including `h12tiny` (57 dependency package names). `cargo tree -d`
reports the `webpki-roots` 0.26.11 → 1.0.9 duplicate. The duplicate is a
transitive-version detail to revisit if dependency minimization becomes a
priority.

`sh scripts/check-normal-dependencies.sh` passes and reports no forbidden
normal package. `cargo tree -i tokio` exits with Cargo's “package ID
specification `tokio` did not match any packages” result, which is the expected
absence check. `smol` remains a dev dependency only.

There is an important qualification to the plan's dependency wording: the
normal graph does contain transitive `libc` through platform/crypto support,
while `h12tiny` has no direct `libc` dependency. The repository check script
explicitly allows that transitive platform edge and rejects a direct `libc`
dependency. Thus the current implementation satisfies the script's direct-
dependency rule, not a literal interpretation of “NO libc” for every normal
transitive package.

## Verification record

The following focused commands passed:

| Command | Result |
| --- | --- |
| `cargo test --all-features` | 25 library tests + 19 integration tests passed |
| `cargo test --all-features --lib` | 25 passed |
| `cargo test --all-features --test raw_h1` | 7 passed |
| `cargo test --all-features --test dogfood_h1` | 1 passed |
| `cargo test --all-features --test dogfood_h1_matrix` | 2 passed |
| `cargo test --all-features --test dogfood_h2_prior` | 2 passed |
| `cargo test --all-features --test dogfood_tls` | 4 passed |
| `cargo test --all-features --test h2_lifecycle` | 2 passed |
| `cargo test --all-features --test adversarial_h1` | 1 passed |
| `sh scripts/check-normal-dependencies.sh` | pass; transitive platform `libc` explicitly allowed |
| `sh scripts/miri-io.sh` | 3 focused `io::tests` passed; integration targets were filtered to 0 |

The parent agent's final `cargo test --all-features` run passed with 25 library
tests and 19 integration tests. The focused H2 lifecycle target now contributes
2 passing tests; the earlier `!Unpin` selection-harness failure is resolved.

The Miri command also emitted two deprecation warnings from the sibling
`h2-futures` crate; its three focused adapter tests passed. No formatter,
linter, or pre-commit hook was run.

## Pool and lifecycle evidence

Counters in `tests/support/mod.rs` are test-only server-boundary observations;
they are not production metrics or inferred request-success counts.

The strongest recorded snapshots are:

| Workload | TCP | TLS handshakes | H1 connections | H2 sessions | logical requests |
| --- | ---: | ---: | ---: | ---: | ---: |
| H1 dogfood matrix: 5 sequential requests | 1 | 0 | 1 | 0 | 5 |
| H1 dogfood matrix: 16 concurrent, then 16 warm requests | 16 | 0 | 16 | 0 | 32 |
| TLS H1 streaming request | 1 | 1 | 1 | 0 | 1 |
| TLS H2: 100 simultaneous first requests | 1 | 1 | 0 | 1 | 100 |

The cleartext H2 first-request test accepts exactly one peer socket and serves
16 requests; it therefore exercises the single-establishment path but does not
provide a full connection-counter snapshot. Raw H1 sequential reuse similarly
uses one accepted peer socket for two requests. Concurrent H1 establishment is
allowed by the design and is covered by the 16-connection counter workload.

The pool unit tests directly exercise unique/shared reservations, H2 marker
release, dropped waiters, max-idle capping, closed-value removal, and checkout
expiry; the background timer eviction test also passes without a later
checkout. A failed network connector with multiple waiters and a stale-but-idle
H1 socket race remain less directly evidenced.

## Debugability

The client has an opt-in, dependency-free event surface:

- `client::DebugEventLog`, passed to `Builder::debug_event_log`, is a
  pull-based log so recording never invokes application code while a pool mutex
  is held. Its `drain` method exposes checkout, connection establishment, TLS
  ALPN selection, pool return/eviction, close, and stale-retry events.
- `tests/raw_h1.rs` attaches a log to a real H1 client and verifies checkout,
  connection establishment, and pool-return observations.
- `tests/support::ConnectionCounters` records TCP opens, TLS handshakes, H1
  connections, H2 sessions, and logical requests at explicit test boundaries.
- `client::ErrorKind` distinguishes cancellation, unsupported scheme, connect,
  TLS, ALPN, handshake, send, URI, version, and protocol-availability errors;
  underlying errors are retained where the endpoint owns them.
- Source comments document the pool lifecycle, protocol selection, replay
  buffer, and every unsafe adapter block.

There is intentionally no mandatory logging, tracing, metrics, or callback
framework. Callers that need structured diagnostics retain a clone of the log;
callers that do not opt in pay no event storage cost.

## External interop and performance status

The local loopback run started `examples/interop-server` and an independent
cleartext `nghttpd` endpoint. `scripts/interop.sh` then observed:

- curl forced H1: HTTP/1.1, 1,024 bytes;
- curl forced TLS H2: HTTP/2, 1,024 bytes;
- nghttp over TLS: negotiated ALPN `h2`, status 200, and a 1,024-byte DATA
  frame; and
- `examples/client-load` against `nghttpd` h2c: one HTTP/2 response, zero
  errors, and zero body mismatches.

This is direct tool-level interoperability evidence for these loopback
endpoints, not a claim of broad independent-server coverage.

The bounded 20-request `/1k` smoke run completed with zero errors: oha reported
100% success for forced H1 (8,393 requests/s) and forced h2c (3,728
requests/s); h2load reported 20/20 succeeded at 6,158 requests/s. These tiny
same-machine measurements only validate the harness and protocol selection;
they are not a meaningful performance comparison.

## Code size

`find src -name '*.rs' | xargs wc -l` reports 3,484 production Rust source
lines. By ownership module, 2,728 lines sit in boundaries described as
substantially ported from Hyper-util (`client::{normalize,pool,mod}` and
`server::conn::auto`); 728 lines are rewritten endpoint adapters/transport
(`client::connect`, `io`, and `runtime`); and 28 lines are crate/module exports.
This is a module-boundary accounting, not a claim that every line in a ported
module originated upstream.

The default normal graph has 57 dependency packages (58 package names including
`h12tiny`). On this audit machine, the release `client-load` example is
3,881,248 bytes and the release `interop-server` example is 3,641,264 bytes.

## Concrete remaining gaps

The evidence-led gaps are:

- Preserve the parent agent's full-suite output alongside this report if the
  audit record is intended to be independently reproduced.
- Add an integration proof for failed-connect waiter retry and a dedicated stale
  idle H1 socket race; current unit/peer-close tests cover narrower cases.
- Track the DER fixtures before claiming committed fixtures.
- Decide whether the transitive `libc` exception and duplicated `webpki-roots`
  version are acceptable dependency-budget outcomes.
- Preserve broader external-tool logs and repeat performance measurements under
  pinned conditions before making independent-server or speed claims.
