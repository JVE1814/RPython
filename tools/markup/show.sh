#!/bin/bash
# Show traceback tails of failing tests from /tmp/mk/utv.out:
#   N=lines show.sh name1 name2 ...
for pat in "$@"; do
awk -v pat="$pat" 'index($0, pat) && /^(FAIL|ERROR):/ {p=1; n=0} p {print; n++; if (n > 5 && /^(FAIL|ERROR):|^------|^======/) {p=0}}' /tmp/mk/utv.out \
  | grep -v '^\s*[~^]*\s*$' | grep -v '^    \S.*[~^]$' | grep -v '^  File "/home/user/core' | head -${N:-30}
done
