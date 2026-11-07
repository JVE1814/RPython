# (scale3) Parallel young collections with owner-side clearing: several threads make
# cyclic garbage (some of it linked to other threads' objects, weakly referenced, or
# with finalizers) while keeping live structures whose contents they verify. Checks:
# no live object is ever cleared or freed, every finalizer runs exactly once, every
# weakref to garbage is dead after a final full collection.
import gc, sys, threading, time, weakref

NTHREADS = 6
ROUNDS = int(sys.argv[1]) if len(sys.argv) > 1 else 400
lock = threading.Lock()
made = [[] for _ in range(64)]   # per-thread lists: no contention (threads stay runnable)
finalized = []
shared = [None] * NTHREADS   # one live object per thread, read by the others
weaks = [[] for _ in range(NTHREADS)]   # weakrefs to garbage


class Node:
    def __init__(self, v, nxt=None):
        self.v = v
        self.nxt = nxt


class Slotted:
    __slots__ = ("v", "nxt", "__weakref__")

    def __init__(self, v, nxt=None):
        self.v = v
        self.nxt = nxt


class Fin:
    def __init__(self, v):
        self.v = v
        self.me = self
        made[threading.get_ident() % 64].append(1)

    def __del__(self):
        finalized.append(1)


def ring(cls, n, base):
    first = cls(base)
    cur = first
    for j in range(1, n):
        cur = cls(base + j, cur)
    first.nxt = cur
    return first


def check_ring(r, n, base):
    seen = 0
    cur = r
    total = 0
    while seen < n:
        total += cur.v
        cur = cur.nxt
        seen += 1
    want = sum(range(base, base + n))
    if total != want:
        raise SystemExit("live ring corrupted: %r != %r" % (total, want))


SRC = "K = 2\ndef f(x):\n    return x * K\nclass C:\n    def m(self):\n        return f\n"
funcs = [None] * NTHREADS
errors = []
start = threading.Barrier(NTHREADS)


def worker(tid):
    try:
        start.wait()
        work(tid)
    except BaseException as e:
        errors.append(repr(e))


def work(tid):
    keep = [ring(Node, 7, tid * 1000 + k * 10) for k in range(20)]
    keepd = {i: [i] * 3 for i in range(50)}
    for r in range(ROUNDS):
        for i in range(60):
            g = ring(Node if i % 2 else Slotted, 9, i)
            if i % 7 == 0:
                # link garbage to a live object of another thread and to itself
                other = shared[(tid + 1) % NTHREADS]
                if isinstance(g, Node):
                    g.extra = other
            if i % 11 == 0 and isinstance(g, Slotted):
                weaks[tid].append(weakref.ref(g))
            if i % 13 == 0 and r % 10 == 0:
                f = Fin(i)
                f.ring = g if isinstance(g, Node) else None
        if r % 3 == 0:
            # functions whose globals hold them (cycles), called by other threads
            # (sharded reference counts), then dropped: sharded garbage
            ns = {}
            exec(SRC, ns)
            funcs[tid] = ns["f"]
            g = funcs[(tid + 1) % NTHREADS]
            if g is not None:
                for _ in range(6):
                    if g(2) != 4:
                        raise SystemExit("shared function broken")
            del ns
        if r % 5 == 0:
            n = Node(r)
            n.nxt = n
            shared[tid] = n
        if r % 10 == 0:
            for k, x in enumerate(keep):
                check_ring(x, 7, tid * 1000 + k * 10)
            for i in range(50):
                if keepd[i] != [i] * 3:
                    raise SystemExit("live dict corrupted")


def collector(ts):
    while any(t.is_alive() for t in ts):
        gc.collect()
        time.sleep(0.1)


def main():
    ts = [threading.Thread(target=worker, args=(i,)) for i in range(NTHREADS)]
    for t in ts:
        t.start()
    c = threading.Thread(target=collector, args=(ts,))
    c.start()
    for t in ts:
        t.join()
    c.join()
    if errors:
        raise SystemExit("worker failed: " + errors[0])
    shared[:] = [None] * NTHREADS
    for _ in range(3):
        gc.collect()
    nmade = sum(len(m) for m in made)
    if len(finalized) != nmade:
        raise SystemExit("finalizers: %d made, %d finalized" % (nmade, len(finalized)))
    alive = sum(1 for ws in weaks for w in ws if w() is not None)
    if alive:
        raise SystemExit("%d weakrefs to garbage still alive" % alive)
    print("ok", nmade, sum(len(ws) for ws in weaks))


main()
