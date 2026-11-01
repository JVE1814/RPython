log = []
def L(x):
    log.append(x)
    return x
# evaluation order
d = {}
d[L("k")] = L("v"); print(log); log.clear()
L([1])[L(0)] += L(5); print(log); log.clear()
print(L(1) < L(2) < L(0) < L(9), log); log.clear()
def kwo(*, a=L("a"), b=L("b")): pass
print(log); log.clear()
f = lambda: 0
f(L(1), *[L(2)], k=L(3), **{L("x"): L(4)}) if False else None
# return in finally overrides
def rf():
    try:
        return 1
    finally:
        return 2
print(rf())
def nested_finally():
    out = []
    for i in [1, 2]:
        try:
            try:
                if i == 1:
                    continue
                return out
            finally:
                out.append(("inner", i))
        finally:
            out.append(("outer", i))
print(nested_finally())
def exc_in_loop():
    r = []
    for x in [1, 0, 2]:
        try:
            r.append(10 // x)
        except ZeroDivisionError as e:
            r.append("zde")
            continue
        else:
            r.append("ok")
    return r
print(exc_in_loop())
def gen_finally():
    try:
        yield 1
        yield 2
    finally:
        print("gen cleanup")
g = gen_finally(); print(next(g)); g.close(); print("closed")
def unbound():
    try:
        print(zz)
    except NameError as e:
        print(type(e).__name__)
    zz = 1
unbound()
def e_deleted():
    try:
        raise ValueError
    except ValueError as e:
        pass
    try:
        e
    except NameError:
        print("e unbound after except")
e_deleted()
def outer():
    a = 1
    def mid():
        def inner():
            nonlocal a
            a += 10
        inner()
        return a
    return mid()
print(outer())
class Meta(type):
    def __new__(m, name, bases, ns, **kw):
        ns["kw"] = kw
        return super().__new__(m, name, bases, ns)
class WithMeta(metaclass=Meta, flag=1): pass
print(WithMeta.kw)
def cdeco(c):
    c.tag = "tagged"
    return c
@cdeco
class D: pass
print(D.tag)
class Base:
    def __init_subclass__(cls, **kw):
        cls.sub_kw = kw
class Sub(Base, x=3): pass
print(Sub.sub_kw)
class SupSuper:
    def m(self): return "base"
class SupChild(SupSuper):
    def m(self):
        return [super().m() for _ in [1]] + [(lambda: super(SupChild, self).m())()]
print(SupChild().m())
def w_loop():
    class Sup:
        def __enter__(self): return self
        def __exit__(self, *a): return True
    n = 0
    for i in [1, 2, 3]:
        with Sup():
            n += i
            if i == 2:
                break
            raise KeyError
    return n
print(w_loop())
x = 5
def gl():
    global x
    x = 6
    def inner():
        return x
    return inner()
print(gl(), x)
print([*map(lambda v: v * 2, [1, 2])], (lambda *a, **k: (a, k))(1, b=2))
a = b = c = [0]
print(a is b is c)
(p, q), r = (1, 2), 3; print(p, q, r)
try:
    raise TypeError("a") from ValueError("b")
except TypeError as ex:
    print(type(ex.__cause__).__name__)
assert True, "never"
try:
    assert 1 == 2, "msg here"
except AssertionError as ae:
    print("assert", ae)
