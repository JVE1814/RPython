# Inline-cache invalidation after warm-up (functions get compiled with caches filled,
# then the guarded assumptions change).
import sys

class A:
    def __init__(self, x):
        self.x = x
    def m(self):
        return 'A.m'

class B(A):
    pass

class S:
    __slots__ = ('x',)
    def __init__(self, x):
        self.x = x
    def m(self):
        return 'S.m'

def get_x(o):
    return o.x

def call_m(o):
    return o.m()

def set_x(o, v):
    o.x = v

out = []
objs = [A(1), B(2), S(3)]
for rnd in range(4):
    for i in range(300):
        o = objs[i % 3] if rnd >= 2 else objs[0]
        out.append(get_x(o))
        out.append(call_m(o))
        set_x(o, i)
    if rnd == 1:
        A.m = lambda self: 'patched'
        objs[0].m = lambda: 'instance'
    if rnd == 2:
        del objs[0].m
        B.x = 'class-x'
        del objs[1].x
print(len(out), out[:6], out[-12:], sum(1 for v in out if v == 'patched'))

M = 10
def read_global():
    return M + len('abc')

r = []
for i in range(500):
    r.append(read_global())
    if i == 250:
        M = 20
    if i == 300:
        import builtins
        orig_len = builtins.len
        builtins.len = lambda s: -1
    if i == 350:
        builtins.len = orig_len
print(sum(r), r[249:253], r[299:303], r[349:353])

def modattr(m):
    return m.maxsize

r = []
for i in range(300):
    r.append(modattr(sys))
    if i == 150:
        sys.maxsize = 5
print(r[149:153])

class P:
    @property
    def v(self):
        return 42
    cnt = 0

def pv(o):
    return o.v + P.cnt

r = []
for i in range(300):
    r.append(pv(P()))
    if i == 100:
        P.cnt = 1
    if i == 200:
        P.v = property(lambda self: 7)
print(r[99:102], r[199:202])

class Q:
    pass

def qx(o):
    return o.x

qs = [Q() for _ in range(3)]
for k, q in enumerate(qs):
    q.x = k
    q.y = k * 2
r = []
for i in range(300):
    q = qs[i % 3]
    r.append(qx(q))
    if i == 150:
        del qs[0].x
        qs[0].x = 'readded'
print(r[:6], r[-6:])

def unpack_types(v):
    a, b = v
    return a + b

r = []
for i in range(300):
    v = (i, i) if i % 2 else [i, 1]
    r.append(unpack_types(v))
print(sum(r))

def arith_types(a, b):
    return a + b, a - b, a * b, a < b, a == b

r = []
vals = [1, 2.5, 2 ** 40, -3, 'x', True, 10 ** 30]
for i in range(400):
    a = vals[i % 7]
    b = vals[(i // 7) % 7]
    try:
        r.append(arith_types(a, b))
    except TypeError as e:
        r.append(str(e))
print(len(r), r[:3], r[-3:])
