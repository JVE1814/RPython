# Float loops with unboxed locals around calls and templates (inlined helpers,
# functions passed as values, comprehensions); a spill-slot invariant of tier 2
# (values of locals stale across a template must be addressable) was broken here.
def a_elem(i, j):
    return 1.0 / ((i + j) * (i + j + 1) // 2 + i + 1)

def times(f, u):
    n = len(u)
    out = []
    for i in range(n):
        s = 0.0
        for j, uj in enumerate(u):
            s += f(i, j) * uj
        out.append(s)
    return out

def at_elem(i, j):
    return a_elem(j, i)

def norm(n, rounds):
    u = [1.0] * n
    v = None
    for r in range(rounds):
        v = times(at_elem, times(a_elem, u))
        u = times(at_elem, times(a_elem, v))
    vbv = vv = 0.0
    for ue, ve in zip(u, v):
        vbv += ue * ve
        vv += ve * ve
    return vbv, vv

for k in range(8):
    r = norm(12, 3)
print(r)

def acc(xs):
    t = 0.0
    m = 0.0
    for x in xs:
        t += x
        if x > m:
            m = x
        ys = [y * 2.0 for y in (x, t)]
        t -= ys[0] / 4.0
    return t, m

for k in range(60):
    r = acc([0.5 * i for i in range(30)])
print(r)
