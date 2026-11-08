#!/bin/bash
# (mem1) Resident pages of the executable's writable data (.data, .bss, .tbss excluded) after
# start-up or after some code ran, attributed to the symbols that overlap them (anonymous
# memory: zero pages that were written, or copy-on-write data pages).
# usage: tools/bench/perf/resident_bss.sh PY [code] [min-kB]
PY="$1"; CODE="${2:-}"; MINKB="${3:-4}"
EXE=$(readlink -f "$PY")
TMP=$(mktemp -d)
mkfifo "$TMP/in"
env --default-signal=INT "$PY" -c "$CODE
import sys
open(sys.argv[1]).read()" "$TMP/in" &
PID=$!
for _ in $(seq 1000); do
  ls -l /proc/$PID/fd 2>/dev/null | grep -q "$TMP/in" && break
  sleep 0.01
done
BASE=$(awk -v exe="$EXE" '$6 == exe { split($1, r, "-"); print r[1]; exit }' /proc/$PID/maps)
# writable allocated sections: name vaddr size
readelf -SW "$EXE" | sed -n 's/^ *\[ *[0-9]*\] //p' | awk '$7 ~ /W/ && $7 ~ /A/ && $7 !~ /T/ { print $1, $3, $5 }' > "$TMP/secs"
: > "$TMP/pages"
while read -r name addr size; do
  lo=$((16#$BASE + 16#$addr)); hi=$((lo + 16#$size))
  first=$((lo / 4096)); last=$(((hi + 4095) / 4096))
  dd if=/proc/$PID/pagemap bs=8 skip=$first count=$((last - first)) 2>/dev/null | od -An -v -t u8 -w8 |
    awk -v first="$first" -v base=$((16#$BASE)) '$1 + 0 >= 9223372036854775808 { print (first + NR - 1) * 4096 - base }' >> "$TMP/pages"
done < "$TMP/secs"
echo > "$TMP/in"
wait $PID
sort -nu "$TMP/pages" > "$TMP/p2"
awk 'NR == FNR { res[$1 / 4096] = 1; next } { lo = int(("0x" $2) + 0) } END {}' "$TMP/p2" /dev/null
echo "section                 size_kB  resident_kB"
while read -r name addr size; do
  awk -v lo=$((16#$addr)) -v sz=$((16#$size)) -v name="$name" '{ if ($1 + 4096 > lo && $1 < lo + sz) n++ } END { printf "%-22s %8d %10d\n", name, sz / 1024, n * 4 }' "$TMP/p2"
done < "$TMP/secs"
nm -S -t d "$EXE" | awk '$3 ~ /^[bBdD]$/ && NF == 4' > "$TMP/syms"
awk -v minkb="$MINKB" 'NR == FNR { res[int($1 / 4096)] = 1; next }
  { va = $1 + 0; sz = $2 + 0; r = 0
    for (p = int(va / 4096); p * 4096 < va + sz; p++) if (p in res) {
      lo = (p * 4096 > va) ? p * 4096 : va; hi = ((p + 1) * 4096 < va + sz) ? (p + 1) * 4096 : va + sz; r += hi - lo }
    if (r >= minkb * 1024) printf "%8d %8d  %s\n", r / 1024, sz / 1024, $4 }' "$TMP/p2" "$TMP/syms" | sort -rn | awk 'BEGIN { print "  res_kB  size_kB  symbol" } { print }'
rm -rf "$TMP"
