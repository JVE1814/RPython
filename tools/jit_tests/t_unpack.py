# UnpackSequence into many locals (jit6: fused unpack + stores in tier 2)
log = []

class D:
    def __init__(self, n):
        self.n = n
    def __del__(self):
        log.append(self.n)
    def __repr__(self):
        return 'D%d' % self.n


def swap(n):
    a, b, c, d, e = 1, 2, 3, 4, 5
    for i in range(n):
        a, b, c, d, e = b, c, d, e, a
        a, b, c, d, e = (e, d, c, b, a)
    return a, b, c, d, e


def mixed(seqs):
    out = []
    for s in seqs:
        try:
            p, q, r, t = s
        except ValueError as ex:
            out.append(str(ex))
            continue
        except TypeError as ex:
            out.append(type(ex).__name__)
            continue
        out.append((t, r, q, p))
    return out


def dels(n):
    w = x = y = z = None
    for i in range(n):
        w, x, y, z = D(4 * i), D(4 * i + 1), D(4 * i + 2), D(4 * i + 3)
    return w, x, y, z


def first_bind(n):
    total = 0
    for i in range(n):
        if i % 3 == 0:
            del_me = 1
        a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12 = range(i, i + 12)
        total += a1 + a12 + a6
    return total


def lists(n):
    s = 0
    for i in range(n):
        x = [i, i + 1, i + 2, i + 3, i + 4]
        a, b, c, d, e = x
        s += a * e - b * d + c
        x[2] = 'z'
        a, b, c, d, e = x
        if c != 'z':
            s = -1
    return s


for k in range(60):
    r1 = swap(k)
    r2 = mixed([(1, 2, 3, 4), [5, 6, 7, 8], (1, 2, 3), 'abcd', 5, iter([9, 8, 7, 6])])
    log.clear()
    r3 = dels(5)
    r4 = first_bind(40)
    r5 = lists(50)
print(r1, r2)
print(r3, sorted(log))
print(r4, r5)
