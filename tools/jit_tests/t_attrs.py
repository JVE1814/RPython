# Attribute access in compiled code: inline values (shapes), dict-mode objects,
# __slots__, class attributes changing, methods shadowed by instance attributes,
# module attributes rebinding, stores from several threads.
import math
import threading


class P:
    def __init__(self, x, y):
        self.x = x
        self.y = y

    def norm(self):
        return self.x * self.x + self.y * self.y

    def move(self, d):
        self.x += d
        self.y -= d


class S:
    __slots__ = ('a', 'b')

    def __init__(self):
        self.a = 1
        self.b = 2


def work(ps, n):
    t = 0
    for i in range(n):
        for p in ps:
            p.move(1)
            t += p.norm() + p.x - p.y
    return t


ps = [P(i, -i) for i in range(5)]
for k in range(30):
    r = work(ps, 20)
print(r, [(p.x, p.y) for p in ps])

# shape changes: an extra attribute, an object in dict mode, deletion
ps[1].z = 5
vars(ps[2])
del ps[3].y
ps[3].y = 7
for k in range(5):
    r = work(ps, 20)
print(r, [(p.x, p.y) for p in ps])

# instance attribute shadowing a method
class Q(P):
    pass

qs = [Q(1, 2), Q(3, 4)]
def calls(qs, n):
    out = 0
    for i in range(n):
        for q in qs:
            out += q.norm()
    return out
for k in range(40):
    r = calls(qs, 10)
qs[1].norm = lambda: 1000
print(r, calls(qs, 10))
P.norm = lambda self: -1
print(calls(qs, 10), calls([Q(0, 0)], 3))

# class attribute changes
class C:
    k = 3
def readk(objs, n):
    s = 0
    for i in range(n):
        for o in objs:
            s += o.k
    return s
cs = [C(), C()]
for k in range(40):
    r = readk(cs, 10)
C.k = 4
print(r, readk(cs, 10))
cs[0].k = 10
print(readk(cs, 10))

# __slots__
def slots(ss, n):
    t = 0
    for i in range(n):
        for s in ss:
            s.a = s.a + s.b
            t += s.a
    return t
ss = [S(), S()]
for k in range(40):
    r = slots(ss, 10)
print(r)
try:
    del ss[0].a
    slots(ss, 1)
except AttributeError as e:
    print("AttributeError", e)

# module attributes folded and rebound
def roots(n):
    t = 0.0
    for i in range(n):
        t += math.sqrt(i)
    return t
for k in range(40):
    r = roots(50)
print(round(r, 6))
math.sqrt = lambda x: 1.0
print(roots(50))
del math.sqrt
try:
    roots(2)
except AttributeError as e:
    print("AttributeError", e)

# stores from several threads (locked path)
class Ctr:
    def __init__(self):
        self.n = 0
        self.m = 0
def bump(c, n):
    for i in range(n):
        c.n = i
        c.m = c.n + 1
c = Ctr()
for k in range(30):
    bump(c, 30)
ts = [threading.Thread(target=bump, args=(c, 2000)) for _ in range(3)]
for t in ts:
    t.start()
for t in ts:
    t.join()
print(c.n, c.m)

# many attributes (dict mode from the start)
class Big:
    def __init__(self):
        for i in range(40):
            setattr(self, 'a%d' % i, i)
def big(bs, n):
    t = 0
    for i in range(n):
        for b in bs:
            b.a3 = b.a3 + 1
            t += b.a39
    return t
bs = [Big(), Big()]
for k in range(40):
    r = big(bs, 5)
print(r, bs[0].a3)

# polymorphic receivers and attribute-adding stores in __init__
class A:
    def __init__(self, v):
        self.v = v
        self.w = v * 2
    def get(self):
        return self.v
class B(A):
    def __init__(self, v):
        self.w = v
        self.v = v + 1
    def get(self):
        return -self.v
class D:
    __slots__ = ('v', 'w')
    def __init__(self, v):
        self.v = v
        self.w = 0
    def get(self):
        return self.v * 10
def poly(n):
    t = 0
    for i in range(n):
        for o in (A(i), B(i), D(i), A(-i)):
            t += o.v + o.w + o.get()
            o.w = i
            t += o.w
    return t
for k in range(40):
    r = poly(10)
print(r)
class E(A):
    pass
print(poly(3), [o.get() for o in (E(1), A(2))])
