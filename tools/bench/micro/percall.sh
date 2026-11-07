#!/bin/bash
# usage: tools/bench/micro/percall.sh [-a N1] [-b N2] PY SCRIPT KIND...
# Callgrind instructions per operation of a micro script that takes `N KIND` arguments
# (e.g. calls.py): (I(N2) - I(N1)) / (N2 - N1), which removes startup and warm-up.
# Output: kind<TAB>instructions per operation
N1=20000; N2=60000
while [ $# -gt 0 ]; do
  case "$1" in
    -a) N1="$2"; shift 2;;
    -b) N2="$2"; shift 2;;
    *) break;;
  esac
done
PY="$1"; SCRIPT="$2"; shift 2
HERE="$(cd "$(dirname "$0")" && pwd)"
case "$SCRIPT" in /*) ;; *) SCRIPT="$HERE/$SCRIPT";; esac
cg() {
  valgrind --tool=callgrind --callgrind-out-file=/dev/null "$PY" "$SCRIPT" "$1" "$2" 2>&1 >/dev/null | awk '/Collected/{print $4}'
}
for k in "$@"; do
  a=$(cg "$N1" "$k"); c=$(cg "$N2" "$k")
  if [ -z "$a" ] || [ -z "$c" ]; then printf "%s\tERROR\n" "$k"; continue; fi
  awk -v a="$a" -v c="$c" -v n1="$N1" -v n2="$N2" -v k="$k" 'BEGIN{printf "%s\t%.0f\n", k, (c-a)/(n2-n1)}'
done
