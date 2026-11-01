#!/bin/bash
# usage: tools/bench/run_bench.sh <python> <out.tsv> [benchmark names (without bm_)]
# Runs pyperformance benchmarks with the pyperf shim. Output: name<TAB>seconds|ERROR
PY="$1"; OUT="$2"; shift 2
HERE="$(cd "$(dirname "$0")" && pwd)"
BENCHDIR="${PYPERF_BENCHMARKS:-/opt/bench/dl/pyperformance-1.14.0/pyperformance/data-files/benchmarks}"
if [ $# -eq 0 ]; then set -- $(cd "$BENCHDIR" && ls -d bm_* | sed 's/^bm_//'); fi
: > "$OUT"
for b in "$@"; do
  d="$BENCHDIR/bm_$b"; script="$d/run_benchmark.py"
  [ -f "$script" ] || { printf "%s\tMISSING\n" "$b" >> "$OUT"; continue; }
  res=$(cd "$d" && PYTHONPATH="$HERE/shim${BENCH_EXTRA_PATH:+:$BENCH_EXTRA_PATH}" timeout "${BENCH_TIMEOUT:-600}" "$PY" "$script" 2>"/tmp/bench_err_$b.txt" | awk '/^BENCH /{print $3}' | head -1)
  if [ -z "$res" ]; then res="ERROR"; fi
  printf "%s\t%s\n" "$b" "$res" | tee -a "$OUT"
done
