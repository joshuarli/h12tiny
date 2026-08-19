# Engineering report: modular application extension

Audit date: 2026-08-19. This report records the completed followup.md extension in the current working tree. It separates verified facts from intentional scope limits; it does not claim that ../smolvm was modified.

## Workspace graph

| Crate | Responsibility | Deliberate exclusions |
| --- | --- | --- |
| h12tiny-core | Futures-I/O to Hyper I/O adapters, executor, timer | Pooling, listener loops, TLS policy, JSON, router |
| h12tiny-client | Direct connector/dialer, Rustls/ALPN, normalization, pool, H1/H2 handshakes | Server, router, serde, matchit |
| h12tiny-server | H1/H2 auto serving, Rustls ALPN dispatch, raw H1 upgrade, TCP/TLS/Unix lifecycle | Client pool/connector, router, serde, matchit |
| h12tiny-util | Body constructors/streams, bounded collection, idle timeout, replay factories, optional JSON | DNS, TCP, TLS, accept loop, router |
| h12tiny-web | Router, extractors, response conversion, route limits/deadlines, optional JSON/query/SSE/CORS/upgrade extractor | Protocol implementation, pooling, TLS, WebSocket framing, Tower |
| h12tiny | Conditional facade reexports | Endpoint implementation |

Dependencies follow those ownership boundaries: client and server use core; util is transport-free; web uses util and only reaches server for its optional raw-upgrade extractor. Client never depends on server or web.

## Feature graph

The facade default is empty. Its features only forward component features:

| Facade feature | Forwarded component surface |
| --- | --- |
| core | h12tiny-core |
| client, server, util, web | Corresponding component; web also selects facade server/util reexports |
| http1, http2 | Selected client/server protocol feature; neither role is instantiated by protocol alone |
| tls | Selected client/server Rustls support |
| upgrade | Server H1 raw upgrade and web HttpUpgrade extractor when web is present |
| json | util JSON and web JSON when web is present |
| query, sse, cors | Web-only optional application vocabulary |
| full | Client, server, util, web, both protocols, TLS, upgrade, JSON, query, SSE, and CORS |

h12tiny-client and h12tiny-server each default to no protocol. Their H1 and H2 features are independent. h12tiny-util/json, h12tiny-web/json, and h12tiny-web/query are the only serde-bearing convenience surfaces.

## Dependency evidence

scripts/check-features.sh runs both normal-tree assertions and the direct component/facade checks. The following unique normal-package counts were recorded with cargo tree -e normal --prefix none; the root package is included in facade counts.

| Minimal configuration | Normal packages | h2-futures |
| --- | ---: | --- |
| client H1 | 38 | absent |
| client H1 + TLS | 49 | absent |
| client H2 + TLS | 57 | present |
| server H1 | 37 | absent |
| server H2 + TLS | 55 | present |
| facade full | 79 | present |

This proves the key isolation property: H1-only client/server builds omit H2, and minimal roles omit the other endpoint role, router, TLS when disabled, and serde. scripts/check-normal-dependencies.sh audits full by default and rejects Tokio, Tokio-util, native TLS, Hyper-util, Reqwest, Tower, Tower-layer, Axum, async-trait, URL, mime, cookie, and socket2 from the normal graph. libc is intentionally permitted. fastwebsockets is a root dev-dependency only and is absent from every normal graph.

## Application primitives and lifecycle evidence

- The connector exposes an object-safe Dialer, explicit Rustls config, mTLS-compatible configuration, and a timer-raced establishment timeout. It does not set an unsafe implicit whole-request deadline.
- h12tiny-util reexports the normal http-body-util vocabulary and adds body constructors, reader/frame streams, bytes_limited/text_limited/optional json_limited, idle body timeouts, bearer, and explicit BodyFactory/ReplayableRequest construction.
- h12tiny-web provides static, parameter, and catch-all paths; method dispatch including 404/405; State, Path, Query, Json, optional Json, Extension, Bytes, raw Request, and raw query; small IntoResponse; per-route streaming limits (413) and handler deadlines (408); SSE fields; structural CORS; and raw H1 upgrade extraction.
- server::serve supports TCP and Unix listeners. server::serve_tls owns the Rustls handshake plus ALPN dispatch. Both track executor-owned tasks, stop accepting on caller shutdown, cancel pending TLS handshakes, gracefully drain established drivers, await completion, optionally cap active work by closing excess accepted streams, and expose a connection-error hook.
- HyperIo<T> in core adapts Hyper upgraded I/O back to futures-I/O so an application-selected framing library composes without adding framing to h12tiny itself.

