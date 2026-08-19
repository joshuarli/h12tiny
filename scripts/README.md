# Interoperability and load tools

The examples are deliberately small and use only this repository's existing
dependencies. Start the deterministic endpoint server in one terminal:

```sh
cargo run --release --example interop-server
```

It listens on `http://127.0.0.1:3000` and
`https://127.0.0.1:3443` by default. The TLS listener uses the committed
localhost fixture and advertises `h2,http/1.1`; its certificate is suitable
for local `curl --insecure`/`nghttp -y` checks only. Endpoints are `/0`,
`/1k`, and `/64k`.

For external checks, point the harness at complete endpoint URLs. The
independent URL should be an h2c endpoint from `nghttpd`, or an HTTPS server
whose certificate is trusted by h12tiny's normal web-PKI client policy:

```sh
H12TINY_HTTP1_URL=http://127.0.0.1:3000/1k \
H12TINY_HTTPS_URL=https://127.0.0.1:3443/1k \
H12TINY_NGHTTPD_URL=http://127.0.0.1:8080/1k \
scripts/interop.sh
```

`interop.sh` checks HTTP/1.1 and TLS HTTP/2 with curl, exercises the TLS
endpoint with nghttp, and runs the internal `client-load` example against the
independent H2 URL. `H12TINY_EXPECTED_BYTES` defaults to `1024`; set it to
`0` or `65536` when using `/0` or `/64k`.

For a performance run, use the same body size, protocol, and loopback server
for both URLs:

```sh
H12TINY_HTTP1_URL=http://127.0.0.1:3000/64k \
H12TINY_HTTP2_URL=http://127.0.0.1:3000/64k \
H12TINY_REQUESTS=10000 H12TINY_CONNECTIONS=16 H12TINY_STREAMS=16 \
scripts/bench.sh
```

The benchmark invokes oha for the uniform H1/H2 comparison and h2load for the
specialist H2 measurement. It defaults to bounded H2 windows (`16` stream
bits, `20` connection bits) so results do not silently represent an
effectively unlimited-flow-control workload. See `--help` on either script
for all environment controls.

`client-load` also prints its configured and observed request concurrency plus
event-derived `tcp_connections`, `tls_handshakes`, `h1_connections`, and
`h2_sessions`. These are diagnostic counts for a single run, not a benchmark
score or a replacement for server-side metrics.

## Dependency policy check

Run the normal-graph policy check with:

```sh
sh scripts/check-normal-dependencies.sh
```

The check rejects the forbidden production packages listed in `plan.md` from
the enabled normal graph. `libc` is intentionally allowed, whether it is direct
or transitive, because platform support may require it. Dev and optional
dependencies are not inspected.
