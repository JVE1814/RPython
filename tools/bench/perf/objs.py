# Peak RSS / time of creating many small instances (inline attribute values).
import time
class P:
    def __init__(self, x, y, z):
        self.x = x
        self.y = y
        self.z = z
t = time.perf_counter()
ps = [P(i, i, i) for i in range(200000)]
s = 0
for p in ps:
    s += p.x + p.y + p.z
dt = time.perf_counter() - t
for l in open("/proc/self/status"):
    if l.startswith("VmHWM"):
        print("objs_rss_kb", l.split()[1])
print("objs_ms %.1f" % (dt * 1000))
