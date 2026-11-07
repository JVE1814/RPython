#!/bin/bash
# Summarise a scaling run: tools/bench/scaling/summarize.sh OUTDIR/raw.tsv
#
# Per interpreter, workload and thread count: median wall time and median CPU time of the
# runs kept by the contention check (all runs if none was kept; marked with '*').
#   speedup  = N * wall(1) / wall(N)   (throughput relative to one thread; ideal = N)
#   cpu-infl = cpu(N) / (N * cpu(1))   (CPU spent per unit of work relative to one thread;
#              ideal = 1.00; >1 means the threads slow each other down: contended atomics,
#              locks that spin, cache-line ping-pong, GC/safepoint work growing with N).
#              CPU time is barely affected by other processes, so this column stays
#              meaningful on a loaded machine while the wall-clock speedup may not.
#   kept     = runs kept / runs made, per thread count
#   vs-ft    = our median wall / py-ft median wall at the same N (below 1 = we are faster)
# STAT=min uses the best run instead of the median (wall: best kept run; CPU: best of all
# runs). Interference from other processes only ever adds time, so on a loaded machine
# the minimum is the better estimate of the undisturbed cost.
RAW="${1:?usage: summarize.sh raw.tsv}"
awk -F'\t' -v stat="${STAT:-median}" '
function median(s,    a, n, i, j, t) {
  n = split(s, a, " ")
  if (n == 0) return ""
  for (i = 2; i <= n; i++) { t = a[i] + 0; for (j = i - 1; j >= 1 && a[j] + 0 > t; j--) a[j + 1] = a[j]; a[j + 1] = t }
  if (stat == "min") return a[1]
  if (n % 2) return a[(n + 1) / 2]
  return (a[n / 2] + a[n / 2 + 1]) / 2
}
NR == 1 { next }
$5 == "ERR" { err[$1 SUBSEP $2 SUBSEP $3]++; next }
{
  k = $1 SUBSEP $2 SUBSEP $3
  all_w[k] = all_w[k] " " $5; all_c[k] = all_c[k] " " $6; made[k]++
  if ($13 == 1) { kw[k] = kw[k] " " $5; kc[k] = kc[k] " " $6; kept[k]++ }
  if (!($1 in seen_i)) { seen_i[$1] = 1; impls[++ni] = $1 }
  if (!($2 in seen_w)) { seen_w[$2] = 1; wls[++nw] = $2 }
  if (!($3 in seen_n)) { seen_n[$3] = 1; ns[++nn] = $3 }
}
END {
  for (i = 1; i <= nn; i++) for (j = i + 1; j <= nn; j++) if (ns[j] + 0 < ns[i] + 0) { t = ns[i]; ns[i] = ns[j]; ns[j] = t }
  for (i = 1; i <= ni; i++) for (w = 1; w <= nw; w++) for (x = 1; x <= nn; x++) {
    k = impls[i] SUBSEP wls[w] SUBSEP ns[x]
    if (kept[k] > 0) { W[k] = median(kw[k]); C[k] = median(stat == "min" ? all_c[k] : kc[k]); M[k] = "" }
    else if (made[k] > 0) { W[k] = median(all_w[k]); C[k] = median(all_c[k]); M[k] = "*" }
  }
  hdr = "| workload |"; sep = "|---|"
  for (i = 1; i <= ni; i++) {
    for (x = 2; x <= nn; x++) { hdr = hdr " " impls[i] " x" ns[x] " |"; sep = sep "---|" }
    hdr = hdr " " impls[i] " cpu-infl@" ns[nn] " |"; sep = sep "---|"
  }
  hdr = hdr " ours/ft wall@1 | ours/ft wall@" ns[nn] " |"; sep = sep "---|---|"
  print "Speedup (throughput vs 1 thread) and CPU inflation; " (stat == "min" ? "best runs" : "medians of kept runs")
  print hdr; print sep
  for (w = 1; w <= nw; w++) {
    line = "| " wls[w] " |"
    for (i = 1; i <= ni; i++) {
      k1 = impls[i] SUBSEP wls[w] SUBSEP ns[1]
      for (x = 2; x <= nn; x++) {
        k = impls[i] SUBSEP wls[w] SUBSEP ns[x]
        if (W[k] > 0 && W[k1] > 0) line = line sprintf(" %.2f%s |", ns[x] * W[k1] / W[k], M[k] M[k1])
        else line = line " - |"
      }
      k = impls[i] SUBSEP wls[w] SUBSEP ns[nn]
      if (C[k] > 0 && C[k1] > 0) line = line sprintf(" %.2f |", C[k] / (ns[nn] * C[k1]))
      else line = line " - |"
    }
    for (x = 1; x <= nn; x += nn - 1) {
      ko = "ours" SUBSEP wls[w] SUBSEP ns[x]; kf = "py-ft" SUBSEP wls[w] SUBSEP ns[x]
      if (W[ko] > 0 && W[kf] > 0) line = line sprintf(" %.2f |", W[ko] / W[kf]); else line = line " - |"
      if (nn == 1) break
    }
    print line
  }
  print ""
  print "Median wall seconds [kept/made runs] per thread count"
  for (w = 1; w <= nw; w++) {
    line = sprintf("%-14s", wls[w])
    for (i = 1; i <= ni; i++) {
      line = line sprintf("  %s:", impls[i])
      for (x = 1; x <= nn; x++) {
        k = impls[i] SUBSEP wls[w] SUBSEP ns[x]
        line = line sprintf(" %s%s[%d/%d]", W[k], M[k], kept[k], made[k] + err[k])
      }
    }
    print line
  }
}' "$RAW"
