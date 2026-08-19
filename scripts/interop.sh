#!/usr/bin/env sh
# Check an already-running h12tiny example server and an independent H2 peer.
#
# Start the local fixture with:
#   cargo run --release --example interop-server
# Then run this script with endpoint URLs, for example:
#   H12TINY_HTTP1_URL=http://127.0.0.1:3000/1k \
#   H12TINY_HTTPS_URL=https://127.0.0.1:3443/1k \
#   H12TINY_NGHTTPD_URL=http://127.0.0.1:8080/1k \
#   scripts/interop.sh
#
# The HTTPS fixture is intentionally local and self-signed, so curl/nghttp are
# run with peer verification disabled. The internal client uses its normal web
# PKI policy; use a trusted independent server (or plaintext h2c) for that
# leg. No command supplied through an environment variable is evaluated.
set -eu

usage() {
    cat <<'EOF'
usage: H12TINY_HTTP1_URL=URL H12TINY_HTTPS_URL=URL \
       H12TINY_NGHTTPD_URL=URL scripts/interop.sh

The first two URLs are deterministic endpoints on an h12tiny server. The
third URL is an independent HTTP/2 endpoint (nghttpd is a convenient choice).
Each URL should return the same body size; H12TINY_EXPECTED_BYTES defaults to
1024 for the /1k example endpoint.
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
    echo "interop: $*" >&2
    exit 2
}

require_tool() {
    command -v "$1" >/dev/null 2>&1 || die "required tool not found: $1"
}

require_tool curl
require_tool nghttp
require_tool cargo

http1_url=${H12TINY_HTTP1_URL:?set H12TINY_HTTP1_URL to an h12tiny HTTP/1.1 endpoint}
https_url=${H12TINY_HTTPS_URL:?set H12TINY_HTTPS_URL to an h12tiny TLS endpoint}
nghttpd_url=${H12TINY_NGHTTPD_URL:?set H12TINY_NGHTTPD_URL to an independent HTTP/2 endpoint}
expected=${H12TINY_EXPECTED_BYTES:-1024}
case "$expected" in
    ''|*[!0-9]*) die "H12TINY_EXPECTED_BYTES must be a non-negative integer" ;;
esac

probe_curl() {
    label=$1
    version_flag=$2
    url=$3
    expected_bytes=$4
    expected_version=$5
    case "$url" in
        https://*) result=$(curl --fail --silent --show-error --insecure "$version_flag" \
            --output /dev/null --write-out '%{http_version} %{size_download}' "$url") ;;
        *) result=$(curl --fail --silent --show-error "$version_flag" \
            --output /dev/null --write-out '%{http_version} %{size_download}' "$url") ;;
    esac
    version=${result%% *}
    bytes=${result#* }
    [ "$version" = "$expected_version" ] || die "$label negotiated HTTP/$version; expected HTTP/$expected_version"
    [ "$bytes" = "$expected_bytes" ] || die "$label returned $bytes bytes; expected $expected_bytes"
    echo "$label: HTTP/$version, bytes=$bytes"
}

probe_curl "h12tiny H1" --http1.1 "$http1_url" "$expected" 1.1
probe_curl "h12tiny TLS H2" --http2 "$https_url" "$expected" 2

# -y permits the committed local fixture certificate. nghttp's verbose output
# includes the negotiated ALPN and response headers, which makes protocol
# selection and the response body observable in the command log.
echo "nghttp independent check: $https_url"
nghttp -y -nv "$https_url"

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
echo "h12tiny client -> independent H2 endpoint: $nghttpd_url"
cargo run --manifest-path "$repo_dir/Cargo.toml" --release --example client-load -- \
    "$nghttpd_url" --http2 --requests 1 --concurrency 1
