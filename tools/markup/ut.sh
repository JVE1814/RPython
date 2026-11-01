#!/bin/bash
# Run a test module with unittest directly: ut.sh test.test_email.test_email [-k pattern]
# Prints the summary plus the names of failing tests.
W=$(cd "$(dirname "$0")/../.." && pwd)
PY=${PY:-/home/user/RPython/target/markup/release/python3}
cd /tmp
if [ -d /tmp/mklib ] && [ -z "$NOOVERLAY" ]; then export RPYTHON_STDLIB=/tmp/mklib; fi
mod=$1; shift
PYTHONPATH=$W/spec/Lib timeout ${TMO:-600} "$PY" -m unittest "$mod" "$@" > /tmp/mk/ut.out 2>&1
grep -E "^(FAIL|ERROR):" /tmp/mk/ut.out | sort | uniq -c | sort -rn | head -${TOP:-60}
tail -3 /tmp/mk/ut.out
