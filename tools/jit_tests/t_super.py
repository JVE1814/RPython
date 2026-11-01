class A(object):
    def __init__(self, s):
        super(A, self).__init__()
        self.s = s

class B(A):
    def __init__(self, a, b, s):
        super(B, self).__init__(s)
        self.a = a

class C(B):
    pass

for i in range(5):
    c = C(1, 2, 3)
    print(c.a, c.s)

class D:
    def f(self):
        return 'D'
class E(D):
    def f(self):
        return 'E' + super().f()
for i in range(3):
    print(E().f())
