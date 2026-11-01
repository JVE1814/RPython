#!/bin/bash
# perf-runtime: release build into the workstream's target dir; prints errors only.
cd "$(dirname "$0")/../../.." || exit 1
export CARGO_TARGET_DIR=/home/user/RPython/target/perf-runtime
cargo build --release -p python 2>&1 | grep -E "^error" -A14 | head -120
echo "BUILD_END ${PIPESTATUS[0]}"
