import sys
def f(n):
    t = (1, 2, 3)
    s = 0
    for i in range(n):
        a, b, c = t
        s += a + b + c
        t = (b, c, a)
    return s
print(f(int(sys.argv[1]) if len(sys.argv) > 1 else 1000000))
