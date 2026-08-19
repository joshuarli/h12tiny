#!/usr/bin/env sh
# Focused memory-model check for the sole unsafe adapter boundary.
set -eu

cargo +nightly miri test -p h12tiny-core io::tests
