import sys
def f(n):
    s = 0
    for i in range(n):
        s += i
    return s
print(f(int(sys.argv[1]) if len(sys.argv) > 1 else 3000000))
