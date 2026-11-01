def gen(n):
    i = 0
    while i < n:
        yield i * 2
        i += 1
print(list(gen(4)))
def g2():
    x = yield from gen(2)
    yield "done"
print(list(g2()))
print([x*y for x in [0, 1, 2] for y in (1, 2) if x], {k: v for k, v in [(1, 2)]}, {c for c in "aa"})
print(list(x + 1 for x in [1, 2]))
a, *b, c = [0, 1, 2, 3, 4]; print(a, b, c)
x = 10
print([x for x in [1]], x)
def f():
    y = 5
    fs = [lambda: y for y in [1, 2]]
    return [h() for h in fs], y
print(f())
class C:
    v = 3
    def m(self): return self.v
    @staticmethod
    def s(): return "s"
print(C().m(), C.s(), C.__qualname__, C.m.__qualname__, C.__static_attributes__ if hasattr(C, "__static_attributes__") else None)
try:
    try:
        raise ValueError("x")
    finally:
        print("inner finally")
except ValueError as e:
    print("caught", e)
def ret_in_finally():
    try:
        return 1
    finally:
        print("finally runs")
print(ret_in_finally())
def loop_break():
    out = []
    for i in [1, 2, 3, 4]:
        try:
            if i == 3:
                break
            if i == 1:
                continue
            out.append(i)
        finally:
            out.append(-i)
    return out
print(loop_break())
class CM:
    def __enter__(self): print("enter"); return self
    def __exit__(self, *a): print("exit", a[0]); return True
with CM() as cm:
    raise KeyError(1)
print("after with")
def wr():
    with CM():
        return "ret"
print(wr())
print(f"{'nested'!r} {3+4:>4} {x=}")
import sys as s2
print(s2.__name__)
def kw(a, /, b, *, c=3): return (a, b, c)
print(kw(1, b=2), kw(1, 2, c=4))
d = {"a": 1, **{"b": 2}}
print(d, [*d, *"xy"], (*d,))
print(not 0, 1 if [] else 2, [] or "e", 5 and 6)
del d["a"]; print(d)
n = 0
while n < 3:
    n += 1
else:
    print("while else", n)
def deco(f): return lambda *a: f(*a) * 2
@deco
def dbl(v): return v
print(dbl(21))
x: int = 3
print(__annotate__(1) if '__annotate__' in dir() else "noann")
class A:
    def f(self): return "A"
class B(A):
    def f(self): return "B" + super().f()
print(B().f())
