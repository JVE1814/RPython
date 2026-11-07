def f0():
    return 0

def f2(a, b=10, *args, c=5, **kw):
    return (a, b, args, c, sorted(kw.items()))

def rec(n):
    return n if n < 2 else rec(n - 1) + rec(n - 2)

def kwcalls(n):
    out = []
    for i in range(n):
        out.append(f2(i))
        out.append(f2(i, 2))
        out.append(f2(i, 2, 3, 4))
        out.append(f2(i, c=1))
        out.append(f2(a=i, b=3, z=9))
        out.append(f2(*[i, 1, 2], **{'c': 7, 'y': 1}))
        out.append(f0())
    return out

print(kwcalls(3))
print([rec(i) for i in range(15)])

def make_adders(n):
    fs = []
    for i in range(n):
        def add(x, i=i):
            return x + i
        fs.append(add)
        fs.append(lambda x, i=i: x * i)
    return [f(10) for f in fs]

print(make_adders(5))

def closures(n):
    total = 0
    def inc(k):
        nonlocal total
        total += k
        return total
    for i in range(n):
        inc(i)
    def gen_cl():
        return [lambda: total + j for j in range(3)]
    return total, [g() for g in gen_cl()]

print(closures(100))

class C:
    cls_attr = 5
    def __init__(self, v):
        self.v = v
    def get(self):
        return self.v
    @property
    def p(self):
        return self.v * 2
    @staticmethod
    def s(x):
        return x + 1
    @classmethod
    def c(cls, x):
        return (cls.__name__, x)
    def __add__(self, o):
        return C(self.v + o.v)
    def __eq__(self, o):
        return isinstance(o, C) and self.v == o.v
    def __hash__(self):
        return hash(self.v)

class D(C):
    def get(self):
        return -super().get()

def methods(n):
    out = []
    a = C(1)
    for i in range(n):
        b = D(i)
        out.append((a.get(), b.get(), b.p, b.s(i), b.c(i), C.c(i), (a + b).v, a == b, b.cls_attr))
        if i == 2:
            b.get = lambda: 'shadowed'
            out.append(b.get())
        if i == 3:
            C.cls_attr = 7
    return out

print(methods(6))

class S:
    __slots__ = ('x', 'y')
    def __init__(self):
        self.x = 1

def slots(n):
    out = []
    for i in range(n):
        s = S()
        s.x = i
        try:
            out.append(s.y)
        except AttributeError as e:
            out.append('AE')
        s.y = i * 2
        out.append(s.x + s.y)
    return out

print(slots(5))

def builtins_shadow(n):
    out = []
    for i in range(n):
        out.append(len([1] * i))
    return out

print(builtins_shadow(4))
G = 1
def globals_change(n):
    global G
    out = []
    for i in range(n):
        out.append(G)
        if i == 2:
            G = 'changed'
    return out

print(globals_change(5))

def ns():
    import sys as _s
    m = _s
    out = []
    for i in range(3):
        out.append(m.maxsize > 0)
    return out

print(ns())

# jit5: direct calls that fill trailing parameters from __defaults__
def dflt(a, b=10, c=[1]):
    return a + b + len(c)

def call_dflt(n):
    t = 0
    for i in range(n):
        t += dflt(i) + dflt(i, 2) + dflt(i, 2, [])
        if i == 150:
            dflt.__defaults__ = (100, [1, 2, 3])
        if i == 250:
            dflt.__defaults__ = (7, 8, 1000, [])  # more defaults than parameters
    return t

print(call_dflt(400))
try:
    dflt.__defaults__ = None
    dflt(1)
except TypeError as e:
    print('TypeError', e)
