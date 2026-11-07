# Consumers that collect or search a fresh generator (jit6: drain mode)
import sys
log = []

class B:
    def __init__(self, v):
        self.v = v
    def __bool__(self):
        log.append(('bool', self.v))
        return bool(self.v)
    def __repr__(self):
        return 'B(%r)' % self.v

class H:
    def __init__(self, v):
        self.v = v
    def __hash__(self):
        log.append(('hash', self.v))
        if self.v == 'bad':
            raise TypeError('bad hash')
        return hash(self.v)
    def __eq__(self, o):
        return isinstance(o, H) and o.v == self.v
    def __repr__(self):
        return 'H(%r)' % self.v

def noisy(n):
    for i in range(n):
        log.append(('gen', i))
        yield i

def raising(n, at):
    for i in range(n):
        if i == at:
            raise ValueError('at %d' % i)
        yield i * 2

def inspecting():
    g = yield 1
    yield sys._getframe().f_back is not None
    yield 3

def run(k):
    out = []
    out.append(list(x * 2 for x in range(k)))
    out.append(sorted(set(x % 3 for x in range(k))))
    out.append(tuple(str(x) for x in range(k)))
    out.append(any(x > k - 2 for x in range(k)))
    out.append(all(x < k for x in range(k)))
    out.append(any(x for x in [0, 0.0, '', None, (), 7]))
    out.append(all(x for x in [1, 'a', (1,), 0, 5]))
    log.clear()
    out.append(any(B(x) for x in [0, 0, 1, 1]))
    out.append(all(B(x) for x in [1, 1, 0, 1]))
    out.append(list(log))
    log.clear()
    out.append([y for y in noisy(4)])
    out.append({y for y in noisy(3)})
    out.append(list(log))
    try:
        list(x for x in raising(6, 3))
    except ValueError as e:
        out.append(('caught', str(e), e.__traceback__.tb_next is not None))
    log.clear()
    try:
        set(H(v) for v in ['a', 'b', 'bad', 'c'])
    except TypeError as e:
        out.append(('caught', str(e)))
    out.append(list(log))
    out.append(sorted(h.v for h in set(H(v) for v in ['a', 'b', 'a'])))
    out.append([x for x in inspecting()])
    out.append(list(x + y for x in range(3) for y in range(2)))
    out.append(sum(1 for _ in range(k)))
    out.append([x for x in (y * y for y in range(5)) if x % 2])
    return out

for k in range(1, 40):
    r = run(5 + k % 3)
for item in r:
    print(item)
