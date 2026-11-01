import sys
class Node:
    def __init__(self):
        self.other = None

def f(n):
    err = None
    for i in range(n):
        a = Node()
        b = Node()
        a.other = b
        b.other = a
        try:
            raise ValueError(i)
        except ValueError as e:
            err = e
    return err

print(f(int(sys.argv[1]) if len(sys.argv) > 1 else 300000))
