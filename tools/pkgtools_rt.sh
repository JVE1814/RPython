#!/bin/bash
# Run spec tests with the pkgtools build: tools/pkgtools_rt.sh [regrtest args...]
WT="$(cd "$(dirname "$0")/.." && pwd)"
PY="${PY:-/home/user/RPython/target/pkgtools/release/python3}"
cd /tmp || exit 1
PYTHONPATH="$WT/spec/Lib${EXTRA:+:$EXTRA}" exec "$PY" -m test "$@"
