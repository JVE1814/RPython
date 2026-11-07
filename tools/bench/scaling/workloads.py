# Workloads for the multi-core scaling harness (tools/bench/scaling/run.sh).
#
# Every workload is a class with
#   units             work units one thread performs in a timed run (scaled by SCALE)
#   setup(nthreads)   builds the data shared by all threads; returns it
#   work(shared, tid, units) performs `units` units on thread `tid`
# The same fixed amount of work per thread is done for every thread count, so ideal
# scaling keeps the wall time constant and the throughput grows with N.
#
# Categories (name prefixes):
#   cpu_    independent pure-Python CPU work (no shared mutable state)
#   shr_    shared read-mostly data: dict/list/globals/class attributes/functions/types
#   alloc_  many short-lived objects per thread (allocator, GC, deferred freeing)
#   mut_    shared mutable structures with contention (queue, dict writes, locks)
#   jit_    code compiled by the JIT, shared by all threads
#   misc_   class creation, imports, calls into builtin (C/Rust) modules
#
# All kernels are written for this harness.

import math, re, json, threading, queue, struct, zlib, binascii, os, sys, itertools, operator


REGISTRY = {}


def workload(cls):
    REGISTRY[cls.__name__] = cls
    return cls


# ---------------------------------------------------------------------- (a) cpu_

@workload
class cpu_nbody:
    """Float-heavy gravitational simulation of five bodies (lists of floats)."""
    units = 20

    def setup(self, nthreads, units):
        return None

    def work(self, shared, tid, units):
        e = 0.0
        for _ in range(units):
            bodies = [
                [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 39.47],
                [4.84, -1.16, -0.10, 0.60, 2.81, -0.02, 0.037],
                [8.34, 4.12, -0.40, -1.01, 1.82, 0.008, 0.011],
                [12.89, -15.11, -0.22, 1.08, 0.86, -0.01, 0.0017],
                [15.37, -25.91, 0.17, 0.97, 0.59, -0.03, 0.002],
            ]
            pairs = [(bodies[i], bodies[j]) for i in range(5) for j in range(i + 1, 5)]
            dt = 0.01
            for _step in range(1500):
                for (a, b) in pairs:
                    dx = a[0] - b[0]
                    dy = a[1] - b[1]
                    dz = a[2] - b[2]
                    d2 = dx * dx + dy * dy + dz * dz
                    mag = dt / (d2 * math.sqrt(d2))
                    bm = b[6] * mag
                    am = a[6] * mag
                    a[3] -= dx * bm
                    a[4] -= dy * bm
                    a[5] -= dz * bm
                    b[3] += dx * am
                    b[4] += dy * am
                    b[5] += dz * am
                for body in bodies:
                    body[0] += dt * body[3]
                    body[1] += dt * body[4]
                    body[2] += dt * body[5]
            for body in bodies:
                e += body[6] * (body[3] ** 2 + body[4] ** 2 + body[5] ** 2)
        return e


class _Job:
    __slots__ = ("kind", "prio", "count", "next", "payload")

    def __init__(self, kind, prio, nxt):
        self.kind = kind
        self.prio = prio
        self.count = 0
        self.next = nxt
        self.payload = [0] * 4


class _Scheduler:
    def __init__(self, njobs):
        head = None
        for i in range(njobs):
            head = _Job(i % 3, (i * 7) % 11, head)
        self.head = head
        self.ticks = 0

    def step_job(self, job):
        k = job.kind
        if k == 0:
            job.count += 1
            job.payload[job.count & 3] += job.prio
        elif k == 1:
            job.prio = (job.prio * 5 + 3) % 17
            job.payload[0] ^= job.prio
        else:
            if job.next is not None:
                job.next.count += 1
            job.payload[2] = job.count + job.prio
        self.ticks += 1

    def run(self, rounds):
        for _ in range(rounds):
            job = self.head
            while job is not None:
                self.step_job(job)
                job = job.next
        return self.ticks


@workload
class cpu_oo:
    """Object-oriented scheduler: method calls, attribute reads/writes on own objects."""
    units = 20

    def setup(self, nthreads, units):
        return None

    def work(self, shared, tid, units):
        t = 0
        for _ in range(units):
            t += _Scheduler(50).run(400)
        return t


class _Vec:
    __slots__ = ("x", "y", "z")

    def __init__(self, x, y, z):
        self.x = x
        self.y = y
        self.z = z

    def scaled(self, f):
        return _Vec(self.x * f, self.y * f, self.z * f)

    def norm(self):
        return math.sqrt(self.x * self.x + self.y * self.y + self.z * self.z)

    def unit(self):
        n = self.norm()
        return _Vec(self.x / n, self.y / n, self.z / n)


