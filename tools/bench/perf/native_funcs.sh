#!/bin/bash
# (mem1) Functions of the executable that a *native* run executes, in first-execution order:
# gdb with a temporary breakpoint on every function symbol (each fires once). Valgrind runs
# take a few different paths (and callgrind names functions in its own order), so a
# start-up order made from callgrind alone left functions out; each such function costs a
# whole fault-around window (64-128 KB) of the code mapping.
# usage: tools/bench/perf/native_funcs.sh PY OUT [python args...]
#   OUT gets one symbol name per line.
PY="$1"; OUT="$2"; shift 2
EXE=$(readlink -f "$PY")
TMP=$(mktemp -d)
nm "$EXE" | awk '$2 ~ /^[tTwW]$/ { print $1, $3 }' | sort -u -k1,1 > "$TMP/syms"
{
  echo "set pagination off"
  echo "set confirm off"
  echo "set breakpoint always-inserted on"
  echo "set disable-randomization on"
  echo "starti"
  # the executable is mapped now: relocate the symbol addresses by its load base
  echo "info proc mappings"
} > "$TMP/pre.gdb"
gdb -batch -x "$TMP/pre.gdb" --args "$EXE" "$@" 2>/dev/null | awk -v exe="$EXE" '$NF == exe && $4 == "0x0" { print $1; exit }' > "$TMP/base"
BASE=$(cat "$TMP/base")
[ -n "$BASE" ] || { echo "could not find the load base" >&2; rm -rf "$TMP"; exit 1; }
{
  cat "$TMP/pre.gdb"
  awk -v base="$BASE" 'function hex(h,   i, v) { v = 0; h = tolower(h); sub(/^0x/, "", h); for (i = 1; i <= length(h); i++) v = v * 16 + index("0123456789abcdef", substr(h, i, 1)) - 1; return v }
    BEGIN { b = hex(base) } { printf "tbreak *0x%x\n", b + hex($1) }' "$TMP/syms"
  echo "while 1"
  echo "  continue"
  echo "  printf \"HIT %lx\\n\", \$pc - $BASE"
  echo "end"
} > "$TMP/run.gdb"
# (a script started in the background inherits SIGINT ignored, and the start-up then skips
# installing its SIGINT handler: record what a foreground run does)
env --default-signal=INT gdb -batch -x "$TMP/run.gdb" --args "$EXE" "$@" 2>/dev/null | awk '$1 == "HIT" { print $2 }' > "$TMP/hits"
# hits are offsets from the load base: map back to the names (first name per address)
awk 'function hex(h,   i, v) { v = 0; h = tolower(h); for (i = 1; i <= length(h); i++) v = v * 16 + index("0123456789abcdef", substr(h, i, 1)) - 1; return v }
  NR == FNR { a = hex($1); if (!(a in name)) name[a] = $2; next }
  { a = hex($1); if ((a in name) && !seen[a]++) print name[a] }' "$TMP/syms" "$TMP/hits" > "$OUT"
echo "$(wc -l < "$OUT") functions executed -> $OUT"
rm -rf "$TMP"
