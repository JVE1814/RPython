#!/bin/bash
# Verbose unittest run: ut.sh test.test_mod[.Class[.test]] ...  -> /tmp/m2_ut.out, prints a summary
W=$(cd "$(dirname "$0")/../.." && pwd)
PY=${PY:-/home/user/RPython/target/markup2/release/python3}
cd /tmp || exit 1
PYTHONPATH=$W/spec/Lib timeout "${TMO:-600}" "$PY" -m unittest -v "$@" > /tmp/m2_ut.out 2>&1
echo "exit=$?"
grep -cE "\.\.\. ok$" /tmp/m2_ut.out
grep -E "^(FAIL|ERROR):" /tmp/m2_ut.out | sort | uniq -c | sort -rn | head -"${TOP:-60}"
tail -3 /tmp/m2_ut.out
