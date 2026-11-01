#!/bin/bash
# C-API coverage: declared (crates/runtime/src/capi/declared.txt) vs exported (nm -D).
#   tools/capi_coverage.sh [path/to/python3] [-v]
# Prints counts; with -v also lists missing non-deprecated symbols.
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PY="${1:-${CARGO_TARGET_DIR:-$ROOT/target}/debug/python3}"
DECL="$ROOT/crates/runtime/src/capi/declared.txt"
EXP="$(mktemp)"; trap 'rm -f "$EXP"' EXIT
nm -D --defined-only "$PY" | awk '{print $3}' | sort -u > "$EXP"
awk -v expf="$EXP" '
  BEGIN { while ((getline l < expf) > 0) have[l] = 1 }
  {
    name = $1; kind = $2; dep = ($3 ~ /dep/)
    tot[kind]++; if (name in have) ok[kind]++
    if (!dep) { ndtot[kind]++; if (name in have) ndok[kind]++; else miss[++nm] = kind " " name }
  }
  END {
    printf "functions: %d/%d exported (non-deprecated %d/%d)\n", ok["func"], tot["func"], ndok["func"], ndtot["func"]
    printf "data:      %d/%d exported (non-deprecated %d/%d)\n", ok["data"], tot["data"], ndok["data"], ndtot["data"]
    if (verbose) for (i = 1; i <= nm; i++) print "missing " miss[i]
  }' verbose="$([ "${2:-}" = -v ] && echo 1)" "$DECL"
