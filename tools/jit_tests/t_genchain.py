# yield-from / await chains (jit6: resumption of the innermost generator directly)
import sys


def leaf(n, fail_at=-1, ret='done'):
    for i in range(n):
        if i == fail_at:
            raise KeyError(i)
        x = yield i
        if x is not None:
            yield ('got', x)
    return ret


def mid(g, tag):
    r = yield from g
    return (tag, r)


def chain(depth, n, **kw):
    g = leaf(n, **kw)
    for d in range(depth):
        g = mid(g, d)
    return g


def drive(g):
    out = []
    while True:
        try:
            out.append(next(g))
        except StopIteration as e:
            out.append(('stop', e.value))
            return out


for depth in (1, 2, 5, 40):
    for rep in range(30):
        res = drive(chain(depth, 6))
    print(depth, res)
    s = 0
    for rep in range(30):
        for v in chain(depth, 20):
            s += v
    print('sum', s)

# exceptions from the leaf come out of every level with a full traceback
def tb_names(e):
    names = []
    tb = e.__traceback__
    while tb:
        names.append(tb.tb_frame.f_code.co_name)
        tb = tb.tb_next
    return names

for rep in range(40):
    g = chain(4, 10, fail_at=5)
    got = []
    try:
        for v in g:
            got.append(v)
    except KeyError as e:
        r = (got, repr(e), tb_names(e))
print(r)

# send() through the chain
for rep in range(40):
    g = chain(3, 5)
    a = [next(g), next(g), g.send('hello'), next(g), next(g)]
print(a)


# f_back of the leaf frame sees every level; the levels are 'running'
def inspect_leaf(holder):
    for i in range(3):
        f = sys._getframe()
        names = []
        while f is not None and len(names) < 8:
            names.append(f.f_code.co_name)
            f = f.f_back
        running = [x.gi_running for x in holder]
        yield (names, running)


for rep in range(40):
    holder = []
    g0 = inspect_leaf(holder)
    g1 = mid(g0, 1)
    g2 = mid(g1, 2)
    holder.extend([g1, g2])
    res = list(g2)
print(res)


# a level that is resumed directly by the leaf: already executing
def evil(holder):
    yield 1
    try:
        next(holder[0])
    except ValueError as e:
        yield str(e)
    yield 3

for rep in range(40):
    holder = []
    g1 = mid(evil(holder), 'x')
    g2 = mid(g1, 'y')
    holder.append(g1)
    res = list(g2)
print(res)


# sys.exc_info() inside the leaf with handled exceptions in outer levels
def exc_leaf():
    for i in range(3):
        yield repr(sys.exc_info()[1])

def exc_mid(g):
    try:
        raise ValueError('outer')
    except ValueError:
        yield from g

for rep in range(40):
    res = list(mid(exc_mid(mid(exc_leaf(), 0)), 1))
print(res)


# throw() and close() into a chain
for rep in range(40):
    g = chain(3, 10)
    next(g); next(g)
    try:
        g.throw(IndexError('boom'))
    except IndexError as e:
        t = repr(e)
    g = chain(3, 10)
    next(g)
    g.close()
print(t, g.gi_frame)


# coroutine await chains driven by send()
class Fut:
    def __await__(self):
        v = yield 'suspend'
        return v

async def cleaf(n):
    t = 0
    for i in range(n):
        t += await Fut()
    return t

async def cmid(c):
    return await c

for rep in range(40):
    c = cmid(cmid(cmid(cleaf(5))))
    r = [c.send(None)]
    try:
        for i in range(10):
            r.append(c.send(i))
    except StopIteration as e:
        r.append(('stop', e.value))
print(r)


# generator returning a value through deep chains; the next level continues
def tree(n):
    if n == 0:
        return 0
    a = yield from tree(n - 1)
    yield n
    b = yield from tree(n - 1)
    return a + b + 1

for rep in range(30):
    g = tree(6)
    vals = []
    try:
        while True:
            vals.append(next(g))
    except StopIteration as e:
        vals.append(e.value)
print(len(vals), sum(vals[:-1]), vals[-1])
