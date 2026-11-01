#!/bin/bash
# Verbose unittest run with timeout into /tmp/mk/utv.out: utv.sh module [args]
W=$(cd "$(dirname "$0")/../.." && pwd)
PY=${PY:-/home/user/RPython/target/markup/release/python3}
cd /tmp
if [ -d /tmp/mklib ] && [ -z "$NOOVERLAY" ]; then export RPYTHON_STDLIB=/tmp/mklib; fi
mod=$1; shift
PYTHONPATH=$W/spec/Lib timeout ${TMO:-100} "$PY" -m unittest -v "$mod" "$@" > /tmp/mk/utv.out 2>&1
echo "exit=$?"
grep -c "ok$" /tmp/mk/utv.out
grep -E "^(FAIL|ERROR):" /tmp/mk/utv.out | sort | uniq -c | sort -rn | head -${TOP:-60}
tail -3 /tmp/mk/utv.out
