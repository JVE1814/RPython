# (refs1) Borrowed value-stack references: locals pushed without a new reference must
# stay valid until consumed, also when the local is rebound behind the frame's back.
import sys
import gc


class Noisy:
    log = []

    def __init__(self, name):
        self.name = name

    def __del__(self):
        Noisy.log.append(self.name)

    def __repr__(self):
        return "N(%s)" % self.name


def rebind_caller(name, value):
    # writes a fast local of the calling frame through the f_locals proxy
    f = sys._getframe(1)
    f.f_locals[name] = value
    return 0


def victim():
    x = Noisy("x1")
    # x is pushed (borrowed) as an argument, then rebound by the callee of the
    # nested call before the outer call consumes it
    r = pair(x, rebind_caller("x", Noisy("x2")))
    # (tier 2 may keep using a copy of x it holds in a register: only the borrowed
    # argument is checked here, and that x is one of the two live objects)
    return r, x.name in ("x1", "x2")


def pair(a, b):
    gc.collect()
    return (repr(a), b)


def swap():
    a = Noisy("a")
    b = Noisy("b")
    for _ in range(3):
        a, b = b, a
    return a.name + b.name


def method_self():
    lst = []
    for i in range(5):
        lst.append(i)
        lst.append(lst.pop() * 2)
    return lst


def attr_chain(o):
    t = 0
    for i in range(20):
        t += o.v + o.w
        o.v = o.v + 1
    return t


class O:
    def __init__(self):
        self.v = 1
        self.w = 2


def gen(xs):
    for x in xs:
        y = yield x
        if y is not None:
            xs = y


def exc_mid(o):
    try:
        return o.missing + o.v
    except AttributeError as e:
        return type(e).__name__


def clear_in_call():
    s = "abc" * 3
    def k():
        f = sys._getframe(1)
        f.f_locals["s"] = None
        return 1
    return len(s) + k()


def kwcall(a, b=2, *, c=3):
    return a + b + c


def calls():
    x, y, z = 1, 2, 3
    return kwcall(x, b=y, c=z) + kwcall(x) + kwcall(*[x, y], **{"c": z})


def store_self(o):
    o.self_ref = o
    v = o.self_ref
    del o.self_ref
    return v is o


def build(a, b, c):
    t = (a, b, c)
    l = [a, b, c]
    s = {a, b, c}
    d = {a: b, b: c}
    return t, l, sorted(s), d, "%s-%s" % (a, b), f"{a}{b}{c}"


def nested_frames():
    def inner(seq):
        n = 0
        for v in seq:
            n += v
        return n
    data = list(range(10))
    return inner(data) + sum(data) + len(data)


def refcounts():
    o = object()
    base = sys.getrefcount(o)
    lst = [o]
    return sys.getrefcount(o) - base, len(lst)


class Key:
    # a dict key whose __eq__/__hash__ run Python code while the dict and the key
    # are borrowed operands of the subscript (tier-2 templates get them tagged)
    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return hash(self.v)

    def __eq__(self, other):
        return isinstance(other, Key) and other.v == self.v


def subscripts(n):
    d = {Key(i): i for i in range(8)}
    keys = [Key(i & 7) for i in range(n)]
    t = 0
    for k in keys:
        t += d[k]
        if k in d:
            t += 1
        t += len(d) + isinstance(k, Key)
    return t


class Slots:
    __slots__ = ("a", "b")

    def __init__(self):
        self.a = 1
        self.b = [1, 2]


def locals_mix(n):
    s = Slots()
    lst = []
    t = 0
    for i in range(n):
        x = s.a
        y = s.b
        lst.append(x)
        lst.append(y[0])
        t += x + y[1] + len(lst)
        if lst[-1] is x:
            t -= 1
        z = x
        t += z
        x = None
        t += y is not None
        if len(lst) > 50:
            del lst[:]
    return t


def getrefcounts():
    a = []
    b = [a, a]
    def f(x):
        return sys.getrefcount(x)
    return sys.getrefcount(a), f(a), sys.getrefcount(b), len(b)


def main():
    for k in range(60):
        if k == 0 or k == 59:
            print(subscripts(40), locals_mix(70), getrefcounts())
        subscripts(30)
        locals_mix(60)
        r = victim()
        if k == 0:
            print(r)
        s = swap()
        if k == 0:
            print(s)
        if k == 0:
            print(method_self(), attr_chain(O()))
        g = gen([1, 2, 3])
        out = [next(g), g.send([7, 8]), next(g)]
        if k == 0:
            print(out, exc_mid(O()), clear_in_call(), calls())
            print(store_self(O()), build(1, 2, 3), nested_frames(), refcounts())
        method_self()
        attr_chain(O())
        exc_mid(O())
        clear_in_call()
        calls()
        store_self(O())
        build("p", "q", "r")
        nested_frames()
        refcounts()
    gc.collect()
    print(sorted(set(Noisy.log)))


main()
