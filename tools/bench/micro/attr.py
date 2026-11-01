import sys
class P:
    def __init__(self, x, y):
        self.x = x
        self.y = y
def f(n):
    p = P(1, 2)
    s = 0
    for i in range(n):
        s += p.x + p.y
        p.x = i & 15
    return s
print(f(int(sys.argv[1]) if len(sys.argv) > 1 else 1000000))
