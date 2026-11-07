#!/bin/bash
# Build the C test helper extensions from spec/Modules against spec/Include + tools/pyconfig.h.
#   tools/build_test_exts.sh [OUTDIR] [module...]      (default OUTDIR: <repo>/build/ext)
#   tools/build_test_exts.sh --check                   (syntax-check every source, report errors)
# Output: OUTDIR/<name>.cpython-315t-x86_64-linux-gnu.so; put OUTDIR on sys.path / PYTHONPATH.
#
# Some sources include CPython *internal* headers (Include/internal is not part of the spec,
# DECISIONS D2). tools/capi_shim/ holds project-written stand-ins for the few internal helpers
# that Argument-Clinic output and the tests use (implemented with the public API only).
# Parts that test genuinely internal behaviour are excluded and replaced by stub init functions:
#   (_testcapi/bytes.c builds against tools/capi_shim/pycore_bytesobject.h)
#   _testcapi/immortal.c   (pycore_long.h immortality bits)
#   (_testcapi/monitoring.c builds against tools/capi_shim/internal/pycore_instruments.h)
# _testmultiphase uses tools/capi_shim/clinic/_testmultiphase.c.h (hand-written wrappers).
# Not buildable at all: _testinternalcapi (internal headers; a Rust builtin instead), _testclinic,
# _testclinic_limited (their generated Modules/clinic/*.c.h files are not in the spec).
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CC="${CC:-gcc}"
SUFFIX=".cpython-315t-x86_64-linux-gnu.so"
INC="$(mktemp -d)"; trap 'rm -rf "$INC"' EXIT
cp "$ROOT/tools/pyconfig.h" "$INC/pyconfig.h"
CFLAGS="-fPIC -g -O1 -fno-strict-aliasing -fwrapv -Wno-unused-result -std=c11 -D_GNU_SOURCE"
CFLAGS="$CFLAGS -I$INC -I$ROOT/spec/Include -I$ROOT/tools/capi_shim ${EXTRA_CFLAGS:-}"
M="$ROOT/spec/Modules"
EXCLUDED="immortal"

sources() { # module name -> list of sources
  case "$1" in
    _testcapi)
      echo "$M/_testcapimodule.c"
      for f in "$M"/_testcapi/*.c; do
        b="$(basename "$f" .c)"
        case " $EXCLUDED " in *" $b "*) ;; *) echo "$f" ;; esac
      done
      echo "$INC/_testcapi_excluded.c" ;;
    _testlimitedcapi) echo "$M/_testlimitedcapi.c" "$M"/_testlimitedcapi/*.c ;;
    *) echo "$M/$1.c" ;;
  esac
}
# stub init functions for the excluded _testcapi parts
{
  echo '#include "Python.h"'
  for b in $EXCLUDED; do
    up="$(echo "${b:0:1}" | tr a-z A-Z)${b:1}"
    echo "int _PyTestCapi_Init_${up}(PyObject *m) { (void)m; return 0; }"
  done
} > "$INC/_testcapi_excluded.c"

ALL="_testcapi _testlimitedcapi _testbuffer _testsinglephase _testimportmultiple _testmultiphase"

if [ "${1:-}" = "--check" ]; then
  for mod in $ALL _testclinic _testclinic_limited; do
    for src in $(sources "$mod"); do
      err=$("$CC" -fsyntax-only -w $CFLAGS "$src" 2>&1 | grep -m3 'error')
      [ -n "$err" ] && printf '%s:\n%s\n' "${src#$ROOT/}" "$err"
    done
  done
  exit 0
fi

OUT="${1:-$ROOT/build/ext}"; shift || true
MODS="${*:-$ALL}"
mkdir -p "$OUT/obj"
status=0
for mod in $MODS; do
  objs=""
  ok=1
  for src in $(sources "$mod"); do
    obj="$OUT/obj/${mod}__$(basename "$src" .c).o"
    if ! "$CC" -c $CFLAGS "$src" -o "$obj" 2> "$obj.log"; then
      echo "FAILED to compile ${src#$ROOT/} (see $obj.log)"; ok=0; status=1
    fi
    objs="$objs $obj"
  done
  [ $ok = 1 ] || continue
  # extensions resolve the C API from the executable (D13): no -lpython
  if "$CC" -shared $objs -o "$OUT/$mod$SUFFIX" -lm; then
    echo "built $mod$SUFFIX"
  else
    echo "FAILED to link $mod"; status=1
  fi
done
# _testimportmultiple also provides _testimportmultiple_foo / _bar via the same file
if [ -f "$OUT/_testimportmultiple$SUFFIX" ]; then
  for sub in foo bar; do ln -sf "_testimportmultiple$SUFFIX" "$OUT/_testimportmultiple_$sub$SUFFIX"; done
fi
exit $status
