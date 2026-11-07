# Class attributes read through instances (scale2: tier-2 CLASS_ATTR_SHADOWABLE, also
# inside inlined methods): later shadowing by an instance attribute, by a materialised
# instance dict, and class attribute rebinding must be seen.

class Base:
    LIMIT = 10
    FACTOR = 2.0

    def value(self, x):
        return x * self.FACTOR


class Obj(Base):
    BONUS = 1.0

    def __init__(self):
        self.extra = 0.5

    def compute(self, x):
        if x > self.LIMIT:
            return self.value(x) + self.BONUS + self.extra
        return self.value(x) - self.extra


def run(objs, n):
    t = 0.0
    for i in range(n):
        for o in objs:
            t += o.compute(i & 31)
    return t


a, b = Obj(), Obj()
for k in range(40):
    r = run([a, b], 200)
print(r)
b.LIMIT = 0            # shadow in the instance (inline values)
print(run([a, b], 200))
b.__dict__["FACTOR"] = 3.0   # materialised dict shadows too
print(run([a, b], 200))
Base.FACTOR = 5.0      # class attribute rebinding
print(run([a, b], 200))
del b.LIMIT
print(run([a, b], 200))
c = Obj()
c.BONUS = 100.0
print(run([a, c], 200))
