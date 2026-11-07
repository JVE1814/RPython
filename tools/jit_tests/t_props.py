# perf5: property getters called through the machine-code call routine, and bound
# method objects unpacked at compiled Call sites.
import types


class A:
    def __init__(self, x):
        self.x = x

    @property
    def p(self):
        return self.x + 1

    @property
    def bad(self):
        raise ValueError(self.x)

    @property
    def miss(self):
        raise AttributeError('miss')

    def m(self, y, z=3, *rest):
        return (self.x, y, z, rest)


class G(A):
    @property
    def miss(self):
        raise AttributeError('gmiss')

    def __getattr__(self, name):
        return ('fallback', name)


def defaults_getter(self, k=10):
    return self.x * k


class B:
    def __init__(self, x):
        self.x = x
    q = property(defaults_getter)
    r = property(lambda self: -self.x)
    s = property(len)             # builtin getter


class L(list):
    @property
    def n(self):
        return len(self)


def reads(o, n):
    t = 0
    for i in range(n):
        t += o.p
    return t


def run():
    out = []
    a = A(1)
    for i in range(300):
        a.x = i
        out.append(a.p)
        try:
            a.bad
        except ValueError as e:
            out.append(e.args)
        try:
            a.miss
        except AttributeError as e:
            out.append(str(e))
    g = G(5)
    for i in range(200):
        out.append(g.miss)
        out.append(g.p)
    b = B(3)
    for i in range(200):
        out.append((b.q, b.r))
    lst = L([1, 2, 3])
    for i in range(200):
        out.append(lst.n)
    # property redefined on the class while compiled code caches it
    out.append(reads(a, 200))
    A.p = property(lambda self: 'new')
    out.append([a.p for _ in range(3)])
    del A.p
    try:
        a.p
    except AttributeError as e:
        out.append(('gone', str(e)))
    A.p = property(lambda self: self.x * 2)
    out.append(reads(A(4), 50))
    # getter replaced in place through __init__
    pr = property(lambda self: 'one')
    A.z = pr
    vals = []
    for i in range(200):
        if i == 100:
            pr.__init__(lambda self: 'two')
        vals.append(a.z)
    out.append((vals.count('one'), vals.count('two')))
    # bound method objects
    m = a.m
    for i in range(300):
        out.append(m(i))
        out.append(m(i, 7))
        out.append(m(i, 7, 8, 9))
    bm = types.MethodType(len, [1, 2])
    bm2 = types.MethodType(defaults_getter, B(2))
    for i in range(200):
        out.append(bm())
        out.append(bm2())
        out.append(bm2(i))
    try:
        for i in range(200):
            m()
    except TypeError as e:
        out.append(str(e))
    return out


res = run()
print(len(res))
for r in res:
    print(r)
