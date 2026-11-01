#!/bin/bash
# End-to-end checks of compiler output through the runtime.
# usage: crates/compiler/e2e/run.sh <python3 executable>
P="$1"; cd "$(dirname "$0")"; fail=0
for f in *.py; do
  if timeout 60 "$P" "$f" 2>&1 | diff -u "${f%.py}.expected" - > /tmp/e2e_$$.diff; then echo "PASS $f"; else echo "FAIL $f"; cat /tmp/e2e_$$.diff; fail=1; fi
done
rm -f /tmp/e2e_$$.diff; exit $fail
