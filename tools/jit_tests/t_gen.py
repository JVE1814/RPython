# Generators in compiled code: send/throw/close, yield from chains, exceptions.
log = []

def inner(n):
    try:
        for i in range(n):
            x = yield i
            if x is not None:
                log.append(('inner got', x))
    except KeyError as e:
        log.append(('inner caught', e.args))
        yield 'recovered'
    finally:
        log.append('inner finally')
    return 'inner-done'

def outer(n):
    r = yield from inner(n)
    log.append(('outer got', r))
    yield from range(2)
    return 'outer-done'

class Tree:
    def __init__(self, l, v, r):
        self.l, self.v, self.r = l, v, r
    def __iter__(self):
        if self.l:
            yield from self.l
        yield self.v
        if self.r:
            yield from self.r

def tree(lo, hi):
    if lo >= hi:
        return None
    mid = (lo + hi) // 2
    return Tree(tree(lo, mid), mid, tree(mid + 1, hi))

def run():
    out = []
    out.append(list(outer(3)))
    g = outer(5)
    out.append(next(g))
    out.append(g.send('a'))
    out.append(g.throw(KeyError('k')))
    out.append(list(g))
    g = outer(5)
    next(g)
    g.close()
    g = inner(3)
    next(g)
    try:
        g.throw(ValueError('v'))
    except ValueError as e:
        out.append(('propagated', str(e)))
    out.append(sum(tree(0, 200)))
    def counter():
        n = 0
        while True:
            n += yield n
    c = counter()
    next(c)
    out.append([c.send(i) for i in range(10)])
    def raiser():
        yield 1
        raise StopIteration('bad')
    try:
        list(raiser())
    except RuntimeError as e:
        out.append(('RuntimeError', str(e)))
    out.append([x * y for x in (i for i in range(5)) for y in (j for j in range(x))])
    return out

for i in range(120):
    r = run()
print(r)
print(log[-12:], len(log))
