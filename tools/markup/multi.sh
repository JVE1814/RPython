#!/bin/bash
# Summaries for several unittest modules: multi.sh prefix mod1 mod2 ...
D=$(dirname "$0")
prefix=$1; shift
for m in "$@"; do
  echo "== $m"
  TOP=${TOP:-25} bash "$D/utv.sh" "$prefix$m" 2>&1 | tail -${LINES_PER:-28} | cut -c1-220
done
