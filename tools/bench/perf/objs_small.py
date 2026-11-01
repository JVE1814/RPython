import sys
class P:
    def __init__(self, x, y, z):
        self.x = x
        self.y = y
        self.z = z
ps = [P(i, i, i) for i in range(int(sys.argv[1]) if len(sys.argv) > 1 else 50000)]
