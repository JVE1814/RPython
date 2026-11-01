import sys
K = 3
def g(x):
    return x + K
def f(n):
    s = 0
    for i in range(n):
        s += g(i) + len("ab")
    return s
print(f(int(sys.argv[1]) if len(sys.argv) > 1 else 1000000))
