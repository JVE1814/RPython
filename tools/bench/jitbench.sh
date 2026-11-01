#!/bin/bash
# usage: tools/bench/jitbench.sh [-n RUNS] [-c] [-l LOOPS] PY OUT.tsv [names]
# Whole-process measurement of pyperformance benchmarks through tools/bench/jitshim
# (fixed loop count, no timing inside the process): best-of-RUNS wall ms, or with -c
# the callgrind instruction count. Used by the jit workstream before the stdlib can
# run the real pyperf shim; run the same command for the baseline interpreters.
RUNS=3; CG=0; LOOPS=1
while [ $# -gt 0 ]; do
  case "$1" in
    -n) RUNS="$2"; shift 2;;
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
for b in "$@"; do
  d="$BENCHDIR/bm_$b"; script="$d/run_benchmark.py"
  [ -f "$script" ] || { printf "%s\tMISSING\n" "$b" | tee -a "$OUT"; continue; }
  if [ "$CG" = 1 ]; then
    res=$(cd "$d" && PYTHONPATH="$HERE/jitshim" timeout 3000 valgrind --tool=callgrind --callgrind-out-file=/dev/null \
          "$PY" "$script" --jitshim-loops=$LOOPS 2>&1 >/dev/null | awk '/Collected/{print $4}')
    [ -n "$res" ] || res=ERROR
  else
    res=""
    for i in $(seq "$RUNS"); do
      s=$(date +%s%N)
      if ! (cd "$d" && PYTHONPATH="$HERE/jitshim" timeout "${BENCH_TIMEOUT:-600}" "$PY" "$script" --jitshim-loops=$LOOPS > /dev/null 2>"/tmp/jitbench_err_$b.txt"); then
        res=ERROR; break
      fi
      e=$(date +%s%N)
      t=$(( (e - s) / 1000000 ))
      if [ -z "$res" ] || [ "$t" -lt "$res" ]; then res=$t; fi
    done
  fi
  printf "%s\t%s\n" "$b" "$res" | tee -a "$OUT"
done
