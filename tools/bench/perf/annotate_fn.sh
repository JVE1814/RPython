#!/bin/bash
# (perf7) Per-instruction cost of one function from a callgrind profile written with
# --dump-instr=yes --compress-pos=no --compress-strings=no, joined with its disassembly.
# usage: tools/bench/perf/annotate_fn.sh CALLGRIND.OUT BINARY 'FN-REGEX' [MIN-COUNT]
# FN-REGEX matches callgrind's function name (e.g. 'helpers::h_call(.2)?$'); the binary's
# load bias under valgrind is found from the symbol table. Prints "count asm" for the
# instructions executed at least MIN-COUNT times (default 1).
OUT="$1"; BIN="$2"; RE="$3"; MIN="${4:-1}"
TMP=$(mktemp -d)
awk -v re="$RE" '/^fn=/{infn = ($0 ~ re)} /^calls=/{skip=1; next}
  /^0x/{ if (skip) {skip=0; next}; if (infn) c[$1]+=$3; next } {skip=0}
  END{for (a in c) print a, c[a]}' "$OUT" | sort > "$TMP/costs"
[ -s "$TMP/costs" ] || { echo "no samples for $RE"; rm -rf "$TMP"; exit 1; }
first=$(head -1 "$TMP/costs" | cut -d' ' -f1)
# the function's symbol: the one whose name demangles to a match and contains the address
nm -C "$BIN" | awk '$2 ~ /^[tTwW]$/' > "$TMP/syms"
bias=""
for b in 0x108000 0x0 0x400000 0x4000000; do
  rel=$(printf "%x" $(( first - b )))
  if awk -v r="$rel" 'BEGIN{found=0} { a=$1; sub(/^0+/, "", a); if (a == r) found=1 } END{exit !found}' "$TMP/syms"; then bias=$b; break; fi
  # inside a function: accept any bias that lands in the text range
done
[ -z "$bias" ] && bias=0x108000
lo=$(printf "0x%x" $(( first - bias )))
last=$(tail -1 "$TMP/costs" | cut -d' ' -f1)
hi=$(printf "0x%x" $(( last - bias + 16 )))
while read -r a c; do printf "%x %s\n" $(( a - bias )) "$c"; done < "$TMP/costs" > "$TMP/costs2"
objdump -d --no-show-raw-insn --start-address="$lo" --stop-address="$hi" "$BIN" | grep "^  *[0-9a-f]*:" > "$TMP/asm"
awk -v min="$MIN" 'NR==FNR{c[$1]=$2; next} {addr=$1; sub(/:$/,"",addr); if ((addr in c) && c[addr] >= min) printf "%10d %s\n", c[addr], $0}' "$TMP/costs2" "$TMP/asm"
rm -rf "$TMP"
