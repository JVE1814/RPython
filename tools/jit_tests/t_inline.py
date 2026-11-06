# Small callees inlined by tier 2: results, guard failures inside the callee
# (the callee is finished in the interpreter), exceptions and tracebacks from
# inlined code, rebinding the callee, methods and plain functions.
import sys
import traceback


def add(a, b):
    return a + b

def scale(p, k):
    return p.x * k + p.y

def getx(p):
    return p.x

def setx(p, v):
    p.x = v
    return v


class P:
    def __init__(self, x, y):
        self.x = x
        self.y = y

    def norm2(self):
        return self.x * self.x + self.y * self.y

    def bump(self, d):
        self.x = self.x + d
        return self.x


def loop(ps, n):
    t = 0
    for i in range(n):
        for p in ps:
            t = add(t, scale(p, 2))
            t = add(t, p.norm2())
            p.bump(1)
            setx(p, getx(p) - 1)
    return t


ps = [P(i, i + 1) for i in range(4)]
for k in range(60):
    r = loop(ps, 10)
print(r)

# type changes inside the inlined callee: floats, big ints, strings
ps2 = [P(1.5, 2), P(2 ** 62, 3), P(7, 1)]
print(loop(ps2, 3))
print(add("a", "b"), add([1], [2]), add(2 ** 63, 1))
for k in range(60):
    r = loop(ps, 2)
q = P("s", "t")
try:
    loop([q], 1)
except TypeError as e:
    print("TypeError", e)

# exception raised inside the inlined callee: traceback shows the callee frame
def bad(p):
    return p.missing

def call_bad(ps, n):
    out = 0
    for i in range(n):
        for p in ps:
            out += getx(p)
    try:
        bad(ps[0])
    except AttributeError as e:
        tb = traceback.extract_tb(e.__traceback__)
        return out, [f.name for f in tb]

for k in range(60):
    r = call_bad(ps, 5)
print(r)

def div(a, b):
    return a // b

def divs(n):
    t = 0
    for i in range(1, n):
        t += div(100, i)
    return t

for k in range(60):
    divs(20)
try:
    print(divs(5))
    def zero(n):
        t = 0
        for i in range(n):
            t += div(10, i - 3)
        return t
    for k in range(60):
        try:
            zero(10)
        except ZeroDivisionError as e:
            last = e
    print("ZeroDivisionError", last, [f.name for f in traceback.extract_tb(last.__traceback__)])
except ZeroDivisionError as e:
    print("outer", e)

# rebinding an inlined function
for k in range(60):
    r = divs(10)
def div(a, b):
    return -1
print(r, divs(10))

# frame introspection from a function called by inlined code is not possible
# (inlined callees make no calls); sys._getframe in the caller still works
def who():
    return sys._getframe().f_code.co_name
print(who())

# callees with branches (paths duplicated, results meet in one value)
def sign(x):
    if x > 0:
        return 1
    elif x < 0:
        return -1
    return 0

def pick(o, a, b):
    if o is None:
        return a
    if o:
        return b
    return a + b

class Dir:
    def __init__(self, fwd, v1, v2):
        self.fwd = fwd
        self.v1 = v1
        self.v2 = v2

    def out(self):
        if self.fwd:
            return self.v2
        else:
            return self.v1

    def scaled(self, k):
        if k == 0:
            return None
        r = self.v1 * k
        if r > 100:
            r = 100
        return r

def branchy(n):
    t = 0
    ds = [Dir(i % 2 == 0, i, -i) for i in range(5)]
    for i in range(n):
        t += sign(i - n // 2)
        t += pick(None if i % 3 == 0 else i % 2 == 0, i, 2)
        d = ds[i % 5]
        t += d.out()
        s = d.scaled(i % 4)
        if s is not None:
            t += s
    return t

for k in range(60):
    r = branchy(50)
print(r, branchy(51))
Dir.out = lambda self: 1000
print(branchy(50))

# type change inside one path of an inlined branchy callee (deopt), then exceptions
def half(x, big):
    if big:
        return x / 2
    return x // 2

def use_half(vals):
    t = 0
    for i, v in enumerate(vals):
        t += half(v, i % 2 == 0)
    return t

class Vec2:
    def __init__(self, x, y):
        self.x = x
        self.y = y

    def check(self):
        return self

    def dot(self, o):
        o.check()
        return self.x * o.x + self.y * o.y

    def norm2(self):
        return self.dot(self)

def nested(vs):
    t = 0
    for v in vs:
        t += v.norm2() + sign(v.dot(vs[0]))
    return t

vs = [Vec2(i, -i) for i in range(8)]
for k in range(60):
    r = nested(vs)
print(r)
vs[3].y = 2.5  # float inside a doubly inlined callee
print(nested(vs))
Vec2.check = lambda self: 1 / 0
try:
    nested(vs)
except ZeroDivisionError as e:
    print("ZeroDivisionError", [f.name for f in traceback.extract_tb(e.__traceback__)])

# inlined constructors: object.__new__ + inlined __init__
class P:
    alive = 0
    def __init__(self, x, y):
        if x < 0:
            raise ValueError("negative %d" % x)
        self.x = x
        self.y = y
        P.alive += 1

    def __del__(self):
        P.alive -= 1

    def plus(self, o):
        return P(self.x + o.x, self.y + o.y)

def build(n):
    acc = P(0, 0)
    for i in range(n):
        acc = acc.plus(P(i, 1))
    return acc.x, acc.y

for k in range(60):
    r = build(30)
print(r, P.alive)
def bad(n):
    out = []
    for i in range(n):
        try:
            out.append(P(i - 3, 0).x)
        except ValueError as e:
            out.append(str(e))
    return out
for k in range(60):
    r = bad(6)
print(r, P.alive)
P.__init__ = lambda self, x, y: 5
try:
    build(3)
except TypeError as e:
    print("TypeError", e)
def init2(self, x, y):
    self.x = x * 10
    self.y = y
P.__init__ = init2
print(build(5), P.alive)

for k in range(60):
    r = use_half(list(range(20)))
print(r, use_half([1.5, 2, 3.25, 4, 5]))
try:
    print(use_half([1, 2, None]))
except TypeError as e:
    print("TypeError", [f.name for f in traceback.extract_tb(e.__traceback__)])
