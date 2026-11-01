#!/bin/bash
# Build the C test helper extensions from spec/Modules against spec/Include + tools/pyconfig.h.
#   tools/build_test_exts.sh [OUTDIR] [module...]      (default OUTDIR: <repo>/build/ext)
#   tools/build_test_exts.sh --check                   (syntax-check every source, report errors)
# Output: OUTDIR/<name>.cpython-315t-x86_64-linux-gnu.so; put OUTDIR on sys.path / PYTHONPATH.
# Out of scope (DECISIONS D2): _testinternalcapi (needs Include/internal).
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CC="${CC:-gcc}"
SUFFIX=".cpython-315t-x86_64-linux-gnu.so"
INC="$(mktemp -d)"; trap 'rm -rf "$INC"' EXIT
cp "$ROOT/tools/pyconfig.h" "$INC/pyconfig.h"
CFLAGS="-fPIC -g -O1 -fno-strict-aliasing -fwrapv -Wno-unused-result -Wsign-compare -std=c11 -D_GNU_SOURCE"
CFLAGS="$CFLAGS -I$INC -I$ROOT/spec/Include ${EXTRA_CFLAGS:-}"
M="$ROOT/spec/Modules"

sources() { # module name -> list of sources
  case "$1" in
    _testcapi) echo "$M/_testcapimodule.c" "$M"/_testcapi/*.c ;;
    _testlimitedcapi) echo "$M/_testlimitedcapi.c" "$M"/_testlimitedcapi/*.c ;;
    *) echo "$M/$1.c" ;;
  esac
}
ALL="_testcapi _testlimitedcapi _testbuffer _testmultiphase _testsinglephase _testimportmultiple _testclinic _testclinic_limited"

if [ "${1:-}" = "--check" ]; then
  for mod in $ALL; do
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
exit $status
