import sys
def f(n):
    l = []
    d = {}
    for i in range(n):
        l.append(i)
        d[i & 1023] = i
    s = 0
    for x in l:
        s += x
    for k in d:
        s += d[k]
    return s + len(l) + l[n // 2]
print(f(int(sys.argv[1]) if len(sys.argv) > 1 else 1000000))
