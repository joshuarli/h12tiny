# Upstream provenance and local evidence

This inventory records where the endpoint-layer design came from and what the
current working tree proves. It is a provenance map, not a claim that the local
code is byte-for-byte identical to `hyper-util` or that the full upstream test
suite has been copied. Hyper and h2 continue to own HTTP framing and protocol
state machines; this crate owns transport, routing, pooling, and endpoint
protocol selection.

## Baseline

| Field | Value |
| --- | --- |
| Repository | <https://github.com/hyperium/hyper-util> |
| Version/tag | `v0.1.20` |
| Commit | `b23a13e2b7ee73e15ba008cd9b19dcd2d3861957` |
| License | MIT (see upstream `LICENSE`) |
| Local policy | Port only the endpoint semantics required by `plan.md`; do not add `hyper-util` to `Cargo.toml`. |

The tag and commit above are the project's recorded baseline; this local audit
did not perform a fresh external fetch to re-validate that reference.

The sibling protocol substrate is a path dependency: `hyper` comes from
`../hyper-futures-lite` and that crate path-depends on `../h2-futures-lite`.
Those repositories are local implementation inputs, not vendored source in this
crate. Preserve their independent provenance when distributing this project.

## Source map

Statuses describe the local treatment and the evidence currently present. A
"substantially ported" entry means the endpoint behavior follows the named
upstream boundary; it does not mean every upstream option or regression test is
retained.

| Local destination | Upstream source at `v0.1.20` | Status | Local evidence and scope |
| --- | --- | --- | --- |
| `src/client/normalize.rs` | `src/client/legacy/client.rs` | substantially ported | Unit tests and `tests/raw_h1.rs` cover origin form, root/path/query, Host synthesis/preservation, ports, IPv4/IPv6, and CONNECT authority form. |
| `src/client/pool.rs` | `src/client/legacy/pool.rs` | substantially ported | Unit tests cover unique H1 and shared H2 reservations, the H2 connecting marker, waiter cleanup, max-idle capping, closed values, and checkout expiry. The complete upstream pool test corpus is not present. |
| `src/client/mod.rs` | `src/client/legacy/client.rs` | substantially ported | Keeps the legacy-client boundary and retry/lifecycle shape while omitting proxy/general runtime surface; integration coverage is listed in `REPORT.md`. |
| `src/client/connect.rs` | `src/client/legacy/connect/dns.rs`, `src/client/legacy/connect/http.rs` | rewritten | Direct `async-net` TCP plus `futures-rustls`/Rustls/ALPN transport; no proxy, socket2, direct libc, or native TLS machinery. |
| `src/server/conn/auto.rs` | `src/server/conn/auto/mod.rs`, `src/common/rewind.rs` | substantially ported | Retains progressive plaintext preface selection and byte replay in one module; local unit tests cover divergence, complete preface, and partial replay. |
| `src/server/conn/auto.rs` | `src/server/conn/auto/upgrade.rs` | intentionally omitted | HTTP/1.1 Upgrade-to-h2c is a stated non-goal; plaintext H2 means prior knowledge. |
| `src/io.rs` | `src/rt/io.rs` | rewritten | One futures-io to Hyper I/O adapter with explicit unsafe projection/cursor comments and focused unit/Miri coverage. |
| `src/runtime.rs` | `src/common/exec.rs`, `src/common/timer.rs` | rewritten | Hyper executor boundary plus `async_io::Timer`; no task runtime dependency in the normal graph. |

No Hyper-util source is vendored as a whole. Any later destination not listed
here must be added before its port is treated as complete. Applicable Hyper-util
MIT attribution and license terms must remain available beside any future
substantially copied implementation.

## Upstream behavior inventory

The classifications below distinguish local behavioral coverage from a literal
test port. `covered locally` means the behavior has a focused local proof;
`partial` means the local proof covers only part of the upstream behavior;
`deferred` means a required proof is still absent.

| Upstream behavior/source | Local evidence | Classification | Remaining boundary |
| --- | --- | --- | --- |
| `tests/legacy_client.rs`: H1 unique reservation and reuse | `src/client/pool.rs` unit test; `tests/raw_h1.rs::sequential_h1_requests_reuse_one_direct_connection`; H1 matrix counter proof | covered locally | Not a verbatim upstream test port. |
| `tests/legacy_client.rs`: H2 shared reservation and multiplexing | `src/client/pool.rs` shared-reservation test; `tests/dogfood_h2_prior.rs`; `tests/dogfood_tls.rs` 100-request counter proof; `tests/h2_lifecycle.rs` | covered locally | H2 cancellation isolation and close/reconnect targets now pass; this is not a verbatim upstream test port. |
| `tests/legacy_client.rs`: H2 connecting marker / single establishment owner | `src/client/pool.rs` marker/cancellation tests; 16-request cleartext and 100-request TLS first-request races | covered locally | The local stress sizes are 16 and 100, not the full upstream suite. |
| `tests/legacy_client.rs`: waiter cancellation and retry after failed connect | `src/client/pool.rs::dropped_checkout_removes_its_waiter`; `cancelled_h2_establishment_wakes_waiter_and_allows_retry` | partial | No integration test exercises a failed connector with multiple network waiters. |
| `tests/legacy_client.rs`: idle expiration, max-idle cleanup, closed connection removal | `src/client/pool.rs` expiry, max-idle, closed-value, and background timer tests | covered locally | This is local behavioral coverage, not a verbatim upstream test port. |
| `tests/legacy_client.rs`: stale H1 connection retry | `tests/raw_h1.rs::closed_h1_session_is_not_reused_and_next_request_reconnects` | partial | Covers an explicit peer close, not every stale-idle-socket race. |
| `tests/legacy_client.rs`: absolute URI checks, Host synthesis, target normalization | `src/client/normalize.rs` unit tests and raw-wire tests in `tests/raw_h1.rs` | covered locally | The local matrix is smaller than upstream's complete URI corpus. |
| `tests/test_utils/mod.rs`: deterministic service/connection helpers | `tests/support/mod.rs` (`FullBody`, `YieldingBody`, counters, TLS fixture builders) | local replacement | Helpers were written for this crate; they are not copied upstream helpers. |
| `tests/proxy.rs`: proxy matching, auth, CONNECT proxying | No local implementation or tests | not applicable | Proxies are an explicit non-goal. Direct-origin CONNECT authority normalization is tested; proxy CONNECT is not. |
| `src/rt/tokio.rs` tests: Tokio runtime adapters | No local implementation | not applicable | Runtime-neutral futures I/O is implemented locally. |
| `src/server/conn/auto/upgrade.rs` tests: HTTP/1.1 Upgrade-to-h2c | No local implementation | intentionally omitted | Plaintext H2 prior knowledge only. |
| `src/server/conn/auto/mod.rs` tests: progressive sniff and replay | `src/server/conn/auto.rs` unit tests; `tests/dogfood_h1.rs`; `tests/dogfood_h2_prior.rs`; external loopback smoke harness | covered locally | TLS ALPN dispatch and the recorded endpoint probes have evidence; this is not a verbatim upstream test port. |

The complete verification record, dependency caveats, feature matrix, fixture
status, and concrete remaining gaps are in [`REPORT.md`](REPORT.md).
