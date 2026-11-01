# Multi-core scaling benchmark: N threads each run a CPU-bound pure-Python workload.
# usage: python scaling.py [max_threads]   -> prints "threads seconds speedup"
import sys, threading, time

def work(n=200000):
    s = 0
    for i in range(n):
        s += (i * i) % 7
    return s

def run(nthreads, per_thread=4):
    ts = [threading.Thread(target=lambda: [work() for _ in range(per_thread)]) for _ in range(nthreads)]
    t0 = time.perf_counter()
    for t in ts: t.start()
    for t in ts: t.join()
    return time.perf_counter() - t0

maxt = int(sys.argv[1]) if len(sys.argv) > 1 else 4
base = run(1)
print("threads seconds speedup(work/time vs 1 thread)")
for n in range(1, maxt + 1):
    dt = run(n) if n > 1 else base
    print(n, "%.3f" % dt, "%.2f" % (n * base / dt))
