#!/bin/bash
# Training workload for the profile-guided build (tools/pgo/build.sh); perf7.
# usage: tools/pgo/train.sh <snapshot-dir>   (an installed-layout snapshot of the instrumented
#        build; LLVM_PROFILE_FILE must already point where the raw profiles go)
# The mix: start-up (with and without site), the micro benchmarks of tools/bench/micro, the
# pyperformance benchmarks that need no third-party package (calibrated, ~1 s each, enough for the
# JIT tiers to compile and run), and a set of regression-test modules for the breadth of the
# runtime (imports, str/bytes/dict/set operations, exceptions, generators, regexes, json,
# pickle, XML, decimal, threads). Failures are ignored: only the profile counts.
SNAP="$1"
PY="$SNAP/bin/python3"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BENCHDIR="${PYPERF_BENCHMARKS:-/opt/bench/dl/pyperformance-1.14.0/pyperformance/data-files/benchmarks}"
T="${PGO_TIMEOUT:-300}"
cd /tmp || exit 1
for i in 1 2 3; do "$PY" -c pass; "$PY" -S -c pass; "$PY" -I -c 'import os, re, json, collections'; done
for m in attr:200000 exccycle:50000 fib:22 floatops:200000 forrange:500000 gen:200000 globals_:200000 \
         listdict:200000 loop:500000 method:200000 unpack:200000; do
  timeout "$T" "$PY" "$ROOT/tools/bench/micro/${m%%:*}.py" "${m##*:}" >/dev/null 2>&1
done
for d in "$BENCHDIR"/bm_*; do
  b="${d##*/bm_}"
  case "$b" in
    # need third-party packages, network services or many minutes
    2to3|chameleon|coverage|dask|django_template|docutils|dulwich_log|fastapi|genshi|hg_startup|html5lib|mako|networkx|sphinx|sqlalchemy_*|sqlglot*|sympy|tornado_http|xdsl|yaml|asyncio_websockets|tomli_loads|pyflate) continue;;
  esac
  [ -f "$d/run_benchmark.py" ] || continue
  # calibrated loops (about BENCH_MIN_TIME seconds per measured run), so that every
  # benchmark weighs roughly the same in the profile whatever its loop takes
  (cd "$d" && PYTHONPATH="$ROOT/tools/bench/jitshim" BENCH_MIN_TIME="${PGO_MIN_TIME:-0.2}" BENCH_RUNS=1 BENCH_WARMUPS=1 \
     timeout "$T" "$PY" run_benchmark.py >/dev/null 2>&1)
done
TESTS="test_dict test_set test_list test_tuple test_str test_bytes test_unicode_identifiers test_int test_long
test_float test_exceptions test_generators test_coroutines test_class test_descr test_re test_json test_pickle
test_xml_etree test_decimal test_functools test_itertools test_collections test_operator test_math test_struct
test_string test_format test_fstring test_scope test_with test_contextlib test_dataclasses test_enum test_typing
test_copy test_sort test_bisect test_heapq test_statistics test_fractions test_datetime test_base64 test_codecs
test_textwrap test_difflib test_pprint test_logging test_pathlib test_threading test_queue test_weakref test_gc"
for t in $TESTS; do
  PYTHONPATH="$ROOT/spec/Lib" timeout "$T" "$PY" -m test --timeout=0 -q "$t" >/dev/null 2>&1
done
exit 0
