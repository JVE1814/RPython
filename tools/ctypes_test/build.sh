#!/bin/bash
# Build the _ctypes_test helper used by spec/Lib/test/test_ctypes.
#   tools/ctypes_test/build.sh [OUTDIR] [PYTHON]
# OUTDIR defaults to <repo>/build/ext (put it on PYTHONPATH / EXTRA_PYTHONPATH, like the
# other test extensions built by tools/build_test_exts.sh).
# _ctypes_test_generated.c.h is produced by the spec's own generator
# (spec/Lib/test/test_ctypes/test_generated_structs.py run as a script with OUR python,
# which needs a first build of the module); the committed copy is used when no python
# is given.
set -eu
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
HERE="$ROOT/tools/ctypes_test"
OUT="${1:-$ROOT/build/ext}"
PY="${2:-}"
CC="${CC:-gcc}"
SO="$OUT/_ctypes_test.cpython-315t-x86_64-linux-gnu.so"
mkdir -p "$OUT"
INC="$(mktemp -d)"; trap 'rm -rf "$INC"' EXIT
cp "$ROOT/tools/pyconfig.h" "$INC/pyconfig.h"
if [ ! -f "$HERE/_ctypes_test_generated.c.h" ]; then
  # bootstrap stub
  printf '%s\n' 'static PyObject *get_generated_test_data(PyObject *self, PyObject *name)' \
    '{ PyErr_SetString(PyExc_NotImplementedError, "generated data missing"); return NULL; }' \
    > "$INC/_ctypes_test_generated.c.h"
fi
build() {
  "$CC" -shared -fPIC -O1 -g -std=gnu11 -D_GNU_SOURCE -I"$INC" -I"$ROOT/spec/Include" -I"$HERE" \
    "$HERE/_ctypes_test.c" -o "$SO" -lm
}
build
if [ -n "$PY" ]; then
  (cd /tmp && PYTHONPATH="$ROOT/spec/Lib:$OUT" "$PY" "$ROOT/spec/Lib/test/test_ctypes/test_generated_structs.py") \
    > "$HERE/_ctypes_test_generated.c.h.new"
  mv "$HERE/_ctypes_test_generated.c.h.new" "$HERE/_ctypes_test_generated.c.h"
  rm -f "$INC/_ctypes_test_generated.c.h"
  build
fi
echo "built $SO"
