#!/bin/bash
# usage: tools/bench/cachestat.sh [-a L1] [-b L2] PY [names]
# Like perloop.sh, but with cachegrind's cache and branch simulation: steady-state cost per
# benchmark loop of instructions, L1 instruction misses, L1 data misses, last-level misses
# and mispredicted branches, (X(L2 loops) - X(L1 loops)) / (L2 - L1).
# The machine has no hardware counters (no PMU in the VM), so this is the way to tell why
# two runs with similar instruction counts differ in wall time. Run it for the baselines too.
# Output: name, then one line per event: event<TAB>per loop
L1=1; L2=3
while [ $# -gt 0 ]; do
  case "$1" in
    -a) L1="$2"; shift 2;;
    -b) L2="$2"; shift 2;;
    *) break;;
  esac
done
PY="$1"; shift
case "$PY" in /*) ;; */*) PY="$PWD/$PY";; esac
HERE="$(cd "$(dirname "$0")" && pwd)"
BENCHDIR="${PYPERF_BENCHMARKS:-/opt/bench/dl/pyperformance-1.14.0/pyperformance/data-files/benchmarks}"
TMPD="$(mktemp -d)"
cgr() {
  local d="$1" loops="$2" out="$TMPD/cg.$2"
  (cd "$d" && PYTHONPATH="$HERE/jitshim" BENCH_LOOPS=$loops BENCH_RUNS=1 BENCH_WARMUPS=0 timeout 3000 \
     valgrind --tool=cachegrind --cache-sim=yes --branch-sim=yes --cachegrind-out-file="$out" \
     "$PY" run_benchmark.py >/dev/null 2>&1)
  # summary line: Ir I1mr ILmr Dr D1mr DLmr Dw D1mw DLmw Bc Bcm Bi Bim
  awk '/^summary:/{for(i=2;i<=NF;i++) printf "%s ", $i; print ""}' "$out"
}
for b in "$@"; do
  d="$BENCHDIR/bm_$b"
  a=$(cgr "$d" "$L1"); c=$(cgr "$d" "$L2")
  if [ -z "$a" ] || [ -z "$c" ]; then printf "%s\tERROR\n" "$b"; continue; fi
  echo "$b"
  awk -v A="$a" -v C="$c" -v l1="$L1" -v l2="$L2" 'BEGIN{
    split(A, x, " "); split(C, y, " ")
    n = split("Ir I1mr ILmr Dr D1mr DLmr Dw D1mw DLmw Bc Bcm Bi Bim", nm, " ")
    for (i = 1; i <= n; i++) printf "  %s\t%.0f\n", nm[i], (y[i]-x[i])/(l2-l1)
    printf "  D1miss\t%.0f\n  LLmiss\t%.0f\n  Bmisp\t%.0f\n", ((y[5]+y[8])-(x[5]+x[8]))/(l2-l1), ((y[3]+y[6]+y[9])-(x[3]+x[6]+x[9]))/(l2-l1), ((y[11]+y[13])-(x[11]+x[13]))/(l2-l1)
  }'
done
rm -rf "$TMPD"
