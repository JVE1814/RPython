#!/bin/bash
# (mem1) Resident memory of a python process per mapping, read from outside the process
# (works the same for every build, the baselines included).
# usage: tools/bench/smaps_split.sh PY [start|peak|-S|<code>]
#   start (default): right after `PY -c ...` start-up; -S: the same with -S
#   peak: after startup_mem.sh's peak workload; anything else: that code runs first.
# Prints Rss/Anonymous per mapping (summed per file name; anonymous mappings listed one by
# one with their size) and the totals, largest first.
PY="$1"; WHAT="${2:-start}"
FLAGS=()
case "$WHAT" in
  start) CODE="" ;;
  -S) CODE=""; FLAGS=(-S) ;;
  peak) CODE='x = [list(range(100)) for _ in range(10000)]
d = {i: str(i) for i in range(100000)}' ;;
  *) CODE="$WHAT" ;;
esac
TMP=$(mktemp -d)
mkfifo "$TMP/in"
# the process blocks on the fifo until its smaps has been read
# (default SIGINT as in a foreground run: started with &, python would inherit it ignored
# and skip installing its handler)
env --default-signal=INT "$PY" "${FLAGS[@]}" -c "$CODE
import sys
open(sys.argv[1]).read()" "$TMP/in" &
PID=$!
# wait until it has opened the fifo (fd pointing at it)
for _ in $(seq 500); do
  ls -l /proc/$PID/fd 2>/dev/null | grep -q "$TMP/in" && break
  sleep 0.01
done
cp /proc/$PID/smaps "$TMP/smaps"
grep -E '^(VmHWM|VmRSS|RssAnon|RssFile)' /proc/$PID/status | tr -s ' \t' ' '
echo > "$TMP/in"
wait $PID
awk '
  /^[0-9a-f]+-[0-9a-f]+ / { file = $6; perm = $2; next }
  /^Size:/ { name = (file == "") ? "[anon " perm " " $2 "kB]" : file " " perm; next }
  /^Rss:/ { rss[name] += $2 }
  /^Anonymous:/ { anon[name] += $2 }
  END { for (n in rss) if (rss[n] > 0) printf "%8d %8d  %s\n", rss[n], anon[n], n }
' "$TMP/smaps" | sort -rn | awk 'BEGIN { print "  rss_kB  anon_kB  mapping" } { print; t += $1; a += $2 } END { printf "%8d %8d  total\n", t, a }'
rm -rf "$TMP"
