import sys
def f(n):
    i = 0
    s = 0
    while i < n:
        s += i & 7
        i += 1
    return s
print(f(int(sys.argv[1]) if len(sys.argv) > 1 else 3000000))
