# (perf8) Generator expressions consumed by builtins (deepcopy's `all(a is b for a, b
# in zip(x, y))`, comprehension-style any/sum/tuple). usage: genx.py N KIND
import sys

X = [1, "a", None, 2.5]
Y = list(X)


def allzip(n):
    c = 0
    x, y = X, Y
    for _ in range(n):
        c += all(a is b for a, b in zip(x, y))
    return c


def sumgen(n):
    c = 0
    x = X
    for _ in range(n):
        c += sum(1 for a in x if a)
    return c


def tuplegen(n):
    c = 0
    x = X
    for _ in range(n):
        c += len(tuple(a for a in x))
    return c


def zipiter(n):
    c = 0
    x, y = X, Y
    for _ in range(n):
        for a, b in zip(x, y):
            c += a is b
    return c


globals()[sys.argv[2]](int(sys.argv[1]))