@workload
class cpu_float:
    """Float math on small objects (creates and drops many short-lived instances)."""
    units = 20

    def setup(self, nthreads, units):
        return None

    def work(self, shared, tid, units):
        s = 0.0
        for _ in range(units):
            pts = [_Vec(math.sin(i), math.cos(i) * 3.0, (math.sin(i) ** 2) / 2.0) for i in range(4000)]
            for p in pts:
                q = p.unit().scaled(2.5)
                s += q.x + q.y + q.z
        return s


def _tree(depth):
    if depth == 0:
        yield 1
        return
    yield from _tree(depth - 1)
    yield depth
    yield from _tree(depth - 1)


@workload
class cpu_gen:
    """Generators: nested yield from, generator expressions, iterator protocol."""
    units = 20

    def setup(self, nthreads, units):
        return None

    def work(self, shared, tid, units):
        s = 0
        for _ in range(units):
            for v in _tree(12):
                s += v
            s += sum(x * 2 for x in range(20000) if x & 1)
        return s


@workload
class cpu_comp:
    """List/dict/set comprehensions over ranges and small lists."""
    units = 20

    def setup(self, nthreads, units):
        return None

    def work(self, shared, tid, units):
        n = 0
        for _ in range(units):
            for k in range(25):
                a = [i * k for i in range(400)]
                d = {i: v for i, v in enumerate(a) if v % 3}
                st = {v & 255 for v in d.values()}
                m = [[x + y for x in range(8)] for y in range(8)]
                n += len(d) + len(st) + len(m)
        return n


_WORDS = ("alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho "
          "sigma tau upsilon phi chi psi omega").split()


@workload
class cpu_regex:
    """re: search/findall/sub with patterns compiled once per thread."""
    units = 20

    def setup(self, nthreads, units):
        lines = []
        for i in range(300):
            w = _WORDS[i % len(_WORDS)]
            lines.append("%s-%d user%d@example.org %s %d.%d" % (w, i, i * 7, w.upper(), i, i * 3 % 10))
        return "\n".join(lines)

    def work(self, shared, tid, units):
        mail = re.compile(r"([a-z]+\d+)@([a-z]+)\.org")
        num = re.compile(r"\b(\d+)\.(\d)\b")
        word = re.compile(r"\b[A-Z]{3,}\b")
        n = 0
        for _ in range(units):
            for _r in range(4):
                n += len(mail.findall(shared))
                n += len(num.sub(lambda m: m.group(2) + m.group(1), shared))
                n += sum(1 for _m in word.finditer(shared))
        return n


@workload
class cpu_json:
    """json.dumps / json.loads round trips of a nested document (thread-private copy)."""
    units = 20

    def setup(self, nthreads, units):
        return None

    def work(self, shared, tid, units):
        doc = {
            "id": tid,
            "name": "worker %d" % tid,
            "tags": ["x", "y", "z", "with \"quotes\" and \\ slashes"],
            "items": [{"k": i, "v": i * 1.5, "ok": i % 2 == 0, "s": "item-%d" % i} for i in range(60)],
            "nested": {"a": {"b": {"c": [1, 2, 3, None, True]}}},
        }
        n = 0
        for _ in range(units):
            for _r in range(15):
                s = json.dumps(doc)
                d = json.loads(s)
                n += len(s) + len(d["items"])
        return n


# ---------------------------------------------------------------------- (b) shr_

@workload
class shr_dict:
    """All threads read one shared dict (str and int keys), never writing it."""
    units = 20

    def setup(self, nthreads, units):
        d = {}
        for i in range(500):
            d["key%d" % i] = i
            d[i] = "v%d" % i
        keys = ["key%d" % i for i in range(500)]
        return (d, keys)

    def work(self, shared, tid, units):
        d, keys = shared
        n = 0
        for _ in range(units):
            for _r in range(20):
                for k in keys:
                    n += d[k]
                for i in range(0, 500, 5):
                    if i in d:
                        n += len(d[i])
                n += len(d.get("missing", ""))
        return n


@workload
class shr_list:
    """All threads iterate and index one shared list of shared objects (tuples, floats)."""
    units = 20

    def setup(self, nthreads, units):
        return [(i, float(i) * 0.5, "s%d" % i) for i in range(2000)]

    def work(self, shared, tid, units):
        n = 0.0
        L = shared
        for _ in range(units):
            for _r in range(6):
                for t in L:
                    n += t[1]
                for i in range(0, len(L), 3):
                    n += L[i][0]
        return n


G_SCALE = 3
G_OFFSET = 7
G_TABLE = {"a": 1, "b": 2}


