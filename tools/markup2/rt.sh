#!/bin/bash
# Run spec test modules with regrtest: rt.sh test_a [test_b ...]  (extra regrtest args via RTARGS)
# Output of each module goes to /tmp/m2_<test>.out.
W=$(cd "$(dirname "$0")/../.." && pwd)
PY=${PY:-/home/user/RPython/target/markup2/release/python3}
cd /tmp || exit 1
for t in "$@"; do
  PYTHONPATH=$W/spec/Lib timeout "${TMO:-900}" "$PY" -m test $RTARGS "$t" > "/tmp/m2_$t.out" 2>&1
  echo "$t exit=$? $(grep -E '^(Total tests|Result):' "/tmp/m2_$t.out" | tr '\n' ' ')"
done
