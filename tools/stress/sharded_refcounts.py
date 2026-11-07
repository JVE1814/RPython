# Stress (scale1): objects shared by many threads switch to per-thread reference
# counting (crates/runtime/src/sharded.rs) and are released by the collector. Shared
# functions, classes, strings, floats, tuples and long-lived dicts are hammered by
# several threads, then dropped; afterwards they must really be freed (weak
# references die after gc.collect(), refcounts are sane) and nothing may crash.
import gc
import sys
import threading
import weakref

ROUNDS = int(sys.argv[1]) if len(sys.argv) > 1 else 30
NTHREADS = 4


def make_round(r):
    # fresh objects owned by this (main) thread
    def f(x):
        return x + r

    class K:
        attr = r

        def m(self, y):
            return y * 2

    keys = ["k%d_%d" % (r, i) for i in range(300)]
    table = {k: float(i) + 0.5 for i, k in enumerate(keys)}
    tup = tuple(range(r, r + 50))
    gc.collect()  # the table becomes old: eligible for sharding
    return f, K, keys, table, tup


def worker(f, K, keys, table, tup, out):
    s = 0.0
    k = K()
    for _ in range(40):
        for key in keys:
            s += table[key]
        for v in tup:
            s += f(v) + k.m(v) + K.attr
    out.append(s)


for r in range(ROUNDS):
    f, K, keys, table, tup = make_round(r)
    out = []
    ts = [threading.Thread(target=worker, args=(f, K, keys, table, tup, out)) for _ in range(NTHREADS)]
    for t in ts:
        t.start()
    for t in ts:
        t.join()
    assert len(out) == NTHREADS and len(set(out)) == 1, out
    wf, wK = weakref.ref(f), weakref.ref(K)
    rc = sys.getrefcount(keys[0])
    assert 2 <= rc < 100, rc
    del f, K, keys, table, tup, out, ts
    gc.collect()
    gc.collect()
    assert wf() is None, "shared function not released"
    assert wK() is None, "shared class not released"

# classes created and used concurrently, then dropped
def churn(n, res):
    for i in range(n):
        C = type("C%d" % i, (object,), {"v": i})
        res.append(sum(C().v for _ in range(5)))

res = []
ts = [threading.Thread(target=churn, args=(300, res)) for _ in range(NTHREADS)]
for t in ts:
    t.start()
for t in ts:
    t.join()
gc.collect()
assert len(res) == 300 * NTHREADS
print("ok")
