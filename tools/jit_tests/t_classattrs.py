# perf5: class-object attributes under metatypes derived from `type` (plain values,
# classmethods, staticmethods), namedtuple fields, type(x) at call sites.
import abc
import collections


class Meta(type):
    pass


class A(metaclass=abc.ABCMeta):
    k = 1

    @classmethod
    def make(cls, x):
        return (cls.__name__, x)

    @staticmethod
    def st(x):
        return x * 2


class B(A):
    pass


class C(metaclass=Meta):
    v = 'c'

    @classmethod
    def cm(cls):
        return cls.v


Pt = collections.namedtuple('Pt', 'x y z')


class Pt2(Pt):
    @property
    def norm(self):
        return self.x + self.y + self.z


def run():
    out = []
    for i in range(300):
        out.append(A.make(i))
        out.append(B.make(i))
        out.append(A.st(i))
        out.append((A.k, B.k, C.v, C.cm()))
        m = A.make
        out.append(m(-i))
        p = Pt(i, i + 1, i + 2)
        q = Pt2(1, 2, i)
        out.append((p.x, p.y, p.z, q.z, q.norm))
        out.append((type(i), type(p) is Pt, type(q).__name__))
        if i == 100:
            A.k = 'changed'
            B.make = classmethod(lambda cls, x: ('B!', x))
        if i == 150:
            Meta.v = property(lambda cls: 'meta-data-descr')
        if i == 200:
            A.st = staticmethod(lambda x: -x)
            C.cm = classmethod(lambda cls: 'new cm')
        if i == 250:
            del Meta.v
    try:
        Pt.x.__get__(5)
    except TypeError as e:
        out.append(str(e))
    return out


for r in run():
    print(r)
