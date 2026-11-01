# perf-runtime: concurrent attribute traffic on shared instances (inline values,
# deferred reclamation). Prints OK if every thread finished and values are sane.
import threading, sys

class Box:
    def __init__(self):
        self.a = 0
        self.b = "x"

shared = [Box() for _ in range(8)]
errors = []

def writer(tid, n):
    try:
        for i in range(n):
            o = shared[i % 8]
            o.a = [tid, i]            # replaced values are released deferred
            o.b = str(i)
            if i % 50 == 0:
                setattr(o, "extra%d" % (i % 3), i)
            if i % 97 == 0 and hasattr(o, "extra0"):
                try:
                    del o.extra0
                except AttributeError:
                    pass
    except Exception as e:
        errors.append(repr(e))

def reader(n):
    try:
        for i in range(n):
            o = shared[i % 8]
            v = o.a
            if not (v == 0 or (isinstance(v, list) and len(v) == 2)):
                errors.append("bad a %r" % (v,))
            s = o.b
            if not isinstance(s, str):
                errors.append("bad b %r" % (s,))
            if i % 200 == 0:
                d = vars(o)       # materialisation racing with writers
                if "a" not in d:
                    errors.append("vars lost a")
    except Exception as e:
        errors.append(repr(e))

n = int(sys.argv[1]) if len(sys.argv) > 1 else 20000
ts = [threading.Thread(target=writer, args=(t, n)) for t in range(3)]
ts += [threading.Thread(target=reader, args=(n,)) for _ in range(3)]
for t in ts: t.start()
for t in ts: t.join()
# per-thread objects: no sharing, check results
def own(n):
    s = 0
    for i in range(n):
        b = Box(); b.a = i; s += b.a
    return s
res = []
ts = [threading.Thread(target=lambda: res.append(own(5000))) for _ in range(4)]
for t in ts: t.start()
for t in ts: t.join()
if res != [sum(range(5000))] * 4:
    errors.append("own %r" % res)
print("OK" if not errors else "FAIL %r" % errors[:5])
