# (perf6) LoadMethod sites that first run after the function was compiled (code after
# the loop whose back edges triggered compilation): the run-time read of the cache entry.

class Box:
    __slots__ = ('items',)

    def __init__(self):
        self.items = []

    def add(self, x):
        self.items.append(x)
        return len(self.items)


class Other:
    __slots__ = ()

    def add(self, x):
        return -x


def late(n, objs):
    t = 0
    for i in range(n):          # compiled here (back edges)
        t += i
    out = []
    for i in range(n):          # first executions of these sites are after compilation
        out.append(i * 2)
    s = 0
    for o in objs:              # receivers of changing types at one site
        s += o.add(3)
    words = []
    for w in ('a', 'bb', 'ccc'):
        words.append(w.upper())
    return t, len(out), out[-1] if out else None, s, words


def patched(n):
    b = Box()
    r = []
    for i in range(n):
        pass
    for i in range(6):
        if i == 3:
            Box.add = lambda self, x: 1000 + x   # type changes: the tag guard fails
        r.append(b.add(i))
    del Box.add
    return r


Box.add_orig = Box.add
for k in range(3):
    print(late(300 + k, [Box(), Other(), Box(), Other(), Box()]))
    print(late(0, []))
for k in range(2):
    Box.add = Box.add_orig
    print(patched(300))
Box.add = Box.add_orig


def meth_on_dict_type(n):
    class D:
        def get(self):
            return 'class'
    d = D()
    for i in range(n):
        pass
    res = [d.get()]
    d.get = lambda: 'instance'                 # shadowed by the instance
    res.append(d.get())
    return res


for k in range(3):
    print(meth_on_dict_type(300))
