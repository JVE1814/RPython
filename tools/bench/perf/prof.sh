#!/bin/bash
# usage: tools/bench/perf/prof.sh [-i] [-n N] PY args...   callgrind profile, top N functions
# (-i: inclusive costs). Output file: /tmp/perf-runtime-cg.out
INCL=no; N=30
while [ $# -gt 0 ]; do
  case "$1" in
    -i) INCL=yes; shift;;
    -n) N="$2"; shift 2;;
    *) break;;
  esac
done
cd /tmp || exit 1
valgrind --tool=callgrind --callgrind-out-file=/tmp/perf-runtime-cg.out "$@" 2>&1 >/dev/null | grep -E "Collected|refs"
callgrind_annotate --inclusive=$INCL /tmp/perf-runtime-cg.out 2>/dev/null | grep -E "^ *[0-9,]+ " | head -"$N" | sed -e 's#/home/user/RPython/.claude/worktrees/[^/]*/crates/runtime/src/##' -e 's#\[/home[^]]*\]##' | cut -c1-200
