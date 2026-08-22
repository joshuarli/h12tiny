#!/usr/bin/env sh
# Verify the component feature graph as a product contract. Cargo.lock is not
# inspected: optional and dev dependencies may legitimately appear there.
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

tree() {
    cargo tree "$@" -e normal --prefix none
}

has_package() {
    package=$1
    graph=$2
    printf '%s\n' "$graph" | awk -v package="$package" '$1 == package { found = 1 } END { exit !found }'
}

require_absent() {
    package=$1
    graph=$2
    label=$3
    if has_package "$package" "$graph"; then
        printf '%s unexpectedly contains %s\n' "$label" "$package" >&2
        exit 1
    fi
}

require_present() {
    package=$1
    graph=$2
    label=$3
    if ! has_package "$package" "$graph"; then
        printf '%s is missing expected %s\n' "$label" "$package" >&2
        exit 1
    fi
}

check_forbidden() {
    graph=$1
    label=$2
    for package in tokio tokio-util native-tls reqwest axum hyper-util tower tower-layer; do
        require_absent "$package" "$graph" "$label"
    done
}

client_h1=$(tree -p h12tiny-client --no-default-features --features http1)
client_h2=$(tree -p h12tiny-client --no-default-features --features http2)
client_h1_tls=$(tree -p h12tiny-client --no-default-features --features http1,tls)
server_h1=$(tree -p h12tiny-server --no-default-features --features http1)
server_h2=$(tree -p h12tiny-server --no-default-features --features http2)
util_plain=$(tree -p h12tiny-util --no-default-features)
web_plain=$(tree -p h12tiny-web --no-default-features)
web_websocket=$(tree -p h12tiny-web --no-default-features --features websocket)
facade_h1=$(tree -p h12tiny --no-default-features --features client,http1)
facade_websocket=$(tree -p h12tiny --no-default-features --features websocket)

for package in laputa-h2-futures h12tiny-server h12tiny-web rustls serde serde_json tokio; do
    require_absent "$package" "$client_h1" "H1-only client"
done
require_present laputa-h2-futures "$client_h2" "H2-only client"
for package in h12tiny-server h12tiny-web serde serde_json tokio; do
    require_absent "$package" "$client_h2" "H2-only client"
done
require_present rustls "$client_h1_tls" "TLS H1 client"

for package in laputa-h2-futures h12tiny-client h12tiny-web serde serde_json tokio; do
    require_absent "$package" "$server_h1" "H1-only server"
done
require_present laputa-h2-futures "$server_h2" "H2-only server"
for package in h12tiny-client h12tiny-web serde serde_json tokio; do
    require_absent "$package" "$server_h2" "H2-only server"
done

for package in serde serde_json tokio h12tiny-client h12tiny-server h12tiny-web; do
    require_absent "$package" "$util_plain" "util without json"
done
for package in base64 laputa-fastwebsockets-futures-lite h12tiny-core serde_json serde_urlencoded sha1 tokio tower axum; do
    require_absent "$package" "$web_plain" "web without json/query"
done
for package in base64 laputa-fastwebsockets-futures-lite h12tiny-core h12tiny-server sha1; do
    require_present "$package" "$web_websocket" "web WebSocket feature"
done
for package in base64 laputa-fastwebsockets-futures-lite sha1; do
    require_present "$package" "$facade_websocket" "facade WebSocket feature"
done
for package in laputa-h2-futures h12tiny-server h12tiny-web rustls serde serde_json tokio; do
    require_absent "$package" "$facade_h1" "facade H1-only client"
done

for label_and_graph in \
    "H1-only client:$client_h1" \
    "H2-only client:$client_h2" \
    "TLS H1 client:$client_h1_tls" \
    "H1-only server:$server_h1" \
    "H2-only server:$server_h2" \
    "util without json:$util_plain" \
    "web without json/query:$web_plain" \
    "web WebSocket feature:$web_websocket" \
    "facade H1-only client:$facade_h1" \
    "facade WebSocket feature:$facade_websocket"; do
    label=${label_and_graph%%:*}
    graph=${label_and_graph#*:}
    check_forbidden "$graph" "$label"
done

cargo check -p h12tiny-client --no-default-features --features http1
cargo check -p h12tiny-client --no-default-features --features http2
cargo check -p h12tiny-client --no-default-features --features http1,http2
cargo check -p h12tiny-client --no-default-features --features http1,tls
cargo check -p h12tiny-client --no-default-features --features http2,tls
cargo check -p h12tiny-server --no-default-features --features http1
cargo check -p h12tiny-server --no-default-features --features http2
cargo check -p h12tiny-server --no-default-features --features http1,http2
cargo check -p h12tiny-server --no-default-features --features http1,tls
cargo check -p h12tiny-server --no-default-features --features http2,tls
cargo check -p h12tiny --no-default-features --features client,http1
cargo check -p h12tiny --no-default-features --features client,http2
cargo check -p h12tiny --no-default-features --features server,http1
cargo check -p h12tiny --no-default-features --features server,http2
cargo check -p h12tiny --no-default-features --features websocket
cargo check -p h12tiny --no-default-features --features client,server,http1,http2,tls
cargo check -p h12tiny --no-default-features --features full

printf '%s\n' 'feature and dependency isolation checks passed'
