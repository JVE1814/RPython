#!/bin/bash
# usage: tools/bench/startup_mem.sh <python> [runs]
# Prints best-of-N wall time for `-c pass` and `-S -c pass`, and peak RSS of a small workload.
PY="$1"; N="${2:-20}"
best() { local b=999; for i in $(seq $N); do s=$(date +%s%N); "$@" >/dev/null 2>&1; e=$(date +%s%N); t=$(( (e-s)/1000 )); [ $t -lt $b ] 2>/dev/null || [ $b = 999 ] && b=$t; done; echo $b; }
echo "startup_us $(best "$PY" -c pass)"
echo "startup_nosite_us $(best "$PY" -S -c pass)"
# (perf6) resident memory right after start-up, split into anonymous and file-backed pages
"$PY" -c 'for line in open("/proc/self/status"):
    if line.startswith(("VmRSS", "RssAnon", "RssFile")): print("startup_" + line.split(":")[0].lower() + "_kb", line.split()[1])'
"$PY" -c 'import sys
x = [list(range(100)) for _ in range(10000)]
d = {i: str(i) for i in range(100000)}
for line in open("/proc/self/status"):
    if line.startswith("VmHWM"): print("peak_rss_kb", line.split()[1])'
