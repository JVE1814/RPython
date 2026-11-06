#!/bin/bash
# usage: tools/bench/perloop.sh [-a L1] [-b L2] PY [names]
# Steady-state cost per benchmark loop in callgrind instructions:
#   (I(L2 loops) - I(L1 loops)) / (L2 - L1)
# This removes startup, imports and JIT warm-up from the comparison (pyperformance
# itself measures after warm-up). Run the same command for the baselines.
# Output: name<TAB>instructions per loop (millions)
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
cg() {
  local d="$1" loops="$2" err="/tmp/perloop_$$_$RANDOM.txt"
  (cd "$d" && PYTHONPATH="$HERE/jitshim" BENCH_LOOPS=$loops BENCH_RUNS=1 BENCH_WARMUPS=0 timeout 3000 \
     valgrind --tool=callgrind --callgrind-out-file=/dev/null "$PY" run_benchmark.py 2>"$err" >/dev/null)
  awk '/Collected/{print $4}' "$err"
  rm -f "$err"
}
for b in "$@"; do
  d="$BENCHDIR/bm_$b"
  a=$(cg "$d" "$L1"); c=$(cg "$d" "$L2")
  if [ -z "$a" ] || [ -z "$c" ]; then printf "%s\tERROR\n" "$b"; continue; fi
  awk -v a="$a" -v c="$c" -v l1="$L1" -v l2="$L2" -v n="$b" 'BEGIN{printf "%s\t%.2f\n", n, (c-a)/(l2-l1)/1e6}'
done
