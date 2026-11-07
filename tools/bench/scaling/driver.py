# One measurement of the scaling harness: run workload NAME on NTHREADS threads, each doing
# the same fixed amount of work, and print one line of key=value fields.
#
# usage: PY driver.py NAME NTHREADS [SCALE]
#
# Fields: wall (s, timed section), cpu (s, user+sys of this process during the section),
# busy (s, CPU time used by the whole machine during the section, from /proc/stat),
# others = busy - cpu (CPU taken by other processes: a contention signal), util =
# cpu / (nthreads * wall), units, thr (units per second), tmax/tmin (slowest and fastest
# thread's own time), check (workload result, to keep the work from being optimised away).
import os, sys, threading, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import workloads


def proc_ticks():
    # user+sys of this process in clock ticks (fields 14 and 15 of /proc/self/stat)
    with open("/proc/self/stat") as f:
        s = f.read()
    rest = s[s.rindex(")") + 2:].split()
    return int(rest[11]) + int(rest[12])


def machine_busy_ticks():
    with open("/proc/stat") as f:
        first = f.readline().split()
    vals = [int(x) for x in first[1:]]
    idle = vals[3] + (vals[4] if len(vals) > 4 else 0)
    # guest time is already included in user/nice
    total = sum(vals[:8])
    return total - idle


def main():
    name = sys.argv[1]
    nthreads = int(sys.argv[2])
    scale = float(sys.argv[3]) if len(sys.argv) > 3 else float(os.environ.get("SCALE", "1"))
    wl = workloads.get(name)
    units = max(1, int(round(wl.units * scale)))
    warm = max(1, units // 10)
    shared = wl.setup(nthreads, units + warm)

    # warm-up in the main thread: lets the JIT tiers / specialisers see the code first
    wl.work(shared, 0, warm)

    hz = os.sysconf("SC_CLK_TCK") if hasattr(os, "sysconf") else 100
    start = threading.Barrier(nthreads + 1)
    own = [0.0] * nthreads
    res = [None] * nthreads
    errors = []

    def runner(tid):
        try:
            start.wait()
            t0 = time.perf_counter()
            res[tid] = wl.work(shared, tid, units)
            own[tid] = time.perf_counter() - t0
        except BaseException as e:  # report, do not hang the barrier
            errors.append(repr(e))

    threads = [threading.Thread(target=runner, args=(i,)) for i in range(nthreads)]
    for t in threads:
        t.start()
    c0 = proc_ticks()
    b0 = machine_busy_ticks()
    start.wait()
    t0 = time.perf_counter()
    for t in threads:
        t.join()
    wall = time.perf_counter() - t0
    c1 = proc_ticks()
    b1 = machine_busy_ticks()
    if errors:
        print("error=" + errors[0].replace(" ", "_"))
        sys.exit(1)
    cpu = (c1 - c0) / hz
    busy = (b1 - b0) / hz
    others = max(0.0, busy - cpu)
    total_units = units * nthreads
    check = 0
    for r in res:
        try:
            check += int(r) & 0xFFFF
        except (TypeError, ValueError):
            pass
    td = getattr(wl, "teardown", None)
    if td is not None:
        td()
    print("wall=%.4f cpu=%.3f busy=%.3f others=%.3f util=%.3f units=%d thr=%.3f tmax=%.4f tmin=%.4f check=%d" % (
        wall, cpu, busy, others, cpu / (nthreads * wall) if wall > 0 else 0.0, total_units,
        total_units / wall if wall > 0 else 0.0, max(own), min(own), check))


main()
