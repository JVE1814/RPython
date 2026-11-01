#!/bin/bash
# Extract every PyAPI_FUNC / PyAPI_DATA declaration visible from the public headers
# (spec/Include, public + cpython/, never internal/) for our build configuration
# (free-threaded, x86_64 Linux) using the C preprocessor, then list them as
#   <name> <func|data> <flags>
# flags: comma list of  dep (Py_DEPRECATED)  limited (visible in the abi3t limited API)
#        full (visible in the full API)
# Usage: tools/capi_inventory.sh > crates/runtime/src/capi/declared.txt
set -e
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/inc"; cp "$ROOT/tools/pyconfig.h" "$TMP/inc/pyconfig.h"
cat > "$TMP/all.c" <<'EOC'
#include "Python.h"
#include "structmember.h"
#include "marshal.h"
#include "frameobject.h"
#include "datetime.h"
EOC
extract() { # $1 = extra cpp flags; prints "name kind dep" lines
  gcc -E -P -w $1 '-DPyAPI_FUNC(T)=@@F T' '-DPyAPI_DATA(T)=extern @@D T' \
      -I"$TMP/inc" -I"$ROOT/spec/Include" "$TMP/all.c" |
  tr '\n' ' ' | sed 's/;/;\n/g; s/{/{\n/g; s/}/}\n/g' |
  grep '@@[FD]' | sed 's/[[:space:]]\+/ /g' |
  while IFS= read -r decl; do
    dep=0; case "$decl" in *__deprecated__*@@*) dep=1;; esac
    rest="${decl#*@@}"; kind="${rest:0:1}"; body="${rest:1}"
    body=$(printf '%s' "$body" | sed -E 's/__attribute__ ?\(\(([^()]|\([^()]*\))*\)\)//g')
    if [ "$kind" = F ]; then
      # name = identifier directly before the first '('  (after the return type)
      name=$(printf '%s' "$body" | sed -E 's/^([^(]*[^A-Za-z0-9_(])?([A-Za-z_][A-Za-z0-9_]*) ?\(.*/\2/')
      echo "$name func $dep"
    elif printf '%s' "$body" | grep -q '(\*'; then   # function pointer variable
      name=$(printf '%s' "$body" | sed -E 's/^[^(]*\(\*([A-Za-z_][A-Za-z0-9_]*)\).*/\1/')
      echo "$name data $dep"
    else
      name=$(printf '%s' "$body" | sed -E 's/\[.*//; s/;.*//' |
             sed -E 's/.*[^A-Za-z0-9_]([A-Za-z_][A-Za-z0-9_]*) *$/\1/')
      echo "$name data $dep"
    fi
  done
}
extract "" | sed 's/$/ full/' > "$TMP/full"
extract "-DPy_LIMITED_API=0x030f0000" | sed 's/$/ limited/' > "$TMP/lim"
cat "$TMP/full" "$TMP/lim" | sort -u |
awk '{ k=$1; kind[k]=$2; if ($3==1) dep[k]=1; f[k]=f[k] "," $4 }
     END { for (k in kind) { s=substr(f[k],2); if (dep[k]) s="dep," s; print k, kind[k], s } }' | sort
