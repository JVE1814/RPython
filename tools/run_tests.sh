#!/bin/bash
# Run the regression suite one test module at a time and record results.
# usage: tools/run_tests.sh <python-executable> <out-dir> [test names...]
# With no test names, runs every test module in spec/Lib/test.
# Output: <out-dir>/results.tsv with columns: test status run failures errors skipped seconds
set -u
PY="$1"; OUT="$2"; shift 2
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TESTDIR="$ROOT/spec/Lib/test"
mkdir -p "$OUT/logs"
TIMEOUT="${TEST_TIMEOUT:-900}"
JOBS="${TEST_JOBS:-2}"
if [ $# -eq 0 ]; then
  set -- $(cd "$TESTDIR" && ls -d test_* | sed 's/\.py$//' | sort -u)
fi
run_one() {
  local t="$1"
  local log="$OUT/logs/$t.log"
  local start=$(date +%s)
  # the spec's Lib dir provides the `test` package; our stdlib supplies the rest
  ( cd /tmp && PYTHONPATH="$ROOT/spec/Lib${EXTRA_PYTHONPATH:+:$EXTRA_PYTHONPATH}" timeout -k 10 "$TIMEOUT" \
      "$PY" -m test --timeout=0 "$t" > "$log" 2>&1 )
  local rc=$?
  local secs=$(( $(date +%s) - start ))
  local status
  case $rc in
    0) status=PASS ;;
    124|137) status=TIMEOUT ;;
    139|134|132) status=CRASH ;;
    *) status=FAIL ;;
  esac
  # "Total tests: run=1,234 failures=2 skipped=10" (regrtest summary)
  local line=$(grep -E "^Total tests: run=" "$log" | tail -1 | tr -d ',')
  local run=$(echo "$line" | sed -nE 's/.*run=([0-9]+).*/\1/p')
  local fails=$(echo "$line" | sed -nE 's/.*failures=([0-9]+).*/\1/p')
  local skipped=$(echo "$line" | sed -nE 's/.*skipped=([0-9]+).*/\1/p')
  if grep -qE "^(Result: SUCCESS|Tests result: SUCCESS)" "$log" && [ $rc -ne 0 ]; then status=PASS; fi
  if grep -qE "test_[a-z0-9_]+ skipped" "$log" && [ -z "$run" ]; then status=SKIP; fi
  printf "%s\t%s\t%s\t%s\t%s\t%s\n" "$t" "$status" "${run:-0}" "${fails:-0}" "${skipped:-0}" "$secs"
}
export -f run_one
export PY OUT ROOT TIMEOUT EXTRA_PYTHONPATH
printf "%s\n" "$@" | xargs -P "$JOBS" -I{} bash -c 'run_one {}' > "$OUT/results.tsv.tmp"
sort "$OUT/results.tsv.tmp" > "$OUT/results.tsv"; rm -f "$OUT/results.tsv.tmp"
awk -F'\t' '{s[$2]++; run+=$3; f+=$4; sk+=$5} END {for (k in s) printf "%s=%d ", k, s[k]; printf "\ntests run=%d failures=%d skipped=%d\n", run, f, sk}' "$OUT/results.tsv" | tee "$OUT/summary.txt"
