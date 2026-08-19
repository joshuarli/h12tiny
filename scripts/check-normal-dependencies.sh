#!/usr/bin/env sh
# Check only the enabled *normal* dependency graph.  Cargo.lock is deliberately
# not inspected: dev and optional dependencies are allowed to exist there.
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
manifest="$script_dir/../Cargo.toml"

tree=$(cargo tree --manifest-path "$manifest" -e normal --prefix none "$@")
direct_tree=$(cargo tree --manifest-path "$manifest" -e normal --prefix none --depth 1 "$@")

# Keep this list synchronized with plan.md.  Names are matched as Cargo package
# names (the first whitespace-delimited field), not as substrings, so a package
# such as `tokio-metrics` cannot hide a direct `tokio` match and unrelated names
# do not produce false positives.
forbidden='\
tokio
tokio-util
native-tls
hyper-util
reqwest
tower
tower-layer
axum
async-trait
url
serde
serde_json
mime
cookie
socket2
libc
'

printf '%s\n' "$forbidden" | while IFS= read -r package; do
    [ -n "$package" ] || continue
    if printf '%s\n' "$tree" | awk -v package="$package" '$1 == package { found = 1 } END { exit !found }'; then
        # async-net/async-io's Unix polling backend and rustls's ring backend
        # use libc transitively.  That platform support is unavoidable for the
        # requested transports, but a direct libc dependency remains forbidden.
        if [ "$package" = "libc" ]; then
            if printf '%s\n' "$direct_tree" | awk '$1 == "libc" { found = 1 } END { exit !found }'; then
                printf 'forbidden direct dependency: %s\n' "$package" >&2
                exit 1
            fi
            printf 'allowed transitive platform dependency: libc (async-net/async-io or rustls/ring)\n' >&2
            continue
        fi
        printf 'forbidden normal dependency: %s\n' "$package" >&2
        exit 1
    fi
done

printf 'normal dependency graph contains no forbidden packages\n'
