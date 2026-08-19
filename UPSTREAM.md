# Upstream provenance and boundaries

This inventory records the source lineage of the transport code and the intentional boundaries of the workspace. It does not claim byte-for-byte identity with Hyper-util or that the upstream test suite was copied in full. Hyper and the sibling h2-futures-lite continue to own HTTP framing and protocol state machines.

## Baseline

| Field | Value |
| --- | --- |
| Upstream repository | https://github.com/hyperium/hyper-util |
| Recorded tag | v0.1.20 |
| Recorded commit | b23a13e2b7ee73e15ba008cd9b19dcd2d3861957 |
| License | MIT |
| Local transport substrate | ../hyper-futures-lite, which path-depends on ../h2-futures-lite |
| Local policy | Port only endpoint behavior required by plan.md and followup.md; do not add hyper-util. |

The recorded tag and commit are provenance metadata, not a fresh network verification. Keep applicable Hyper-util attribution and license information beside any future substantially copied code.

## Source map

| Local destination | Upstream boundary | Treatment | Current local scope |
| --- | --- | --- | --- |
| crates/h12tiny-client/src/normalize.rs | client/legacy/client.rs | substantially ported | Origin-form, Host, authority, and CONNECT normalization. |
| crates/h12tiny-client/src/pool.rs | client/legacy/pool.rs | substantially ported | H1 unique and H2 shared reservations, markers, waiters, expiry, and idle capping. |
| crates/h12tiny-client/src/lib.rs | client/legacy/client.rs | substantially ported | Client dispatch, handshake, pool lifecycle, and safe retry boundaries. |
| crates/h12tiny-client/src/connect.rs | client/legacy/connect/{dns,http}.rs | rewritten | async-net DNS/TCP, optional Rustls/ALPN, custom dialer, and establishment timeout. |
| crates/h12tiny-server/src/conn/auto.rs | server/conn/auto/mod.rs, common/rewind.rs | substantially ported | Progressive plaintext H1/H2 selection and replay; TLS dispatch is local Rustls/ALPN policy. |
| crates/h12tiny-core/src/io.rs | rt/io.rs | rewritten | Explicit futures-I/O to Hyper I/O bridges, including raw-upgrade adaptation. |
| crates/h12tiny-core/src/runtime.rs | common/{exec,timer}.rs | rewritten | Runtime-neutral executor and timer adapter. |

The workspace split, h12tiny-util, h12tiny-web, raw H1 upgrade wiring, listener lifecycle helpers, and compatibility fixtures are local extensions; they are not represented as copied Hyper-util code.

## Intentional absences

- HTTP/1.1 Upgrade: h2c remains out of scope; cleartext H2 uses prior knowledge.
- There is no proxy, redirect, cookie, URL-resolution, native-TLS, Tokio, Tower, Reqwest, or Axum integration layer.
- Raw HTTP/1 upgrade remains available for application-selected protocols.
  The optional `websocket` feature adds RFC 6455 HTTP/1.1 validation, the
  switching response, and futures-lite server-role framing; its message policy
  remains application-owned. HTTP/2 extended CONNECT remains out of scope.

See REPORT.md for the complete workspace, feature, dependency, size, test, and smolvm-migration evidence.
