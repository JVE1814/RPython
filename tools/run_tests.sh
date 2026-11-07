#!/bin/bash
# Run the regression suite one test module at a time and record results.
# usage: tools/run_tests.sh <python-executable> <out-dir> [test names...]
#        tools/run_tests.sh --rescan <out-dir>   (recompute the errors column from existing logs)
# With no test names, runs every test module in spec/Lib/test.
# Output: <out-dir>/results.tsv with columns: test status run failures skipped seconds errors
# (errors = tests that raised instead of failing an assertion; regrtest's "Total tests" line
# does not count them, so they are read from the per-file "failed (N errors, M failures)" line)
set -u
count_errors() {
  grep -oE "failed \([^)]*\)" "$1" | grep -oE "[0-9]+ errors?" | awk '{s+=$1} END{print s+0}'
}
summarize() {
  awk -F'\t' '{s[$2]++; run+=$3; f+=$4; sk+=$5; e+=$7} END {for (k in s) printf "%s=%d ", k, s[k]; printf "\ntests run=%d failures=%d errors=%d skipped=%d\n", run, f, e, sk}' "$1/results.tsv" | tee "$1/summary.txt"
}
if [ "${1:-}" = "--rescan" ]; then
  OUT="$2"
  while IFS=$'\t' read -r t st run f sk secs rest; do
    printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\n" "$t" "$st" "$run" "$f" "$sk" "$secs" "$(count_errors "$OUT/logs/$t.log")"
  done < "$OUT/results.tsv" > "$OUT/results.tsv.tmp" && mv "$OUT/results.tsv.tmp" "$OUT/results.tsv"
  summarize "$OUT"
  exit 0
fi
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
  printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\n" "$t" "$status" "${run:-0}" "${fails:-0}" "${skipped:-0}" "$secs" "$(count_errors "$log")"
}
export -f run_one count_errors
export PY OUT ROOT TIMEOUT EXTRA_PYTHONPATH
printf "%s\n" "$@" | xargs -P "$JOBS" -I{} bash -c 'run_one {}' > "$OUT/results.tsv.tmp"
sort "$OUT/results.tsv.tmp" > "$OUT/results.tsv"; rm -f "$OUT/results.tsv.tmp"
summarize "$OUT"
