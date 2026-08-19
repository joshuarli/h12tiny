#!/usr/bin/env sh
# Manual performance harness. Run only after `cargo test` and interop pass.
# URLs must serve deterministic 0 B, 1 KiB, or 64 KiB endpoints selected by
# the application embedding the low-level h12tiny server.
set -eu

: "${H12TINY_HTTP1_URL:?set to the selected HTTP/1 benchmark endpoint}"
: "${H12TINY_HTTP2_URL:?set to the selected HTTP/2 benchmark endpoint}"

requests=${H12TINY_REQUESTS:-10000}
connections=${H12TINY_CONNECTIONS:-16}
streams=${H12TINY_STREAMS:-16}

echo "H1: requests=$requests connections=$connections"
oha --no-tui --http-version 1.1 -n "$requests" -c "$connections" "$H12TINY_HTTP1_URL"

echo "H2: requests=$requests connections=$connections streams=$streams"
oha --no-tui --http-version 2 -n "$requests" -c "$connections" "$H12TINY_HTTP2_URL"
h2load -n "$requests" -c "$connections" -m "$streams" "$H12TINY_HTTP2_URL"