## Verification record

The final local verification run passed:

| Command or test | Evidence |
| --- | --- |
| scripts/check-features.sh | Direct H1/H2 client/server and facade feature/tree isolation checks passed. |
| sh scripts/check-normal-dependencies.sh | Full normal graph contains no prohibited package. |
| scripts/miri-io.sh | Four focused core I/O adapter tests passed under Miri. |
| cargo test --workspace --all-features | Workspace units, existing transport regressions, and all integration fixtures passed. |
| tests/client_compat.rs | JSON plus application-owned OCI bearer retry, fresh streaming upload factory, incremental download, bounded response recovery, and mTLS passed. |
| tests/web_transport.rs and tests/web_tls.rs | One Router service works over plaintext and TLS H1/H2; the TLS case uses tracked serve_tls shutdown. |
| tests/unix_lifecycle.rs | Real Unix socket H1 accepts, responds, drains, observes EOF, and cleans up its socket path. |
| tests/web_upgrade.rs | HttpUpgrade route provides raw byte echo. |
| tests/websocket_interop.rs | A dev-only fastwebsockets-futures-lite parser reads a masked WebSocket text frame from HttpUpgrade and writes the echoed frame. |
| scripts/interop.sh | curl H1 and TLS H2 each returned 1,024 bytes; nghttp negotiated H2 against h12tiny; h12tiny H2 client fetched 1,024 bytes from independent cleartext nghttpd. |

No formatter, linter, pre-commit hook, or remote push was run.

## Size evidence

Production Rust source lines, counted by find with Rust source filters:

| Crate | Lines |
| --- | ---: |
| h12tiny-core | 714 |
| h12tiny-client | 2,260 |
| h12tiny-server | 1,753 |
| h12tiny-util | 759 |
| h12tiny-web | 1,690 |
| facade | 40 |
| total | 7,216 |

h12tiny-web is the principal framework-like addition. Its 1,690 lines are bounded to routing/extraction/response conversion and the explicitly selected SSE/CORS/upgrade conveniences; it deliberately has no middleware stack, protocol implementation, or dependency-injection surface. Further features should remain application-side unless a concrete migration fixture identifies a small reusable primitive.

Release builds on this audit machine produced:

| Explicit example feature set | Binary | Bytes |
| --- | --- | ---: |
| server,http1,http2,tls | interop-server | 3,644,368 |
| client,http1,http2,tls | client-load | 3,881,936 |

These are representative release artifact sizes, not a benchmark or a clean-build-time claim.

## smolvm migration inventory

../smolvm was inspected read-only. Its normal dependencies include Axum, Axum-server, Tower/Tower-http, Reqwest, and OpenAPI helpers. The actual route parameter shapes are scalar strings plus two- and three-element string tuples; they are covered directly by Path. Its handler vocabulary includes State, Path, Query, Json, optional Json, Extension, Bytes, Result, SSE, and one WebSocket endpoint.

| Classification | smolvm use | Migration result |
| --- | --- | --- |
| Mechanical port | Conventional API handlers in src/api/handlers | Imports and route assembly map to web State/Path/Query/Json/Extension/Bytes/IntoResponse; H1/H2/TLS serving maps to server helpers. |
| Small application adapter | CORS and route timeout layers | Use structural Cors and per-route timeout; trace IDs remain ordinary request extensions. |
| Small application adapter | Registry bounded reads, streaming pull/push, 401 bearer replay, peer mTLS | Map to client plus util primitives while retaining smolvm status/error mapping and explicit retry decisions. |
| Application-specific | OCI realm validation, token cache, redirect policy, upload/mount/range semantics | Intentionally remain in smolvm; no URL/redirect/auth framework was added. |
| Application-specific | SSE keepalive policy and WebSocket session/PTY protocol | SSE data framing is supplied; keepalive scheduling and WebSocket framing use application code. Raw H1 upgrade plus the dev interop proof provide the WebSocket seam. |
| Application-side | Utoipa/OpenAPI/Swagger and tracing configuration | Deliberately remain independent of h12tiny. |
| Still-uncovered reusable primitive | None identified for the specified target vocabulary | The remaining work is explicit application policy/adaptation, not a missing transport or router capability. |

## Explicit non-goals retained

The extension still does not add Tokio, native TLS, Reqwest, Axum, Tower, proxy or redirect frameworks, WebSocket framing, OpenAPI, application tracing, HTTP/3, or HTTP/2 extended CONNECT. HTTP/1 h2c upgrade remains omitted; cleartext H2 is prior knowledge.
