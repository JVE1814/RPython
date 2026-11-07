# Call-path micro benchmark (perf4): one kind of call per run, selected by argv[2].
# usage: calls.py N KIND   (cost per call = (I(N2) - I(N1)) / (N2 - N1))
import sys


def kw(a, b=1):
    return a


def defaults(a, b=1, c=2):
    return a


def star(*a):
    return a


def kwargs(**k):
    return k


def posmany(a, b, c, d):
    return a


class C:
    def __init__(self, x):
        self.x = x

    def meth(self, y):
        return y

    @property
    def p(self):
        return self.x


class D(C):
    def __init__(self, x):
        super().__init__(x)


class Eq:
    __slots__ = ('v',)

    def __init__(self, v):
        self.v = v

    def __eq__(self, o):
        return self.v == o.v

    def __hash__(self):
        return self.v


def ident(x):
    return x


def run(n, kind):
    o = C(1)
    if kind == 'kw':
        for i in range(n):
            kw(i, b=2)
    elif kind == 'defaults':
        for i in range(n):
            defaults(i)
    elif kind == 'star':
        t = (1, 2)
        for i in range(n):
            star(*t)
    elif kind == 'kwargs':
        d = {'a': 1}
        for i in range(n):
            kwargs(**d)
    elif kind == 'init':
        for i in range(n):
            C(i)
    elif kind == 'super':
        for i in range(n):
            D(i)
    elif kind == 'prop':
        for i in range(n):
            o.p
    elif kind == 'bound':
        m = o.meth
        for i in range(n):
            m(i)
    elif kind == 'map':
        data = list(range(100))
        for i in range(n // 100):
            list(map(ident, data))
    elif kind == 'sortkey':
        data = list(range(100))
        for i in range(n // 100):
            sorted(data, key=ident)
    elif kind == 'dunder':
        keys = [Eq(i) for i in range(8)]
        d = {k: 1 for k in keys}
        probe = [Eq(i) for i in range(8)]
        for i in range(n // 8):
            for k in probe:
                d[k]
    elif kind == 'getattr_call':
        for i in range(n):
            getattr(o, 'meth')(i)
    elif kind == 'posmany':
        for i in range(n):
            posmany(i, i, i, i)
    elif kind == 'alloc':
        for i in range(n):
            (i, i)
            [i]
    else:
        raise SystemExit('unknown kind ' + kind)


run(int(sys.argv[1]), sys.argv[2])
