#!/bin/bash
# usage: tools/bench/fixedwork.sh PY BENCH LOOPS [REPEAT]
# Wall time (best of REPEAT) and peak RSS of one benchmark process doing a fixed amount
# of work (BENCH_LOOPS loops, one run, no warmup) through tools/bench/jitshim.
PY="$1"; B="$2"; LOOPS="$3"; REP="${4:-3}"
case "$PY" in /*) ;; */*) PY="$PWD/$PY";; esac
HERE="$(cd "$(dirname "$0")" && pwd)"
D="${PYPERF_BENCHMARKS:-/opt/bench/dl/pyperformance-1.14.0/pyperformance/data-files/benchmarks}/bm_$B"
best=""
for i in $(seq "$REP"); do
  s=$(date +%s%N)
  (cd "$D" && PYTHONPATH="$HERE/jitshim" BENCH_LOOPS="$LOOPS" BENCH_RUNS=1 BENCH_WARMUPS=0 "$PY" run_benchmark.py > /dev/null 2>&1)
  e=$(date +%s%N)
  t=$(( (e - s) / 1000000 ))
  if [ -z "$best" ] || [ "$t" -lt "$best" ]; then best=$t; fi
done
echo "$B $best ms"
