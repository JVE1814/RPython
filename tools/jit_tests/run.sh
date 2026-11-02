#!/bin/bash
# Differential tests for the JIT tiers: every script in this directory must print the
# same output with RPYTHON_JIT=0 (tier 0, reference) and with the JIT compiling every
# code object on its first call (RPYTHON_JIT_THRESHOLD=0) or after a few calls, with
# tier 2 compiling early (RPYTHON_JIT2_THRESHOLD) or disabled (RPYTHON_JIT2=0).
# usage: tools/jit_tests/run.sh PY [test names]
PY="$1"; shift
HERE="$(cd "$(dirname "$0")" && pwd)"
CONFIGS=(
  "RPYTHON_JIT_THRESHOLD=0 RPYTHON_JIT2=0"
  "RPYTHON_JIT_THRESHOLD=2 RPYTHON_JIT2=0"
  "RPYTHON_JIT_THRESHOLD=50 RPYTHON_JIT2=0"
  "RPYTHON_JIT_THRESHOLD=0 RPYTHON_JIT2_THRESHOLD=1"
  "RPYTHON_JIT_THRESHOLD=2 RPYTHON_JIT2_THRESHOLD=5"
  "RPYTHON_JIT_THRESHOLD=2 RPYTHON_JIT2_THRESHOLD=50"
)
if [ $# -eq 0 ]; then set -- "$HERE"/t_*.py; else set -- $(for t in "$@"; do echo "$HERE/$t"; done); fi
fail=0
for t in "$@"; do
  ref=$(RPYTHON_JIT=0 timeout 120 "$PY" "$t" 2>&1)
  ok=1
  for cfg in "${CONFIGS[@]}"; do
    out=$(env $cfg timeout 120 "$PY" "$t" 2>&1)
    if [ "$ref" != "$out" ]; then
      echo "FAIL $(basename "$t") $cfg"
      diff <(echo "$ref") <(echo "$out") | head -20
      fail=1; ok=0
    fi
  done
  [ $ok = 1 ] && echo "ok   $(basename "$t")"
done
exit $fail
