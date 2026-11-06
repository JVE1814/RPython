#!/bin/bash
# JIT spec smoke: run spec test modules with the JIT off (RPYTHON_JIT=0), with the
# default tiers, and with extra configurations, and list the modules whose result
# differs from the JIT-off reference (only differences the JIT causes matter).
# usage: tools/jit_tests/smoke.sh PY OUTDIR [tests...]
# env: SMOKE_CONFIGS="name=VAR=VAL,VAR=VAL ..." (default: nojit, default, t2 forced on)
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PY="$1"; OUT="$2"; shift 2
[ $# -eq 0 ] && set -- test_bool test_set test_list test_dict test_generators test_coroutines test_descr \
  test_class test_exceptions test_scope test_math test_float test_int test_threading
CONFIGS=${SMOKE_CONFIGS:-"nojit=RPYTHON_JIT=0 default= t2=RPYTHON_JIT2=1"}
for c in $CONFIGS; do
  name=${c%%=*}; envs=${c#*=}
  (
    if [ -n "$envs" ]; then export ${envs//,/ }; fi
    TEST_JOBS=${TEST_JOBS:-2} "$ROOT/tools/run_tests.sh" "$PY" "$OUT/$name" "$@" >/dev/null
  )
done
first=${CONFIGS%% *}; first=${first%%=*}
for c in $CONFIGS; do
  name=${c%%=*}
  echo "== $name"; cat "$OUT/$name/results.tsv"
done
for c in $CONFIGS; do
  name=${c%%=*}; [ "$name" = "$first" ] && continue
  if ! diff <(cut -f1-5 "$OUT/$first/results.tsv") <(cut -f1-5 "$OUT/$name/results.tsv") >/dev/null; then
    echo "== DIFF $first vs $name"
    diff <(cut -f1-5 "$OUT/$first/results.tsv") <(cut -f1-5 "$OUT/$name/results.tsv")
  fi
done
