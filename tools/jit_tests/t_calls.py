# perf4: argument binding shapes through the compiled call paths (keywords, defaults,
# *args/**kwargs, bound methods, errors), compared against the interpreter.

def f0(a, b=2, *, c=3):
    return (a, b, c)

def f1(*args):
    return args

def f2(**kw):
    return sorted(kw.items())

def f3(a, /, b, *args, c, d=4, **kw):
    return (a, b, args, c, d, sorted(kw.items()))

def f4(a, b, c=10, d=20):
    return a + b + c + d

class K:
    def m(self, x, y=1, **kw):
        return (x, y, sorted(kw.items()))

def tryit(fn, *a, **k):
    try:
        return fn(*a, **k)
    except TypeError as e:
        return 'TypeError: ' + str(e)

def run():
    out = []
    k = K()
    t = (1, 2)
    d = {'c': 5}
    for i in range(300):
        out.append(f0(i))
        out.append(f0(i, b=i))
        out.append(f0(i, c=i, b=1))
        out.append(f0(*t, **d))
        out.append(f1(*t))
        out.append(f1(i, i))
        out.append(f1())
        out.append(f2(**d))
        out.append(f2(x=i, **d))
        out.append(f3(1, 2, 3, 4, c=5))
        out.append(f3(1, b=2, c=3, e=6))
        out.append(f3(*t, **{'c': 1, 'a': 9}))
        out.append(f4(1, 2))
        out.append(f4(1, 2, 3))
        out.append(f4(*t, d=1))
        out.append(k.m(*t))
        out.append(k.m(*t, z=3))
        bm = k.m
        out.append(bm(1, **{'y': 2, 'w': 0}))
        if i % 50 == 0:
            out.append(tryit(f0))
            out.append(tryit(lambda: f0(1, b=2, **{'b': 3})))
            out.append(tryit(f0, 1, **{'b': 2, 'q': 1}))
            out.append(tryit(f0, *t, **{'b': 3}))
            out.append(tryit(f3, 1, 2, **{'a': 3, 'c': 1}))
            out.append(tryit(f3, **{'b': 1, 'c': 2}))
            out.append(tryit(f4, *(1,)))
            out.append(tryit(f4, *(1, 2, 3, 4, 5)))
            out.append(tryit(f1, **{'x': 1}))
            out.append(tryit(f2, *t))
            out.append(tryit(lambda: f2(**{1: 2})))
            out.append(tryit(f0, 1, **{'c': 1, 'd': 2}))
    return out

r = run()
print(len(r))
for x in r[:60]:
    print(x)
print(r[-30:])
