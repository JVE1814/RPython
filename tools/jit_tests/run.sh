#!/bin/bash
# Differential tests for the JIT tiers: every script in this directory must print the
# same output with RPYTHON_JIT=0 (tier 0, reference) and with the JIT compiling every
# code object on its first call (RPYTHON_JIT_THRESHOLD=0) or after a few calls.
# usage: tools/jit_tests/run.sh PY
PY="$1"
HERE="$(cd "$(dirname "$0")" && pwd)"
fail=0
for t in "$HERE"/t_*.py; do
  ref=$(RPYTHON_JIT=0 timeout 120 "$PY" "$t" 2>&1)
  for th in 0 2; do
    out=$(RPYTHON_JIT_THRESHOLD=$th timeout 120 "$PY" "$t" 2>&1)
    if [ "$ref" != "$out" ]; then
      echo "FAIL $(basename "$t") threshold=$th"
      diff <(echo "$ref") <(echo "$out") | head -20
      fail=1
    fi
  done
  echo "ok   $(basename "$t")"
done
exit $fail
