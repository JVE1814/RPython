# (perf7) Which pages of the executable's code mapping are resident right after start-up
# (/proc/self/pagemap "present" bit), summarised per 256 KB of the mapping.
# usage: <python under test> -S tools/bench/perf/text_pages.py
import os
if os.environ.get("WORKLOAD"):
    # startup_mem.sh's peak workload first
    x = [list(range(100)) for _ in range(10000)]
    d = {i: str(i) for i in range(100000)}
exe = os.path.realpath("/proc/self/exe")
maps = []
with open("/proc/self/maps") as f:
    for line in f:
        p = line.split()
        if len(p) >= 6 and p[5] == exe:
            lo, hi = (int(x, 16) for x in p[0].split("-"))
            maps.append((lo, hi, p[1], int(p[2], 16)))
fd = os.open("/proc/self/pagemap", os.O_RDONLY)
for lo, hi, perm, off in maps:
    n = (hi - lo) // 4096
    os.lseek(fd, (lo // 4096) * 8, 0)
    data = os.read(fd, n * 8)
    present = [(int.from_bytes(data[8 * i:8 * i + 8], "little") >> 63) & 1 for i in range(n)]
    tot = sum(present)
    print("%s file+%#x %6d kB mapped, %6d kB resident" % (perm, off, n * 4, tot * 4))
    if "x" in perm or perm.startswith("r--"):
        line = []
        step = 64  # pages = 256 KB per character
        for i in range(0, n, step):
            c = sum(present[i:i + step])
            line.append(" .:-=+*#%@"[min(9, (c * 9 + step - 1) // step)])
        print("   ", "".join(line))
os.close(fd)
