#!/bin/bash
# Show the traceback tail of a failing test from /tmp/mk/utv.out: show.sh name [lines]
awk -v pat="$1" 'index($0, pat) && /^(FAIL|ERROR):/ {p=1; n=0} p {print; n++; if (n > 3 && /^(FAIL|ERROR):|^------/ && n>5) {p=0}}' /tmp/mk/utv.out \
  | grep -v '^\s*[~^]*\s*$' | grep -v '^    \S.*[~^]$' | head -${2:-30}
