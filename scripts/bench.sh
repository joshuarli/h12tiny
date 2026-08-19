#!/usr/bin/env sh
# Manual performance harness for deterministic endpoints from
# examples/interop-server.rs. Run correctness checks first:
#
#   cargo test -p h12tiny --all-features
#   H12TINY_HTTP1_URL=http://127.0.0.1:3000/64k \
#   H12TINY_HTTP2_URL=http://127.0.0.1:3000/64k \
#   scripts/bench.sh
#
# The H2 URL may be HTTPS when its certificate is trusted by both oha and
# h2load. The committed localhost fixture is self-signed; use plaintext h2c
# for h2load or configure a trusted external certificate. Set
# H12TINY_INSECURE=1 only for the local oha TLS check.
set -eu

usage() {
    cat <<'EOF'
usage: H12TINY_HTTP1_URL=URL H12TINY_HTTP2_URL=URL scripts/bench.sh

Required URLs are complete deterministic endpoints (/0, /1k, or /64k), not
base directories. Environment controls:
  H12TINY_REQUESTS       total requests (default: 10000)
  H12TINY_CONNECTIONS    client connections/concurrency (default: 16)
  H12TINY_STREAMS        concurrent H2 streams per connection (default: 16)
  H12TINY_INSECURE       pass --insecure to oha (default: 0)
  H12TINY_H2_WINDOW_BITS stream window bits for h2load (default: 16)
  H12TINY_H2_CONN_WINDOW_BITS connection window bits (default: 20)
EOF
}

if [ "${1:-}" = "--help" ] || [ "${1:-}" = "-h" ]; then
    usage
    exit 0
fi
if [ "$#" -ne 0 ]; then
    usage >&2
    exit 2
fi

die() {
    echo "bench: $*" >&2
    exit 2
}

require_tool() {
    command -v "$1" >/dev/null 2>&1 || die "required tool not found: $1"
}

require_tool oha
require_tool h2load

http1_url=${H12TINY_HTTP1_URL:?set H12TINY_HTTP1_URL to an HTTP/1.1 endpoint}
http2_url=${H12TINY_HTTP2_URL:?set H12TINY_HTTP2_URL to an HTTP/2 endpoint}
requests=${H12TINY_REQUESTS:-10000}
connections=${H12TINY_CONNECTIONS:-16}
streams=${H12TINY_STREAMS:-16}
window_bits=${H12TINY_H2_WINDOW_BITS:-16}
connection_window_bits=${H12TINY_H2_CONN_WINDOW_BITS:-20}
insecure=${H12TINY_INSECURE:-0}

positive() {
    name=$1
    value=$2
    case "$value" in
        ''|*[!0-9]*) die "$name must be a positive integer, got $value" ;;
        0) die "$name must be greater than zero" ;;
    esac
}

positive H12TINY_REQUESTS "$requests"
positive H12TINY_CONNECTIONS "$connections"
positive H12TINY_STREAMS "$streams"
positive H12TINY_H2_WINDOW_BITS "$window_bits"
positive H12TINY_H2_CONN_WINDOW_BITS "$connection_window_bits"
case "$insecure" in
    0|1) ;;
    *) die "H12TINY_INSECURE must be 0 or 1" ;;
esac

echo "H1 comparison: requests=$requests concurrency=$connections url=$http1_url"
oha --no-tui --http-version 1.1 -n "$requests" -c "$connections" "$http1_url"

echo "H2 comparison: requests=$requests concurrency=$connections url=$http2_url"
if [ "$insecure" = 1 ]; then
    oha --insecure --no-tui --http-version 2 -n "$requests" -c "$connections" "$http2_url"
else
    oha --no-tui --http-version 2 -n "$requests" -c "$connections" "$http2_url"
fi

echo "H2 specialist: requests=$requests clients=$connections streams=$streams window_bits=$window_bits connection_window_bits=$connection_window_bits"
h2load -n "$requests" -c "$connections" -m "$streams" \
    -w "$window_bits" -W "$connection_window_bits" "$http2_url"
