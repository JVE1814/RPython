#!/bin/bash
# perf-runtime quick check: semantics tests + a few instruction counts.
# usage: tools/bench/perf/quick.sh PY
PY="$1"
HERE="$(cd "$(dirname "$0")" && pwd)"
cd /tmp || exit 1
"$PY" -c pass   # warm the bytecode caches
echo "t_shapes: $("$PY" "$HERE/t_shapes.py") / jit0: $(RPYTHON_JIT=0 "$PY" "$HERE/t_shapes.py") / thr0: $(RPYTHON_JIT_THRESHOLD=0 "$PY" "$HERE/t_shapes.py")"
echo "t_threads: $("$PY" "$HERE/t_threads.py")"
cg() { valgrind --tool=callgrind --callgrind-out-file=/dev/null "$@" 2>&1 >/dev/null | awk '/Collected/{print $4}'; }
echo "pass $(cg "$PY" -c pass)"
echo "fib25 $(cg "$PY" "$HERE/../micro/fib.py" 25)"
echo "objs50k $(cg "$PY" "$HERE/objs_small.py" 50000)"
"$PY" "$HERE/objs.py"
