#!/bin/bash
# (mem1) startup_mem.sh's memory numbers for several builds, interleaved, N rounds: prints
# min/max of start-up VmRSS, RssAnon, RssFile and peak VmHWM per build (kB). Warm page cache:
# each binary is read once first.
# usage: tools/bench/mem_compare.sh N PY...
N="$1"; shift
HERE="$(cd "$(dirname "$0")" && pwd)"
TMP=$(mktemp -d)
for py in "$@"; do cat "$(readlink -f "$py")" > /dev/null; done
for r in $(seq "$N"); do
  i=0
  for py in "$@"; do
    "$HERE/startup_mem.sh" "$py" 1 | awk -v i=$i '{ print i, $1, $2 }' >> "$TMP/all"
    i=$((i + 1))
  done
done
printf "%-48s %15s %15s %15s %15s\n" build vmrss rssanon rssfile peak_hwm
i=0
for py in "$@"; do
  awk -v i=$i -v name="$py" '$1 == i { k = $2; v = $3 + 0; if (!(k in lo) || v < lo[k]) lo[k] = v; if (v > hi[k]) hi[k] = v }
    END { printf "%-48s %7d-%-7d %7d-%-7d %7d-%-7d %7d-%-7d\n", substr(name, length(name) - 47), lo["startup_vmrss_kb"], hi["startup_vmrss_kb"],
      lo["startup_rssanon_kb"], hi["startup_rssanon_kb"], lo["startup_rssfile_kb"], hi["startup_rssfile_kb"], lo["peak_rss_kb"], hi["peak_rss_kb"] }' "$TMP/all"
  i=$((i + 1))
done
rm -rf "$TMP"
