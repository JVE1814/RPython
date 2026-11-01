#!/bin/bash
# Build tools/capi_selftest/capi_selftest.c and run test.py with the given interpreter.
#   tools/capi_selftest/run.sh [path/to/python3]
set -e
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PY="${1:-${CARGO_TARGET_DIR:-$ROOT/target}/debug/python3}"
OUT="$(mktemp -d)"; trap 'rm -rf "$OUT"' EXIT
cp "$ROOT/tools/pyconfig.h" "$OUT/pyconfig.h"  # same config header build_test_exts.sh uses
gcc -shared -fPIC -g -O1 -I"$OUT" -I"$ROOT/spec/Include" "$ROOT/tools/capi_selftest/capi_selftest.c" \
    -o "$OUT/capi_selftest.cpython-315t-x86_64-linux-gnu.so" -lpthread
cd "$OUT"
PYTHONPATH="$OUT" "$PY" "$ROOT/tools/capi_selftest/test.py"
