#!/usr/bin/env sh
# Check only the enabled *normal* dependency graph.  Cargo.lock is deliberately
# not inspected: dev and optional dependencies are allowed to exist there.
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
manifest="$script_dir/../Cargo.toml"

tree=$(cargo tree --manifest-path "$manifest" -e normal --prefix none "$@")

# Keep this list synchronized with plan.md.  Names are matched as Cargo package
# names (the first whitespace-delimited field), not as substrings, so a package
# such as `tokio-metrics` cannot hide a direct `tokio` match and unrelated names
# do not produce false positives.  libc is intentionally absent: both direct
# and transitive libc are permitted by the dependency policy.
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
'

printf '%s\n' "$forbidden" | while IFS= read -r package; do
    [ -n "$package" ] || continue
    if printf '%s\n' "$tree" | awk -v package="$package" '$1 == package { found = 1 } END { exit !found }'; then
        printf 'forbidden normal dependency: %s\n' "$package" >&2
        exit 1
    fi
done

printf 'normal dependency graph contains no forbidden packages\n'
