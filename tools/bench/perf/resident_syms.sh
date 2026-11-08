#!/bin/bash
# (mem1) Which sections and symbols of the executable are resident after start-up (or after
# some code ran): resident pages from /proc/PID/pagemap, mapped to the ELF sections and to
# the symbols that overlap each resident page.
# usage: tools/bench/perf/resident_syms.sh PY [code] [section-regex] [min-kB] [flags]
#   code runs first (default: nothing, i.e. right after start-up); flags default to none
#   (pass -S for start-up without site).
#   With a section regex, the symbols of matching sections are listed by resident bytes
#   (a symbol counts as resident on the pages it overlaps that are present).
#   RESIDENT_PAGES=<file> also writes the resident file offsets there.
#   PAGES_FROM=<file>: take the page list from that file (e.g. touched_pages.sh output)
#   instead of running PY (PY then only names the executable).
PY="$1"; CODE="${2:-}"; SECRE="${3:-}"; MINKB="${4:-8}"; FLAGS="${5:-}"
EXE=$(readlink -f "$PY")
TMP=$(mktemp -d)
# The process only runs CODE and then blocks on a fifo; its maps and pagemap are read from
# outside, so nothing of the measurement runs in it (a Python loop over the pages would
# start the JIT compiler and fault in its code). Works for every build.
measure() {
mkfifo "$TMP/in"
# (default SIGINT as in a foreground run: started with &, python would inherit it ignored
# and skip installing its handler)
env --default-signal=INT "$PY" $FLAGS -c "$CODE
import sys
open(sys.argv[1]).read()" "$TMP/in" &
PID=$!
for _ in $(seq 1000); do
  ls -l /proc/$PID/fd 2>/dev/null | grep -q "$TMP/in" && break
  sleep 0.01
done
: > "$TMP/raw"
: > "$TMP/maps"
while read -r range perm off dev inode path; do
  [ "$path" = "$EXE" ] || continue
  lo=$((16#${range%-*})); hi=$((16#${range#*-}))
  dd if=/proc/$PID/pagemap bs=8 skip=$((lo / 4096)) count=$(((hi - lo) / 4096)) 2>/dev/null >> "$TMP/raw"
  echo "$((16#$off)) $(((hi - lo) / 4096))" >> "$TMP/maps"
done < /proc/$PID/maps
grep -E '^(VmRSS|RssAnon|RssFile)' /proc/$PID/status | tr -s ' \t' ' '
echo > "$TMP/in"
wait $PID
# resident pages as file offsets (bit 63 of a pagemap word: present)
od -An -v -t u8 -w8 "$TMP/raw" | awk 'NR == FNR { for (i = 0; i < $2; i++) off[n++] = $1 + i * 4096; next }
  { if ($1 + 0 >= 9223372036854775808) print off[m]; m++ }' "$TMP/maps" - > "$TMP/pages"
}
if [ -n "${PAGES_FROM:-}" ]; then sort -n "$PAGES_FROM" > "$TMP/pages"; else measure; fi
[ -n "${RESIDENT_PAGES:-}" ] && cp "$TMP/pages" "$RESIDENT_PAGES"
# sections: name file-offset size vaddr (allocated ones)
readelf -SW "$EXE" | sed -n 's/^ *\[ *[0-9]*\] //p' | awk '$1 != "NULL" && $2 != "NOBITS" && $7 ~ /A/ { print $1, hex($4), hex($5), hex($3) }
  function hex(h,   i, v) { v = 0; h = tolower(h); for (i = 1; i <= length(h); i++) v = v * 16 + index("0123456789abcdef", substr(h, i, 1)) - 1; return v }' > "$TMP/secs"
echo "section                      size_kB  resident_kB"
awk 'NR == FNR { res[int($1 / 4096)] = 1; next }
  { name = $1; off = $2; size = $3; n = 0
    for (p = int(off / 4096); p * 4096 < off + size; p++) if (p in res) n++
    if (size >= 8192 || n > 1) printf "%-28s %7d %8d\n", name, size / 1024, n * 4 }' "$TMP/pages" "$TMP/secs"
awk '{ t++ } END { printf "%-28s %7s %8d\n", "total resident (exe)", "", t * 4 }' "$TMP/pages"
if [ -n "$SECRE" ]; then
  # symbols with their file offsets (only those inside a matching section)
  nm -S -t d "$EXE" 2>/dev/null | awk 'NF == 4' > "$TMP/syms"
  awk -v re="$SECRE" -v minkb="$MINKB" '
    FILENAME == ARGV[1] { res[int($1 / 4096)] = 1; next }
    FILENAME == ARGV[2] { if ($1 ~ re) { ns++; sn[ns] = $1; so[ns] = $2; ss[ns] = $3; sv[ns] = $4 } next }
    { va = $1 + 0; sz = $2 + 0; if (sz == 0) next
      for (i = 1; i <= ns; i++) if (va >= sv[i] && va < sv[i] + ss[i]) {
        off = va - sv[i] + so[i]; r = 0
        for (p = int(off / 4096); p * 4096 < off + sz; p++) if (p in res) {
          lo = (p * 4096 > off) ? p * 4096 : off; hi = ((p + 1) * 4096 < off + sz) ? (p + 1) * 4096 : off + sz; r += hi - lo }
        if (sz >= minkb * 1024 || r >= minkb * 1024) printf "%8d %8d  %s  %s\n", r / 1024, sz / 1024, sn[i], $4
        break } }' "$TMP/pages" "$TMP/secs" "$TMP/syms" | sort -rn | awk 'BEGIN { print "  res_kB  size_kB  section  symbol" } { print }'
fi
rm -rf "$TMP"
