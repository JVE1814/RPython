import sys
def gen(n):
    for i in range(n):
        yield i
def f(n):
    s = 0
    for x in gen(n):
        s += x
    return s
print(f(int(sys.argv[1]) if len(sys.argv) > 1 else 1000000))
