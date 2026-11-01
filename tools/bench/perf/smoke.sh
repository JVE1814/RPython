#!/bin/bash
# perf-runtime spec smoke: tools/bench/perf/smoke.sh PY OUTDIR [tests...]
# Uses tools/run_tests.sh; C test extensions from /tmp/perf-runtime-ext
# (build them with tools/build_test_exts.sh /tmp/perf-runtime-ext).
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
PY="$1"; OUT="$2"; shift 2
[ $# -eq 0 ] && set -- test_bool test_set test_list test_dict test_descr test_class test_weakref test_gc test_copy test_pickle test_threading test_capi
EXTRA_PYTHONPATH=/tmp/perf-runtime-ext TEST_JOBS=${TEST_JOBS:-3} "$ROOT/tools/run_tests.sh" "$PY" "$OUT" "$@"
cat "$OUT/results.tsv"
