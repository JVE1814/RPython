def m(x):
    match x:
        case 0 | 1:
            return "small"
        case [a, b, *rest]:
            return ("seq", a, b, rest)
        case {"k": v, **others}:
            return ("map", v, others)
        case str() as s if len(s) > 2:
            return ("str", s)
        case int(n):
            return ("int", n)
        case _:
            return "other"
for v in [0, [1, 2, 3], {"k": 1, "z": 2}, "abcd", 7, "x"]:
    print(m(v))
counter = 0
def inc():
    global counter
    counter += 1
inc(); inc(); print(counter)
def mk():
    n = 0
    def bump():
        nonlocal n
        n += 1
        return n
    return bump
b = mk(); b(); print(b())
class K:
    def who(self): return __class__
print(K().who() is K)
lst = [1, 2, 3, 4]
lst[1:3] = [9]; print(lst)
lst[0] += 10; print(lst)
class O: pass
o = O(); o.a = 1; o.a *= 5; print(o.a)
del lst[0]; print(lst)
fs = [lambda i=i: i for i in [1, 2, 3]]
print([f() for f in fs])
