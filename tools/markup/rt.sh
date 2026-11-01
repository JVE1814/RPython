#!/bin/bash
# Run spec test modules with the markup build: rt.sh [-v] test_a test_b ...
W=$(cd "$(dirname "$0")/../.." && pwd)
PY=${PY:-/home/user/RPython/target/markup/release/python3}
TAIL=${TAIL:-6}
verbose=
if [ "$1" = "-v" ]; then verbose=-v; shift; fi
cd /tmp
if [ -d /tmp/mklib ] && [ -z "$NOOVERLAY" ]; then export RPYTHON_STDLIB=/tmp/mklib; fi
for t in "$@"; do
  echo "=== $t"
  if [ -n "$verbose" ]; then
    PYTHONPATH=$W/spec/Lib timeout ${TMO:-600} "$PY" -m test -v "$t" 2>&1 | grep -E "(FAIL|ERROR|Error|error|Traceback|skipped)" | head -${TAIL}
  else
    PYTHONPATH=$W/spec/Lib timeout ${TMO:-600} "$PY" -m test "$t" 2>&1 | tail -${TAIL}
  fi
done
