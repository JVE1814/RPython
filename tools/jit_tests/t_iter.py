# Iteration fast paths (range/list/tuple iterators) incl. mutation during iteration.
def ranges():
    out = []
    for args in [(0,), (5,), (3, 10), (10, 3, -2), (-5, 5, 3), (0, 0), (2 ** 40, 2 ** 40 + 3), (-2 ** 31, -2 ** 31 + 2)]:
        s = 0
        for i in range(*args):
            s += i
        out.append(s)
    return out

def lists():
    out = []
    l = [1, 2, 3]
    for x in l:
        out.append(x)
        if x < 6:
            l.append(x + 3)
    l2 = [1, 2, 3, 4]
    for x in l2:
        out.append(x)
        if x == 2:
            l2.clear()
    l3 = [5, 6, 7]
    for x in l3:
        out.append(x)
        del l3[0]
    return out

def tuples():
    out = 0
    for t in [(), (1,), (1, 2, 3), tuple(range(50))]:
        for x in t:
            out += x
    return out

def nested(n):
    s = 0
    for i in range(n):
        for j in (i, i + 1):
            for k in [j, -j]:
                s += k * i
    return s

def gens(n):
    def g(m):
        for i in range(m):
            yield i * 2
    return sum(g(n)), [x for x in g(5)], list(zip(range(3), 'abc'))

for _ in range(150):
    r = (ranges(), lists(), tuples(), nested(30), gens(20))
print(r)

def subscr(n):
    l = list(range(10))
    t = tuple(range(10))
    out = []
    for i in range(-12, 12):
        for c in (l, t):
            try:
                out.append(c[i])
            except IndexError as e:
                out.append(str(e))
    out.append(l[2 ** 40 % 7])
    try:
        l[2 ** 40]
    except IndexError as e:
        out.append(str(e))
    out.append(t[True])
    f = [1.5, float('nan'), -0.0]
    out.append([(a < b, a <= b, a == b, a != b, a > b, a >= b) for a in f for b in f])
    return out

for _ in range(120):
    r = subscr(1)
print(r)

def fcmp(vals):
    out = []
    for a in vals:
        for b in vals:
            r = 0
            if a < b: r |= 1
            if a <= b: r |= 2
            if a == b: r |= 4
            if a != b: r |= 8
            if a > b: r |= 16
            if a >= b: r |= 32
            if not (a < b): r |= 64
            out.append(r)
    return out

for _ in range(120):
    r = fcmp([1.5, float('nan'), -0.0, 0.0, float('inf'), 3, -2, 2 ** 70])
print(r)
