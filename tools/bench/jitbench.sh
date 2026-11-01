#!/bin/bash
# usage: tools/bench/jitbench.sh [-c] [-l LOOPS] PY OUT.tsv [names]
# Runs pyperformance benchmarks through tools/bench/jitshim (a pyperf stand-in that needs
# only sys/time/os). Default: prints the shim's median seconds per loop (like
# run_bench.sh). -c: whole-process callgrind instruction count with a fixed amount of
# work (BENCH_LOOPS=LOOPS, one run, no warmup). Run the same command for the baselines.
CG=0; LOOPS=1
while [ $# -gt 0 ]; do
  case "$1" in
    -c) CG=1; shift;;
    -l) LOOPS="$2"; shift 2;;
    *) break;;
  esac
done
PY="$1"; OUT="$2"; shift 2
HERE="$(cd "$(dirname "$0")" && pwd)"
BENCHDIR="${PYPERF_BENCHMARKS:-/opt/bench/dl/pyperformance-1.14.0/pyperformance/data-files/benchmarks}"
if [ $# -eq 0 ]; then set -- $(cd "$BENCHDIR" && ls -d bm_* | sed 's/^bm_//'); fi
: > "$OUT"
TAG=$$
for b in "$@"; do
  d="$BENCHDIR/bm_$b"; script="$d/run_benchmark.py"
  [ -f "$script" ] || { printf "%s\tMISSING\n" "$b" | tee -a "$OUT"; continue; }
  err="/tmp/jitbench_err_${b}_$TAG.txt"
  if [ "$CG" = 1 ]; then
    res=$(cd "$d" && PYTHONPATH="$HERE/jitshim" BENCH_LOOPS=$LOOPS BENCH_RUNS=1 BENCH_WARMUPS=0 timeout 3000 \
          valgrind --tool=callgrind --callgrind-out-file=/dev/null "$PY" "$script" 2>"$err" >/dev/null; \
          awk '/Collected/{print $4}' "$err")
  else
    res=$(cd "$d" && PYTHONPATH="$HERE/jitshim" timeout "${BENCH_TIMEOUT:-600}" "$PY" "$script" 2>"$err" | awk '/^BENCH /{print $3}' | head -1)
  fi
  [ -n "$res" ] || res=ERROR
  printf "%s\t%s\n" "$b" "$res" | tee -a "$OUT"
done
