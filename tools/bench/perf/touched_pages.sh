#!/bin/bash
# (mem1) The pages of the executable a run really touches (loads, stores, and with
# INSTR=1 also instruction fetches), from a valgrind lackey trace: no fault-around, unlike
# /proc/PID/pagemap residency. Prints file offsets of the touched pages, one per line.
# usage: tools/bench/perf/touched_pages.sh PY OUT [python args...]   (slow: ~1-2 min for -c pass)
PY="$1"; OUT="$2"; shift 2
EXE=$(readlink -f "$PY")
TMP=$(mktemp -d)
# where valgrind loads the executable and how its segments map to file offsets
valgrind --tool=none "$PY" -S -c "import sys; sys.stdout.write(open('/proc/self/maps').read())" 2>/dev/null |
  awk -v exe="$EXE" '$6 == exe { split($1, r, "-"); print r[1], r[2], $3 }' > "$TMP/maps"
want='$1 == "L" || $1 == "S" || $1 == "M"'
[ -n "${INSTR:-}" ] && want='$1 == "L" || $1 == "S" || $1 == "M" || $1 == "I"'
valgrind --tool=lackey --trace-mem=yes --log-fd=9 "$PY" "$@" 9>&1 >/dev/null 2>"$TMP/err" |
  awk "$want"' { split($2, a, ","); print substr(a[1], 1, length(a[1]) - 3) }' | sort -u > "$TMP/pages"
# hex page numbers -> file offsets of the executable's pages
awk 'function hex(h,   i, v) { v = 0; h = tolower(h); for (i = 1; i <= length(h); i++) v = v * 16 + index("0123456789abcdef", substr(h, i, 1)) - 1; return v }
  NR == FNR { n++; lo[n] = hex($1); hi[n] = hex($2); off[n] = hex($3); next }
  { a = hex($1) * 4096; for (i = 1; i <= n; i++) if (a >= lo[i] && a < hi[i]) { print a - lo[i] + off[i]; break } }' "$TMP/maps" "$TMP/pages" | sort -n > "$OUT"
echo "$(wc -l < "$OUT") pages touched -> $OUT"
rm -rf "$TMP"
