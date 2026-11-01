def exc_in_loop(n):
    out = []
    d = {0: 'a', 2: 'b'}
    for i in range(n):
        try:
            out.append(d[i])
        except KeyError as e:
            out.append(('KE', e.args))
        finally:
            out.append('f')
        try:
            1 / (i - 3)
        except ZeroDivisionError:
            out.append('zde')
            continue
        if i == 5:
            break
    return out

print(exc_in_loop(10))

def raise_deep(n):
    if n == 0:
        raise ValueError("bottom")
    return raise_deep(n - 1) + 1

for k in range(3):
    try:
        raise_deep(5 + k)
    except ValueError as e:
        tb = e.__traceback__
        depth = 0
        while tb is not None:
            depth += 1
            tb = tb.tb_next
        print("caught", e, depth)

def gen(n):
    for i in range(n):
        if i % 3 == 0:
            yield i
        else:
            yield -i
    return 'done'

def use_gen(n):
    out = list(gen(n))
    g = gen(2)
    out.append(next(g))
    out.append(next(g))
    try:
        next(g)
    except StopIteration as e:
        out.append(e.value)
    return out

print(use_gen(10))

def gen_send():
    total = 0
    while True:
        x = yield total
        if x is None:
            return total
        total += x

g = gen_send()
next(g)
for i in range(5):
    print(g.send(i))

def yf(n):
    yield from gen(n)
    yield from range(3)

print(list(yf(5)))

class CM:
    def __init__(self):
        self.log = []
    def __enter__(self):
        self.log.append('in')
        return self
    def __exit__(self, t, v, tb):
        self.log.append(('out', t.__name__ if t else None))
        return t is KeyError

def withs(n):
    cm = CM()
    for i in range(n):
        with cm as c:
            if i == 1:
                raise KeyError(i)
            c.log.append(i)
    return cm.log

print(withs(4))

def unpack(n):
    out = []
    for i in range(n):
        a, b = i, i + 1
        (c, d), e = (a, b), [i]
        f, *g, h = range(i + 2)
        out.append((a, b, c, d, e, f, g, h))
    try:
        x, y = [1, 2, 3]
    except ValueError as err:
        out.append(str(err))
    return out

print(unpack(4))

def comps(n):
    return ([i * i for i in range(n) if i % 2], {i: i * 2 for i in range(n)}, {i % 3 for i in range(n)},
            sum(x for x in range(n)), [(i, j) for i in range(3) for j in range(i)])

print(comps(10))

def containers(n):
    l = []
    d = {}
    s = set()
    for i in range(n):
        l.append(i)
        l[i // 2] = i
        d[i] = l[-1]
        s.add(i % 4)
        if i in d and i not in s:
            d[i] += 1
    del l[0]
    del d[1]
    return l, sorted(d.items()), sorted(s), l[1:3], l[::2], (1, 2, 3)[1], 3 in l

print(containers(10))

def deep_rec(n):
    return 0 if n == 0 else 1 + deep_rec(n - 1)

try:
    print(deep_rec(500))
    deep_rec(100000)
except RecursionError:
    print("RecursionError")

def matchy(v):
    match v:
        case [x, y]:
            return ('pair', x, y)
        case {'k': k}:
            return ('dict', k)
        case int(n) if n > 3:
            return ('big', n)
        case _:
            return 'other'

print([matchy(v) for v in ([1, 2], {'k': 5}, 7, 2, 'x')])

def assert_and_del(n):
    out = []
    for i in range(n):
        x = i
        del x
        try:
            print(x)
        except UnboundLocalError:
            out.append('ule')
        try:
            assert i < 2, "too big"
        except AssertionError as e:
            out.append(str(e))
    return out

print(assert_and_del(4))
