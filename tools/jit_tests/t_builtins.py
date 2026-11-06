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
