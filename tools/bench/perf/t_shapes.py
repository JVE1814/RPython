# perf-runtime: semantics checks for inline attribute values (shape.rs).
# Run: PY tools/bench/perf/t_shapes.py  -> prints OK (exit status 1 on failure)
import gc, sys, weakref, copy, pickle

fails = []
def check(cond, what):
    if not cond:
        fails.append(what)

class P:
    def __init__(self, x, y):
        self.x = x
        self.y = y

p = P(1, 2)
check(p.x == 1 and p.y == 2, "basic get")
p.x = 10
check(p.x == 10, "replace")
p.z = 3
check(vars(p) == {'x': 10, 'y': 2, 'z': 3}, "vars order %r" % vars(p))
d = p.__dict__
check(d is p.__dict__, "dict identity")
d['w'] = 4
check(p.w == 4, "dict write reflects")
p.v = 5
check(d['v'] == 5, "attr write reflects in dict")
check(list(d) == ['x', 'y', 'z', 'w', 'v'], "order after materialise %r" % list(d))
del p.x
check('x' not in d and not hasattr(p, 'x'), "delete after materialise")

# deletion orders without materialisation
q = P(1, 2)
del q.y
check(not hasattr(q, 'y') and q.x == 1, "delete last")
q.y = 7
check(list(vars(q)) == ['x', 'y'], "re-add order")
r = P(1, 2)
del r.x
check(list(vars(r)) == ['y'] and r.y == 2, "delete first %r" % vars(r))
try:
    del r.x
    check(False, "double delete should raise")
except AttributeError:
    pass
s = P(1, 2)
try:
    del s.nope
    check(False, "delete missing should raise")
except AttributeError as e:
    check("nope" in str(e), "delete missing message")

# many attributes (overflow)
class C: pass
c = C()
for i in range(100):
    setattr(c, "a%d" % i, i)
check(all(getattr(c, "a%d" % i) == i for i in range(100)), "100 attrs")
check(list(vars(c)) == ["a%d" % i for i in range(100)], "100 attrs order")

# non-interned / str-subclass names
class S(str): pass
e = P(1, 2)
setattr(e, "".join(["x"]), 5)
check(e.x == 5, "non-interned name set")
check(getattr(e, "".join(["y"])) == 2, "non-interned name get")
check(getattr(e, S("x")) == 5, "str subclass get")
setattr(e, S("q"), 9)
check(e.q == 9, "str subclass set")

# __dict__ assignment / deletion
f = P(1, 2)
f.__dict__ = {'a': 1}
check(f.a == 1 and not hasattr(f, 'x'), "dict assign")
g = P(1, 2)
del g.__dict__
check(vars(g) == {}, "dict delete")
g.k = 1
check(g.k == 1, "store after dict delete")

# __class__ assignment
class Q:
    def __init__(self):
        self.a = 1
h = P(1, 2)
h.__class__ = Q
check(type(h) is Q and h.x == 1 and vars(h) == {'x': 1, 'y': 2}, "class assign")

# class attribute shadowing / methods
class M:
    def m(self): return "method"
    attr = "class"
m = M()
check(m.m() == "method" and m.attr == "class", "class attrs")
m.m = lambda: "inst"
m.attr = "inst"
check(m.m() == "inst" and m.attr == "inst", "shadowing")
del m.m
check(m.m() == "method", "unshadow")

# data descriptors win over instance values
class D:
    @property
    def x(self): return "prop"
dd = D()
try:
    dd.x = 1
    check(False, "property without setter")
except AttributeError:
    pass
check(dd.x == "prop", "property get")

# weakrefs and GC of instances with cycles
class N:
    pass
n = N()
n.self = n
w = weakref.ref(n)
del n
gc.collect()
check(w() is None, "cycle via inline value collected")

# finalizer sees attributes
log = []
class F:
    def __init__(self): self.v = 42
    def __del__(self): log.append(self.v)
F()
gc.collect()
check(log == [42], "del sees attrs %r" % log)

# copy / pickle
cp = copy.copy(P(1, 2))
check(vars(cp) == {'x': 1, 'y': 2}, "copy")
dp = copy.deepcopy(P([1], 2))
check(dp.x == [1], "deepcopy")
pk = pickle.loads(pickle.dumps(P(3, 4)))
check(vars(pk) == {'x': 3, 'y': 4}, "pickle")

# subclass with __slots__ and inherited inline values
class B(P):
    __slots__ = ('s',)
b = B(1, 2)
b.s = 5
b.t = 6
check((b.x, b.y, b.s, b.t) == (1, 2, 5, 6), "slots+values")
check(vars(b) == {'x': 1, 'y': 2, 't': 6}, "slots+values vars")

# instantiation fast path semantics
class R:
    def __init__(self):
        return 1
try:
    R()
    check(False, "__init__ returning non-None")
except TypeError as ex:
    check("should return None" in str(ex), "init return msg")
class K:
    def __init__(self, a, *, b=2):
        self.a = a; self.b = b
k = K(1, b=3)
check((k.a, k.b) == (1, 3), "kw init")
try:
    K()
    check(False, "missing arg")
except TypeError as ex:
    check("__init__()" in str(ex), "missing arg msg %s" % ex)
class Meta(type):
    def __call__(cls, *a):
        return ("meta", a)
class X(metaclass=Meta):
    def __init__(self, v): self.v = v
check(X(1) == ("meta", (1,)), "metaclass __call__")
class NewC:
    def __new__(cls, v):
        o = object.__new__(cls); o.n = v; return o
    def __init__(self, v): self.i = v
nc = NewC(4)
check((nc.n, nc.i) == (4, 4), "custom __new__")
import abc
class Abs(abc.ABC):
    @abc.abstractmethod
    def f(self): pass
    def __init__(self): pass
try:
    Abs()
    check(False, "abstract")
except TypeError:
    pass
# patching __init__ after instances exist
class Pt:
    def __init__(self): self.a = 1
Pt()
Pt.__init__ = lambda self: setattr(self, 'b', 2)
check(vars(Pt()) == {'b': 2}, "patched init")

# hot loop through the inline caches / JIT
class H:
    def __init__(self, i):
        self.i = i
        self.j = i * 2
    def get(self):
        return self.i + self.j
def hot(n):
    s = 0
    for i in range(n):
        h = H(i)
        s += h.get() + h.i
        h.j = 0
        s += h.j
    return s
check(hot(2000) == sum(3 * i + i for i in range(2000)), "hot loop")

if fails:
    print("FAIL", fails)
    sys.exit(1)
print("OK")
