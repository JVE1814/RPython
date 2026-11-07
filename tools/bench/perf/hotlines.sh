#!/bin/bash
# (perf7) Instruction-cache working set of a steady state, from two callgrind profiles of
# the same workload at L1 and L2 loops, written with
#   valgrind --tool=callgrind --dump-instr=yes --compress-pos=no --compress-strings=no ...
# usage: tools/bench/perf/hotlines.sh A.out B.out MIN
# Counts the distinct 64-byte code lines whose instructions executed at least MIN more
# times in B than in A (MIN = L2 - L1: code that runs on every loop; start-up and warm-up
# cancel out), per object (the executable, libc, JIT code = "???"), in KB. Compare with
# the L1 instruction cache (32-64 KB): a hot set far above it means capacity misses
# however the code is ordered.
awk -v min="$3" '
function hex(h,   i, v) {
  v = 0; h = tolower(substr(h, 3))
  for (i = 1; i <= length(h); i++) v = v * 16 + index("0123456789abcdef", substr(h, i, 1)) - 1
  return v
}
FNR == 1 { f++ }
/^ob=/ { ob = substr($0, 4); next }
/^calls=/ { skip = 1; next }
/^0x/ {
  if (skip) { skip = 0; next }
  k = ob SUBSEP int(hex($1) / 64)
  if (f == 1) cnt[k] -= $3; else cnt[k] += $3
  next
}
{ skip = 0 }
END {
  for (k in cnt) {
    if (cnt[k] >= min) {
      split(k, p, SUBSEP)
      n[p[1]]++
      tot++
    }
  }
  for (o in n) printf "%8d lines %8.1f KB  %s\n", n[o], n[o] * 64 / 1024, o
  printf "%8d lines %8.1f KB  total (at least %d more executions)\n", tot, tot * 64 / 1024, min
}' "$1" "$2"
