#!/bin/bash
# Multi-threaded stress tests for the runtime (stability workstream).
# usage: tools/stress/run.sh PYTHON [script.py ...]
# Each script runs with a timeout (STRESS_TIMEOUT, default 60 s) and must print
# a line starting with "ok". Exit status: number of failed scripts.
PY="$1"; shift
DIR="$(cd "$(dirname "$0")" && pwd)"
T="${STRESS_TIMEOUT:-60}"
if [ $# -eq 0 ]; then
  set -- "$DIR"/*.py
fi
fail=0
for s in "$@"; do
  case "$s" in /*) ;; *) s="$DIR/$s" ;; esac
  name=$(basename "$s")
  start=$(date +%s)
  out=$(cd /tmp && timeout -k 5 "$T" "$PY" "$s" 2>&1)
  rc=$?
  secs=$(( $(date +%s) - start ))
  if [ $rc -eq 0 ] && echo "$out" | grep -q '^ok'; then
    echo "PASS $name (${secs}s)"
  else
    case $rc in 124|137) why=TIMEOUT ;; 134) why=ABORT ;; 139) why=SEGV ;; *) why="rc=$rc" ;; esac
    echo "FAIL $name ($why, ${secs}s)"
    echo "$out" | tail -15 | sed 's/^/    /'
    fail=$((fail + 1))
  fi
done
exit $fail
