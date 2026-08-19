# Upstream provenance and port inventory

This crate uses Hyper and h2 for protocol state machines.  Endpoint-layer
pooling and automatic server protocol selection are based on the mature
`hyper-util` implementation, but `hyper-util` is not a runtime dependency.
When code is copied or substantially ported, retain the applicable Hyper-util
MIT license and source attribution beside the implementation.

## Baseline

| Field | Value |
| --- | --- |
| Repository | <https://github.com/hyperium/hyper-util> |
| Version/tag | `v0.1.20` |
| Commit | `b23a13e2b7ee73e15ba008cd9b19dcd2d3861957` |
| License | MIT (see upstream `LICENSE`) |
| Local policy | Port only endpoint semantics required by `plan.md`; do not add `hyper-util` to `Cargo.toml`. |

## Source map

The destination paths below are the stable ownership boundaries for the port.
Statuses describe the intended treatment of each source, and are kept explicit
while the implementation lands in phases.

| Local destination | Upstream source at `v0.1.20` | Status | Notes |
| --- | --- | --- | --- |
| `src/client/normalize.rs` | `src/client/legacy/client.rs` | substantially ported | Preserve absolute-URI checks, request normalization, pool identity, and request-target semantics. |
| `src/client/pool.rs` | `src/client/legacy/pool.rs` | substantially ported | Preserve unique H1/shared H2 reservations, waiters, connecting bookkeeping, idle expiry, and cleanup. |
| `src/client/mod.rs` | `src/client/legacy/client.rs` | substantially ported | Retain the public legacy-client boundary without the proxy/general runtime surface. |
| `src/client/connect.rs` | `src/client/legacy/connect/dns.rs`, `src/client/legacy/connect/http.rs` | rewritten | New async-net + futures-rustls transport; no proxy, socket2, direct libc, or native TLS machinery. |
| `src/server/conn/auto.rs` | `src/server/conn/auto/mod.rs` | substantially ported | Retain progressive plaintext H2-preface detection and byte replay; omit h2c Upgrade. |
| `src/server/conn/upgrade.rs` | `src/server/conn/auto/upgrade.rs` | intentionally omitted | HTTP/1.1 Upgrade-to-h2c is an explicit non-goal for this POC. |
| `src/io.rs` | `src/rt/io.rs` | rewritten | Replace Tokio I/O assumptions with one `futures_io` ↔ `hyper::rt` adapter. |
| `src/runtime.rs` | `src/common/exec.rs`, `src/common/timer.rs` | rewritten | Keep Hyper's executor boundary and use `async_io::Timer`; no task runtime dependency. |
| `src/server/conn/auto.rs` | `src/server/conn/auto/mod.rs`, `src/common/rewind.rs` | substantially ported | Retain automatic selection and bounded preface-sniff replay in one server module. |

No Hyper-util source is vendored as a whole.  Any later destination not listed
here must be added to this table before its port is considered complete.

## Upstream test inventory

The inventory is deliberately kept even where a test is not copied.  A status
of `deferred` means the corresponding behavior still needs a local regression
test before the phase that owns it is accepted.

| Upstream test/source | Behavior | Status | Local evidence target |
| --- | --- | --- | --- |
| `tests/legacy_client.rs` | H1 unique reservation and reuse | ported | `src/client/pool.rs` unit test; `tests/raw_h1.rs` sequential reuse proof |
| `tests/legacy_client.rs` | H2 shared reservation and multiplexing | ported | `src/client/pool.rs` shared-reservation test; `tests/dogfood_h2_prior.rs` concurrent stream proof |
| `tests/legacy_client.rs` | H2 connecting marker / single establishment owner | ported | `src/client/pool.rs` cancellation test; `tests/dogfood_h2_prior.rs` concurrent-first-request proof |
| `tests/legacy_client.rs` | waiter cancellation and retry after failed connect | deferred | `tests/client_pool.rs` |
| `tests/legacy_client.rs` | idle expiration, max-idle cleanup, closed connection removal | deferred | `tests/client_pool.rs` |
| `tests/legacy_client.rs` | stale H1 connection retry | ported | `tests/raw_h1.rs` closed-session reconnect proof |
| `tests/legacy_client.rs` | absolute URI checks, Host synthesis, target normalization | ported | `src/client/normalize.rs`; `tests/raw_h1.rs` direct-wire proof |
| `tests/test_utils/mod.rs` | deterministic service/connection test helpers | deferred | `tests/support/mod.rs` |
| `tests/proxy.rs` | proxy matching, proxy auth, CONNECT proxying | not applicable | Proxies are an explicit non-goal. |
| `src/rt/tokio.rs` tests | Tokio runtime adapters | not applicable | Runtime-neutral futures I/O is implemented locally. |
| `src/server/conn/auto/upgrade.rs` tests | HTTP/1.1 Upgrade-to-h2c | intentionally omitted | Plaintext H2 means prior knowledge only. |
| `src/server/conn/auto/mod.rs` tests | progressive sniff and replay | ported | `src/server/conn/auto.rs` unit tests; `tests/dogfood_h1.rs` and `tests/dogfood_h2_prior.rs` |

This inventory must be updated from `deferred` to `ported` (or a more precise
classification) as each local regression test is added.  The goal is to
subtract dependencies without subtracting correctness evidence.
