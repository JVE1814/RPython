# (perf7) startup_mem.sh's peak workload, then the resident memory split (anon / file).
# usage: <python under test> tools/bench/perf/peak_split.py
x = [list(range(100)) for _ in range(10000)]
d = {i: str(i) for i in range(100000)}
for line in open("/proc/self/status"):
    if line.startswith(("VmHWM", "VmRSS", "RssAnon", "RssFile")):
        print(line.split(":")[0], line.split()[1])
