# Stress (scale1): dict reads without the object lock (types/dict.rs lookup_lockfree)
# racing writers that insert, replace, delete, pop, resize and clear. Every value a
# reader gets must be one that was stored under that key (values carry their key),
# and nothing may crash or read freed memory.
import sys
import threading
import time

DURATION = float(sys.argv[1]) if len(sys.argv) > 1 else 4.0
stop = time.monotonic() + DURATION
NKEYS = 64
keys = ["key%d" % i for i in range(NKEYS)]
ikeys = list(range(1000, 1000 + NKEYS))
d = {}
G = globals()
errors = []


def writer(seed):
    r = seed
    n = 0
    while time.monotonic() < stop:
        r = (r * 1103515245 + 12345) & 0x7FFFFFFF
        i = r % NKEYS
        op = (r >> 8) % 10
        k = keys[i]
        if op < 4:
            d[k] = (k, n)
            d[ikeys[i]] = (ikeys[i], n)
        elif op < 6:
            d.pop(k, None)
            d.pop(ikeys[i], None)
        elif op == 6:
            for j in range(NKEYS):  # grow: resize while readers probe
                d["tmp%d_%d" % (seed, j)] = ("tmp%d_%d" % (seed, j), n)
            for j in range(NKEYS):
                d.pop("tmp%d_%d" % (seed, j), None)
        elif op == 7 and n % 50 == 0:
            d.clear()
        elif op == 8:
            try:
                d.popitem()
            except KeyError:
                pass
        else:
            G["gvar%d" % i] = ("gvar%d" % i, n)
        n += 1


def reader():
    try:
        while time.monotonic() < stop:
            for i in range(NKEYS):
                k = keys[i]
                v = d.get(k)
                if v is not None and v[0] != k:
                    errors.append(("str", k, v))
                v = d.get(ikeys[i])
                if v is not None and v[0] != ikeys[i]:
                    errors.append(("int", ikeys[i], v))
                if k in d:
                    pass
                g = G.get("gvar%d" % i)
                if g is not None and g[0] != "gvar%d" % i:
                    errors.append(("global", i, g))
    except BaseException as e:
        errors.append(e)


ts = [threading.Thread(target=writer, args=(s,)) for s in (1, 2)]
ts += [threading.Thread(target=reader) for _ in range(3)]
for t in ts:
    t.start()
for t in ts:
    t.join()
assert not errors, errors[:5]
print("ok")
