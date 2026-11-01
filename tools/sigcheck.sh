#!/bin/bash
# List builtin callables lacking an introspectable signature, using our own
# interpreter.  Usage: tools/sigcheck.sh [PY] [sigcheck.py args...]
# PY defaults to $CARGO_TARGET_DIR/debug/python3 (or target/docsig).
set -e
here="$(cd "$(dirname "$0")" && pwd)"
py="${1:-}"
if [ -n "$py" ] && [ -x "$py" ] && [ ! -d "$py" ]; then
  shift
else
  py="${CARGO_TARGET_DIR:-/home/user/RPython/target/docsig}/debug/python3"
fi
cd /tmp
exec "$py" "$here/sigcheck.py" "$@"
