#!/bin/bash
# perf-runtime measurement helper.
# usage: tools/bench/perf/measure.sh PY [what...]
#   what: startup (callgrind instructions of -c pass / -S -c pass),
#         rss (peak RSS of -c pass and of the startup_mem.sh workload),
#         micro (callgrind instructions of tools/bench/micro/*.py),
#         wall (best-of-5 wall ms of micro benchmarks, pinned to cpu 0 when possible)
# Default: startup rss micro.
PY="$1"; shift
HERE="$(cd "$(dirname "$0")" && pwd)"
[ $# -eq 0 ] && set -- startup rss micro
cd /tmp || exit 1
cg() { valgrind --tool=callgrind --callgrind-out-file=/dev/null "$@" 2>&1 >/dev/null | awk '/Collected/{print $4}'; }
for w in "$@"; do
  case "$w" in
    startup)
      echo "startup_instr $(cg "$PY" -c pass)"
      echo "startup_nosite_instr $(cg "$PY" -S -c pass)"
      ;;
    rss)
      echo "pass_rss_kb $("$PY" -c 'for l in open("/proc/self/status"):
    if l.startswith("VmHWM"): print(l.split()[1])')"
      "$PY" -c 'x = [list(range(100)) for _ in range(10000)]
d = {i: str(i) for i in range(100000)}
for l in open("/proc/self/status"):
    if l.startswith("VmHWM"): print("workload_rss_kb", l.split()[1])'
      "$PY" "$HERE/objs.py"
      ;;
    micro)
      "$HERE/../micro/run.sh" -c "$PY"
      ;;
    wall)
      if command -v taskset >/dev/null; then taskset -c 0 "$HERE/../micro/run.sh" "$PY"; else "$HERE/../micro/run.sh" "$PY"; fi
      ;;
  esac
done
