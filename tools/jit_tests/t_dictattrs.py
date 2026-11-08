# (perf8) Instance attributes of objects whose __dict__ was materialised (tier 2 reads
# them at the cached index of the dict): deletion, re-insertion at another index,
# shadowing a class attribute, __dict__ replaced, dict subclass assigned, missing
# attributes falling back to the class, and objects still keeping inline values.
class Rec:
    kind = "class-kind"
    def __init__(self, a, b):
        self.a = a
        self.b = b
        self.c = a + b

def total(objs):
    s = 0
    for o in objs:
        s += o.a + o.b * 2 + o.c
    return s

def kinds(objs):
    return [o.kind for o in objs]

objs = [Rec(i, i + 1) for i in range(40)]
for o in objs[::2]:
    o.__dict__        # materialise the dict
out = []
for rnd in range(60):
    out.append(total(objs))
    out.append(kinds(objs[:3]))
    if rnd == 20:
        o = objs[4]
        del o.a
        o.a = 1000            # re-inserted at the end of its dict
    if rnd == 30:
        objs[6].kind = "instance-kind"
    if rnd == 35:
        objs[8].__dict__ = {"c": 1, "b": 2, "a": 3}
    if rnd == 40:
        class D(dict):
            pass
        objs[10].__dict__ = D(a=7, b=8, c=9)
    if rnd == 45:
        objs[12].__dict__.clear()
        objs[12].__dict__.update(a=-1, b=-2, c=-3, kind="k")
print(out)

def safe(objs):
    r = []
    for o in objs:
        try:
            r.append(o.a)
        except AttributeError as e:
            r.append(str(e))
    return r

for rnd in range(40):
    if rnd == 25:
        del objs[2].a
    res = safe(objs[:5])
print(res)
