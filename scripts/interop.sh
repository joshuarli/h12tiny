#!/usr/bin/env sh
# External interoperability harness. The application server is intentionally
# outside h12tiny's low-level API, so callers provide URLs for a running test
# service instead of making cargo test depend on external tools.
set -eu

: "${H12TINY_HTTP1_URL:?set to an h12tiny plaintext HTTP/1 endpoint}"
: "${H12TINY_HTTPS_URL:?set to an h12tiny TLS endpoint advertising h2 and http/1.1}"
: "${H12TINY_NGHTTPD_URL:?set to an independent nghttpd HTTP/2 URL for a client check}"

curl --fail --silent --show-error --http1.1 "$H12TINY_HTTP1_URL"
curl --fail --silent --show-error --http2 "$H12TINY_HTTPS_URL"
nghttp -nv "$H12TINY_HTTPS_URL"

# The final command is application-specific because h12tiny deliberately has
# no router or CLI. Point it at a binary/example that uses `Client`.
: "${H12TINY_CLIENT_CMD:?set to a command that sends one h2 request with h12tiny}"
sh -c "$H12TINY_CLIENT_CMD \"$H12TINY_NGHTTPD_URL\""
