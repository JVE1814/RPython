# Folded globals (tier 2): rebinding, shadowing builtins, deleting, writes through
# globals() with non-interned keys, unrelated writes in module-level loops.

K = 3

def scale(n):
    t = 0
    for i in range(n):
        t += i * K
    return t

def use_len(xs):
    s = 0
    for x in xs:
        s += len(x)
    return s

for r in range(60):
    v = scale(50)
    if r == 30:
        K = 5                      # rebind: dependent code must see the new value
    if r == 40:
        globals()["".join(["K"])] = 7  # non-interned key
    if r % 10 == 0:
        print(r, v)

data = [[1, 2], [3], []]
for r in range(60):
    v = use_len(data)
    if r == 20:
        def len(x):                # shadow the builtin by inserting into globals
            return 100
    if r == 40:
        del len                    # deleting reveals the builtin again
    if r % 10 == 0 or r in (21, 41):
        print("len", r, v)

# unrelated global writes in a module-level loop must not change results
counter = 0
def f(n):
    return n * K + 1
acc = 0
for i in range(3000):
    counter = i
    acc += f(i)
print(acc, counter)

# a function that rebinds the global it reads
G = 0
def bump(n):
    global G
    for i in range(n):
        G = G + 1
    return G
for i in range(30):
    bump(20)
print(G)

# globals dict cleared / updated wholesale
def readk():
    return K
for i in range(200):
    readk()
globals().update({"K": 11})
print(readk())
