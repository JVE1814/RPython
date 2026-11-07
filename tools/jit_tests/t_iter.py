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

def mixed(vals):
    out = []
    for a in vals:
        for b in vals:
            try:
                out.append((a + b, a - b, a * b, a / b))
            except ZeroDivisionError as e:
                out.append(str(e))
            except TypeError as e:
                out.append('TE')
    return out

for _ in range(120):
    r = mixed([0, 1, -7, 3.5, 0.0, -0.0, float('nan'), float('inf'), 2 ** 31, 2 ** 70, True, 1 << 29])
print(r)

# unpacking sites that see tuples and lists (and then other sequences, wrong sizes)
def unpack_mixed(seqs):
    t = 0
    for s in seqs:
        a, b, c = s
        t += a * 100 + b * 10 + c
    return t

for _ in range(120):
    r = unpack_mixed([(1, 2, 3), [4, 5, 6], (7, 8, 9)])
print(r, unpack_mixed([(1, 2, 3), [4, 5, 6], range(3), "123" and (0, 0, 1)]))
for bad in ([1, 2], (1, 2, 3, 4), "ab"):
    try:
        unpack_mixed([bad])
    except (ValueError, TypeError) as e:
        print(type(e).__name__, e)

# jit5: iter(range(...)) without the range object
class Idx:
    def __init__(self, v):
        self.v = v
    def __index__(self):
        return self.v

def ranges(n):
    out = []
    for k in range(n):
        s = 0
        for i in range(k):
            s += i
        for i in range(k, 2 * k):
            s += i
        for i in range(3 * k, -k, -3):
            s -= i
        for i in range(True, Idx(k)):
            s += i
        for i in range(2**62, 2**62 + 2):
            s += i & 7
        for i in range(-2**63, -2**63 + 1, 2**63):
            s += 1
        out.append(s)
    try:
        for i in range(0, 5, 0):
            pass
    except ValueError as e:
        out.append(str(e))
    try:
        for i in range(1.5):
            pass
    except TypeError as e:
        out.append(str(e))
    for i in range(2**70, 2**70 + 3):
        out.append(i)
    return out

print(ranges(60)[-8:])
