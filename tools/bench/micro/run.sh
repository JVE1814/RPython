#!/bin/bash
# usage: tools/bench/micro/run.sh [-n RUNS] [-c] PY [bench names...]
#   best-of-RUNS wall time (ms) per micro benchmark; -c: callgrind instruction count instead.
# Output: name<TAB>value
RUNS=5; CG=0
while [ $# -gt 0 ]; do
  case "$1" in
    -n) RUNS="$2"; shift 2;;
    -c) CG=1; shift;;
    *) break;;
  esac
done
PY="$1"; shift
HERE="$(cd "$(dirname "$0")" && pwd)"
if [ $# -eq 0 ]; then set -- $(cd "$HERE" && ls *.py | sed 's/\.py$//'); fi
for b in "$@"; do
  if [ "$CG" = 1 ]; then
    case "$b" in fib) n=20;; loop|forrange) n=300000;; *) n=100000;; esac
    out=$(valgrind --tool=callgrind --callgrind-out-file=/dev/null "$PY" "$HERE/$b.py" $n 2>&1 >/dev/null | awk '/Collected/{print $4}')
    printf "%s\t%s\n" "$b" "$out"
  else
    best=""
    for i in $(seq "$RUNS"); do
      s=$(date +%s%N)
      "$PY" "$HERE/$b.py" > /dev/null || { best=ERROR; break; }
      e=$(date +%s%N)
      t=$(( (e - s) / 1000000 ))
      if [ -z "$best" ] || [ "$t" -lt "$best" ]; then best=$t; fi
    done
    printf "%s\t%s\n" "$b" "$best"
  fi
done
