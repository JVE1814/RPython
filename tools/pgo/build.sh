#!/bin/bash
# Profile-guided release build of python3 (perf7).
# usage: tools/pgo/build.sh [-j N]
# Steps: (1) instrumented build (-Cprofile-generate) in $CARGO_TARGET_DIR/pgo-gen,
# (2) installed-layout snapshot of it and the training workload (tools/pgo/train.sh),
# (3) llvm-profdata merge, (4) optimized build (-Cprofile-use) in $CARGO_TARGET_DIR/pgo.
# Result: $CARGO_TARGET_DIR/pgo/x86_64-unknown-linux-gnu/release/python3 (snapshot it with
# tools/snapshot.sh like any other build). Set PGO_PROFILE=<file.profdata> to skip steps 1-3
# and reuse a merged profile (stale functions simply get no profile data).
# Why: the interpreter, the call paths and the JIT helpers are large; without a profile the
# hot paths of the runtime share cache lines with their error handling and cold functions
# (typing_runtime_protocols missed the 64 KB L1 instruction cache 8x as often as CPython
# while executing about as many instructions; see notes/perf7.md).
# --target is explicit so that RUSTFLAGS (which must repeat .cargo/config.toml's target
# rustflags: an environment RUSTFLAGS replaces them) do not apply to build scripts.
set -e
JOBS=2
[ "${1:-}" = "-j" ] && JOBS="$2"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
T="${CARGO_TARGET_DIR:-$ROOT/target}"
TRIPLE=x86_64-unknown-linux-gnu
# keep in sync with .cargo/config.toml [target.x86_64-unknown-linux-gnu] rustflags
BASE="-C link-args=-Wl,--export-dynamic -C target-cpu=x86-64-v3 -Z plt=yes -Z relax-elf-relocations=yes"
SYSROOT="$(rustc --print sysroot)"
PROFDATA="$SYSROOT/lib/rustlib/$TRIPLE/bin/llvm-profdata"
[ -x "$PROFDATA" ] || { echo "llvm-profdata missing: rustup component add llvm-tools" >&2; exit 1; }
cd "$ROOT"
PROF="${PGO_PROFILE:-}"
if [ -z "$PROF" ]; then
  RAW="$T/pgo-raw"
  rm -rf "$RAW"; mkdir -p "$RAW"
  echo "== instrumented build"
  RUSTFLAGS="$BASE -Cprofile-generate=$RAW" cargo build --release -j "$JOBS" --target "$TRIPLE" \
    --target-dir "$T/pgo-gen" -p python
  echo "== training"
  bash "$ROOT/tools/snapshot.sh" "$T/pgo-gen/$TRIPLE/release/python3" "$ROOT" "$T/pgo-gen/snap" >/dev/null
  LLVM_PROFILE_FILE="$RAW/%m_%p.profraw" bash "$ROOT/tools/pgo/train.sh" "$T/pgo-gen/snap"
  PROF="$T/pgo-merged.profdata"
  "$PROFDATA" merge -o "$PROF" "$RAW"
  rm -rf "$RAW"
fi
echo "== optimized build with $PROF"
USE="$BASE -Cprofile-use=$PROF -Cllvm-args=-no-pgo-warn-mismatch"
# (start-up order of the checked-in list first; step 5 replaces it)
RUSTFLAGS="$USE" cargo build --release -j "$JOBS" --target "$TRIPLE" --target-dir "$T/pgo" -p python
# (5) the profile changes inlining, so the start-up functions of this executable are not
# the ones crates/launcher/startup.ld names: list them from this executable and relink
# (only the launcher's link step runs again)
if [ -z "${PGO_NO_ORDER:-}" ] && command -v valgrind >/dev/null; then
  echo "== start-up order of the optimized executable"
  bash "$ROOT/tools/bench/perf/gen_order.sh" "$T/pgo/$TRIPLE/release/python3" "$T/pgo-startup.ld"
  RPY_ORDER_FILE="$T/pgo-startup.ld" RUSTFLAGS="$USE" cargo build --release -j "$JOBS" --target "$TRIPLE" \
    --target-dir "$T/pgo" -p python
fi
echo "$T/pgo/$TRIPLE/release/python3"