def g_helper(x):
    return x * G_SCALE + G_OFFSET


def g_helper2(x, y):
    return g_helper(x) - g_helper(y) + len(G_TABLE)


@workload
class shr_globals:
    """Module globals, builtins and shared module-level functions read by every thread."""
    units = 20

    def setup(self, nthreads, units):
        return None

    def work(self, shared, tid, units):
        n = 0
        for _ in range(units):
            for i in range(12000):
                n += g_helper2(i, G_OFFSET) + abs(-i) + min(i, G_SCALE)
        return n


class _Base:
    LIMIT = 10
    FACTOR = 2.0

    def value(self, x):
        return x * self.FACTOR


class _Shared(_Base):
    BONUS = 1.0

    def __init__(self):
        self.extra = 0.5

    def compute(self, x):
        if x > self.LIMIT:
            return self.value(x) + self.BONUS + self.extra
        return self.value(x) - self.extra

    @property
    def prop(self):
        return self.extra * 2


@workload
class shr_attrs:
    """Class attributes, inherited methods, a property and isinstance on one shared instance."""
    units = 20

    def setup(self, nthreads, units):
        return _Shared()

    def work(self, shared, tid, units):
        o = shared
        n = 0.0
        for _ in range(units):
            for i in range(6000):
                n += o.compute(i & 31) + o.prop + _Shared.BONUS
                if isinstance(o, _Base):
                    n += 1
        return n


# ---------------------------------------------------------------------- (c) alloc_

class _Node:
    def __init__(self, v, nxt):
        self.v = v
        self.nxt = nxt


@workload
class alloc_small:
    """Short-lived tuples, lists, instances and floats (no cycles)."""
    units = 20

    def setup(self, nthreads, units):
        return None

    def work(self, shared, tid, units):
        n = 0
        for _ in range(units):
            for i in range(8000):
                t = (i, i + 1, i * 0.5)
                L = [t, i, str(i & 63)]
                node = _Node(L, None)
                n += len(node.v) + t[0]
        return n


@workload
class alloc_cycles:
    """Cyclic garbage (doubly linked rings) so that the cycle collector runs often."""
    units = 20

    def setup(self, nthreads, units):
        return None

    def work(self, shared, tid, units):
        n = 0
        for _ in range(units):
            for i in range(600):
                first = _Node(i, None)
                cur = first
                for j in range(8):
                    nxt = _Node(j, cur)
                    cur = nxt
                first.nxt = cur
                n += cur.v
        return n


@workload
class alloc_dicts:
    """Short-lived dicts, strings and lists of varying sizes (allocator size classes)."""
    units = 20

    def setup(self, nthreads, units):
        return None

    def work(self, shared, tid, units):
        n = 0
        for _ in range(units):
            for i in range(2500):
                d = {"a": i, "b": [i] * (i & 15), "c": "x" * (i & 63)}
                d["d"] = d["c"] + str(i)
                n += len(d["d"]) + len(d["b"])
        return n


# ---------------------------------------------------------------------- (d) mut_

@workload
class mut_queue:
    """One queue.Queue shared by all threads; each thread puts and gets its own items."""
    units = 20

    def setup(self, nthreads, units):
        return queue.Queue()

    def work(self, shared, tid, units):
        q = shared
        n = 0
        for _ in range(units):
            for i in range(300):
                q.put(i)
                q.put(i + 1)
                n += q.get()
                n += q.get()
        return n


@workload
class mut_dict:
    """One dict written by all threads: per-thread keys (no logical conflicts) + shared hot keys."""
    units = 20

    def setup(self, nthreads, units):
        d = {"hot%d" % i: 0 for i in range(8)}
        return d

    def work(self, shared, tid, units):
        d = shared
        mine = ["t%d-%d" % (tid, i) for i in range(64)]
        hot = ["hot%d" % i for i in range(8)]
        n = 0
        for _ in range(units):
            for r in range(80):
                for k in mine:
                    d[k] = r
                for k in hot:
                    d[k] = d[k] + 1
                n += len(d)
        return n


@workload
class mut_lock:
    """A threading.Lock protecting a shared counter, plus private work between acquisitions."""
    units = 20

    def setup(self, nthreads, units):
        return [threading.Lock(), 0]

    def work(self, shared, tid, units):
        lock = shared[0]
        n = 0
        for _ in range(units):
            for i in range(2500):
                n += (i * i) % 7
                with lock:
                    shared[1] += 1
        return n


@workload
class mut_list:
    """append/pop on one shared list and reads of its length."""
    units = 20

    def setup(self, nthreads, units):
        return []

    def work(self, shared, tid, units):
        L = shared
        n = 0
        for _ in range(units):
            for i in range(3000):
                L.append(i)
                if len(L) > 64:
                    try:
                        n += L.pop()
                    except IndexError:
                        pass
        return n


