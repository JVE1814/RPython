#!/bin/bash
# Build and run the standalone decimal engine tests (incl. the .decTest runner).
# Usage: run_engine_tests.sh [test-filter] [-- extra args]
set -e
here="$(cd "$(dirname "$0")" && pwd)"
out=/home/user/RPython/target/decimal/engine
mkdir -p "$out"
rustc --edition 2024 --test -O -C debug-assertions=on "$here/engine_tests.rs" -o "$out/t" 2>&1 | grep -v "^warning\|^  \|^   \|^$\|= note\|-->" || true
DECTEST_DIR="$here/../../../../../spec/Lib/test/decimaltestdata" "$out/t" "$@"
