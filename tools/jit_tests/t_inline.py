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
