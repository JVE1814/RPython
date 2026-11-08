#!/bin/bash
# (mem1) Refresh the start-up order of an existing profile-guided build and relink it: the
# start-up runs are recorded again on the current executable (gen_order.sh ORDER_REFRESH=1,
# the rest of $CARGO_TARGET_DIR/pgo-startup.ld is kept), then only the link step runs.
# For after a merge that recompiled parts of the PGO build (renamed or re-inlined start-up
# functions each cost a 64-128 KB window of start-up RSS until the order is refreshed).
# usage: tools/pgo/reorder.sh [-j N]   (needs $CARGO_TARGET_DIR/pgo-merged.profdata from build.sh)
set -e
JOBS=2
[ "${1:-}" = "-j" ] && JOBS="$2"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
T="${CARGO_TARGET_DIR:-$ROOT/target}"
TRIPLE=x86_64-unknown-linux-gnu
# keep in sync with tools/pgo/build.sh
BASE="-C link-args=-Wl,--export-dynamic -C target-cpu=x86-64-v3 -Z plt=yes -Z relax-elf-relocations=yes"
PROF="${PGO_PROFILE:-$T/pgo-merged.profdata}"
[ -f "$PROF" ] || { echo "no profile at $PROF (run tools/pgo/build.sh)" >&2; exit 1; }
USE="$BASE -Cprofile-use=$PROF -Cllvm-args=-no-pgo-warn-mismatch"
cd "$ROOT"
[ -f "$T/pgo-startup.ld" ] || cp crates/launcher/startup.ld "$T/pgo-startup.ld"
ORDER_REFRESH=1 bash "$ROOT/tools/bench/perf/gen_order.sh" "$T/pgo/$TRIPLE/release/python3" "$T/pgo-startup.ld"
RPY_ORDER_FILE="$T/pgo-startup.ld" RUSTFLAGS="$USE" cargo build --release -j "$JOBS" --target "$TRIPLE" \
  --target-dir "$T/pgo" -p python
echo "$T/pgo/$TRIPLE/release/python3"
