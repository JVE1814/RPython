#!/bin/bash
# Multi-core scaling harness.
#
# usage: tools/bench/scaling/run.sh OUTDIR [workload ...]
#
# Environment:
#   OURS      our interpreter (default: /home/user/RPython/target/scale1/snap/bin/python3)
#   IMPLS     space-separated name=path list (default: ours, py-ft, py-default)
#   THREADS   thread counts (default "1 2 4")
#   REPS      repetitions per configuration (default 5)
#   SCALE     work multiplier per thread (default 1)
#   TOL       contention tolerance in CPUs (default 0.3): a run is discarded when other
#             processes used more than (NCPU - N + TOL) CPUs on average during it, i.e.
#             when fewer than N CPUs were free for our N threads
#   TIMEOUT   per-run timeout in seconds (default 300)
#
# Every run is a fresh process (tools/bench/scaling/driver.py): set-up, warm-up in the main
# thread, then N threads doing the same fixed work each, timed from a barrier to the last
# join. Repetitions are interleaved (rep -> workload -> N -> interpreter, interpreter order
# rotated every repetition) so slow drifts in machine load hit every interpreter alike.
# Output: OUTDIR/raw.tsv (one line per run) and OUTDIR/summary.txt (summarize.sh).
# The baseline interpreters are run only through this script (DECISIONS.md D15/D22).
set -u
OUT="${1:?usage: run.sh OUTDIR [workload ...]}"; shift
HERE="$(cd "$(dirname "$0")" && pwd)"
OURS="${OURS:-/home/user/RPython/target/scale1/snap/bin/python3}"
IMPLS="${IMPLS:-ours=$OURS py-ft=/opt/baseline/py-ft/bin/python3 py-default=/opt/baseline/py-default/bin/python3}"
THREADS="${THREADS:-1 2 4}"
REPS="${REPS:-5}"
SCALE="${SCALE:-1}"
TOL="${TOL:-0.3}"
TIMEOUT="${TIMEOUT:-300}"
NCPU=$(nproc)
if [ $# -eq 0 ]; then
  set -- cpu_nbody cpu_oo cpu_float cpu_gen cpu_comp cpu_regex cpu_json \
         shr_dict shr_list shr_globals shr_attrs \
         alloc_small alloc_cycles alloc_dicts \
         mut_queue mut_dict mut_lock mut_list \
         jit_loop jit_calls \
         misc_classes misc_import misc_cext
fi
mkdir -p "$OUT"
RAW="$OUT/raw.tsv"
[ -f "$RAW" ] || printf 'impl\tworkload\tthreads\trep\twall\tcpu\tbusy\tothers\tutil\tthr\ttmax\ttmin\tkept\tload1\n' > "$RAW"
read -r -a impl_list <<< "$IMPLS"
nimpl=${#impl_list[@]}
for rep in $(seq "$REPS"); do
  for wl in "$@"; do
    for n in $THREADS; do
      for k in $(seq 0 $((nimpl - 1))); do
        spec="${impl_list[$(( (k + rep) % nimpl ))]}"
        name="${spec%%=*}"; py="${spec#*=}"
        load1=$(cut -d' ' -f1 /proc/loadavg)
        line=$(cd /tmp && SCALE="$SCALE" timeout -k 5 "$TIMEOUT" "$py" "$HERE/driver.py" "$wl" "$n" "$SCALE" 2>&1 | tail -1)
        case "$line" in
          wall=*) ;;
          *) printf '%s\t%s\t%s\t%s\tERR\t\t\t\t\t\t\t\t0\t%s\n' "$name" "$wl" "$n" "$rep" "$load1" >> "$RAW"
             echo "ERROR $name $wl n=$n: $line" >&2; continue ;;
        esac
        eval "$(echo "$line" | tr ' ' '\n' | sed 's/^/f_/')"
        kept=$(awk -v o="$f_others" -v w="$f_wall" -v n="$n" -v c="$NCPU" -v t="$TOL" \
               'BEGIN { print (o <= (c - n + t) * w) ? 1 : 0 }')
        printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$name" "$wl" "$n" "$rep" \
          "$f_wall" "$f_cpu" "$f_busy" "$f_others" "$f_util" "$f_thr" "$f_tmax" "$f_tmin" "$kept" "$load1" >> "$RAW"
        printf '%-10s %-13s n=%s rep=%s wall=%s cpu=%s others=%s util=%s kept=%s\n' \
          "$name" "$wl" "$n" "$rep" "$f_wall" "$f_cpu" "$f_others" "$f_util" "$kept"
      done
    done
  done
done
bash "$HERE/summarize.sh" "$RAW" | tee "$OUT/summary.txt"