# ---------------------------------------------------------------------- (e) jit_

def _hot_kernel(n):
    # integer/float loop: compiled by the JIT tiers after the warm-up
    a = 0
    b = 1.0
    for i in range(n):
        a = (a + i * 3) & 0xFFFFF
        b = b * 0.999 + (i & 7)
    return a + int(b)


class _Acc:
    def __init__(self):
        self.total = 0

    def add(self, v):
        self.total += v
        return self


@workload
class jit_loop:
    """A hot numeric function compiled once (warm-up) and run by every thread."""
    units = 20

    def setup(self, nthreads, units):
        return None

    def work(self, shared, tid, units):
        s = 0
        for _ in range(units):
            s += _hot_kernel(30000)
        return s


def _hot_calls(acc, n):
    for i in range(n):
        acc.add(i).add(1)
    return acc.total


@workload
class jit_calls:
    """Hot method calls on thread-private objects of a shared class (inlining candidates)."""
    units = 20

    def setup(self, nthreads, units):
        return None

    def work(self, shared, tid, units):
        s = 0
        for _ in range(units):
            s += _hot_calls(_Acc(), 8000)
        return s


# ---------------------------------------------------------------------- (f) misc_

@workload
class misc_classes:
    """Class creation with type() and class statements, then instantiation."""
    units = 20

    def setup(self, nthreads, units):
        return None

    def work(self, shared, tid, units):
        n = 0
        for _ in range(units):
            for i in range(60):
                C = type("C%d" % i, (object,), {"x": i, "f": lambda self: self.x})

                class D(C):
                    y = i

                    def g(self):
                        return self.f() + self.y

                n += D().g()
        return n


@workload
class misc_import:
    """Imports of fresh modules (one source file per thread and unit) through importlib."""
    units = 20

    def setup(self, nthreads, units):
        import tempfile
        d = tempfile.mkdtemp(prefix="scal_imp_")
        body = "".join("def f%d(x):\n    return x + %d\n\nclass K%d:\n    a = %d\n\n" % (i, i, i, i)
                       for i in range(20))
        nmods = units * 2 + 2
        for t in range(nthreads):
            for u in range(nmods):
                with open(os.path.join(d, "scalmod_%d_%d.py" % (t, u)), "w") as f:
                    f.write(body)
        sys.path.insert(0, d)
        self.dir = d
        self.counter = [0] * nthreads
        return d

    def work(self, shared, tid, units):
        import importlib
        n = 0
        for _ in range(units):
            for _r in range(2):
                k = self.counter[tid]
                self.counter[tid] = k + 1
                name = "scalmod_%d_%d" % (tid, k)
                m = importlib.import_module(name)
                n += m.f3(1) + m.K7.a
                del sys.modules[name]
        return n

    def teardown(self):
        import shutil
        shutil.rmtree(self.dir, ignore_errors=True)


@workload
class misc_cext:
    """Calls into builtin modules: math, struct, zlib, binascii, operator."""
    units = 20

    def setup(self, nthreads, units):
        return b"0123456789abcdef" * 8

    def work(self, shared, tid, units):
        data = shared
        st = struct.Struct("<idh")
        n = 0
        for _ in range(units):
            for i in range(1500):
                n += int(math.hypot(i, 3.0))
                p = st.pack(i, i * 0.5, i & 0x7FFF)
                n += st.unpack(p)[0]
                n += zlib.crc32(data) & 1
                n += len(binascii.hexlify(p))
                n += operator.add(i, 1)
        return n


# Work units per thread at SCALE=1, calibrated so that one thread of our interpreter runs
# for roughly 0.4 s (CPU-time samples have 10 ms resolution). Fixed for every interpreter.
UNITS = {
    "cpu_nbody": 100, "cpu_oo": 120, "cpu_float": 80, "cpu_gen": 120, "cpu_comp": 200,
    "cpu_regex": 300, "cpu_json": 560,
    "shr_dict": 500, "shr_list": 700, "shr_globals": 160, "shr_attrs": 220,
    "alloc_small": 120, "alloc_cycles": 240, "alloc_dicts": 160,
    "mut_queue": 320, "mut_dict": 1600, "mut_lock": 440, "mut_list": 600,
    "jit_loop": 2000, "jit_calls": 460,
    "misc_classes": 600, "misc_import": 160, "misc_cext": 200,
}
for _k, _v in UNITS.items():
    REGISTRY[_k].units = _v


def get(name):
    return REGISTRY[name]()
