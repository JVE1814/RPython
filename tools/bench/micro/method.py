import sys
class C:
    def __init__(self):
        self.v = 0
    def inc(self, d):
        self.v += d
        return self.v
def f(n):
    c = C()
    for i in range(n):
        c.inc(1)
    return c.v
print(f(int(sys.argv[1]) if len(sys.argv) > 1 else 1000000))
