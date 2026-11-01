# Differential test: output must be identical for RPYTHON_JIT=0 and the JIT tiers.

def arith(n):
    out = []
    for i in range(-n, n):
        a = i * 7 - 3
        b = i // 3 if i else 1
        out.append((a + b, a - b, a * b, a & b, a | b, a ^ b, a // (b or 1), a % (b or 1), a << 3, a >> 2))
    return out

print(arith(20)[:5], sum(sum(t) for t in arith(50)))

def bigarith():
    x = 1
    for i in range(100):
        x = x * 3 + i
    y = -x
    return x, y, x - y, x * y % 1000003, x // 7, x > y, x == -y

print(bigarith())

def floats(n):
    x = 0.5
    s = 0.0
    for i in range(n):
        x = x * 1.0001 + 0.25 - i / 1000.0
        s += x * 2 - 1
        if x > s:
            s = s + 1.5
    return x, s, 3 * x, x * 3, 1 + x, x - 2

print(floats(1000))

def compares(n):
    c = 0
    for i in range(n):
        for j in range(-3, 4):
            if i < j: c += 1
            if i <= j: c += 2
            if i == j: c += 4
            if i != j: c += 8
            if i > j: c += 16
            if i >= j: c += 32
            if i * 1.5 < j: c += 64
            if "a" == "b" or "x" != "x": c += 1000
    return c

print(compares(50))

def strs():
    s = ""
    for i in range(30):
        s += str(i)
        s = s + "-"
    return s, len(s), s == "x", s != "y", f"{s[:5]}!{len(s):>5}|{3.25!r}|{'q'!s}"

print(strs())

def bools(n):
    r = []
    for i in range(n):
        a = i % 2 == 0
        b = not a
        c = a and b
        d = a or b
        e = i and i - 1
        f = i or 'zero'
        r.append((a, b, c, d, e, f, not i, bool(i), i is None, i is not None))
    return r

print(bools(6))

def unbound(flag):
    if flag:
        x = 1
    return x

for k in (1, 0):
    try:
        print(unbound(k))
    except UnboundLocalError as e:
        print("UnboundLocalError", e)

def loops_with_else(n):
    out = []
    for i in range(n):
        for j in range(i):
            if j * j > i:
                out.append((i, j))
                break
        else:
            out.append(-i)
    k = 0
    while k < n:
        k += 3
        if k == 7:
            continue
    else:
        out.append('w')
    return out

print(loops_with_else(12))
