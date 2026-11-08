# (perf7) Resident memory of each mapping right after start-up (smaps), largest first.
# usage: python3 -S tools/bench/perf/rss_map.py   (run with the interpreter under test)
rows = []
cur = None
with open("/proc/self/smaps") as f:
    for line in f:
        parts = line.split()
        if not parts:
            continue
        if not parts[0].endswith(":") or "-" in parts[0]:
            name = parts[5] if len(parts) > 5 else "[anon]"
            cur = [0, parts[1], name, parts[0]]
            rows.append(cur)
        elif parts[0] == "Rss:":
            cur[0] = int(parts[1])
rows.sort(reverse=True)
for rss, perm, name, rng in rows[:16]:
    lo, hi = rng.split("-")
    size = (int(hi, 16) - int(lo, 16)) // 1024
    print("%8d kB of %8d kB  %s  %s" % (rss, size, perm, name))
print("total", sum(r[0] for r in rows), "kB")
