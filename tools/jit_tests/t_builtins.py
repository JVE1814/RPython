# Builtins compiled inline by tier 2 (len), incl. other types, errors, shadowing.
class L:
    def __len__(self):
        return 3

class Bad:
    def __len__(self):
        raise ValueError("no len")

def size(x):
    return len(x)

def total(objs, n):
    t = 0
    for i in range(n):
        for o in objs:
            t += len(o) + size(o)
    return t

objs = [[1, 2], (1,), {1: 2, 3: 4}, "héllo", set([1]), b"abc", L(), range(7), ""]
for k in range(60):
    r = total(objs, 5)
print(r)
try:
    total([Bad()], 1)
except ValueError as e:
    print("ValueError", e)
try:
    total([3], 1)
except TypeError as e:
    print("TypeError", e)

def len(x):
    return 100
print(total(objs, 1))
del len
print(total(objs, 1))

# jit5: dict.get / list.append called directly from tier 2
class Key:
    def __init__(self, v):
        self.v = v
    def __hash__(self):
        return hash(self.v)
    def __eq__(self, o):
        return isinstance(o, Key) and o.v == self.v

class D(dict):
    pass

class GetOverride(dict):
    def get(self, k, d=None):
        return ('override', k)

def methods(n):
    d = {i: i * i for i in range(10)}
    d[Key(3)] = 'k3'
    sub = D(d)
    ov = GetOverride(d)
    out = []
    acc = []
    for i in range(n):
        out.append(d.get(i % 12))
        out.append(d.get(i % 12, -1))
        out.append(sub.get(i % 11, 'x'))
        out.append(d.get(Key(i % 5)))
        acc.append(i)
        if i == n - 3:
            out.append(ov.get(i))
    try:
        d.get([1])
    except TypeError as e:
        out.append(str(e))
    try:
        d.get()
    except TypeError as e:
        out.append(str(e))
    return out[-9:], len(acc)

print(methods(300))
