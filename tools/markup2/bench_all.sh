#!/bin/bash
# Like tools/bench/run_bench.sh for one pyperformance benchmark, but prints every
# BENCH line (sub-benchmarks), e.g.: bench_all.sh PY xml_etree
PY="$1"; b="$2"
HERE="$(cd "$(dirname "$0")/../bench" && pwd)"
BENCHDIR="${PYPERF_BENCHMARKS:-/opt/bench/dl/pyperformance-1.14.0/pyperformance/data-files/benchmarks}"
d="$BENCHDIR/bm_$b"
cd "$d" || exit 1
PYTHONPATH="$HERE/shim" BENCH_RUNS="${BENCH_RUNS:-3}" timeout "${BENCH_TIMEOUT:-900}" "$PY" "$d/run_benchmark.py" 2>"/tmp/m2_bench_err_$b.txt" | grep '^BENCH '
