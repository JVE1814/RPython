#!/bin/bash
# Run unittest on spec test modules with the pkgtools build:
#   tools/pkgtools_ut.sh [-v] test.test_importlib.resources.test_files ...
# An argument naming a test package directory (e.g. test_importlib/resources/)
# expands to all test_*.py modules in it.
WT="$(cd "$(dirname "$0")/.." && pwd)"
PY="${PY:-/home/user/RPython/target/pkgtools/release/python3}"
args=()
for a in "$@"; do
  if [[ "$a" == */ ]]; then
    for f in "$WT/spec/Lib/test/$a"test_*.py; do
      m="${f#$WT/spec/Lib/}"; m="${m%.py}"; args+=("${m//\//.}")
    done
  else
    args+=("$a")
  fi
done
cd /tmp || exit 1
PYTHONPATH="$WT/spec/Lib${EXTRA:+:$EXTRA}" exec "$PY" -m unittest "${args[@]}"
