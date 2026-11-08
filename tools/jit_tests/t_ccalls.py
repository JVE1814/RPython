# (perf8) Builtin functions and method descriptors called from compiled code through
# the shared call routine: METH_O, METH_NOARGS, METH_FASTCALL (with and without
# keywords), borrowed (local) and owned arguments, self of a subclass (generic path),
# argument-count errors, exceptions raised by the C function, reference counts.
import sys

class L(list):
    pass

def work(xs, d, s):
    out = []
    for i, x in enumerate(xs):
        out.append(len(x))                 # METH_O, borrowed argument
        out.append(abs(-i))                # METH_O, owned argument
        out.append(d.get(i, -1))           # method descriptor, FASTCALL
        out.append(d.get(i))
        out.append(isinstance(x, list))    # FASTCALL builtin
        out.append(s.upper().lower())      # METH_NOARGS
        out.append(min(i, 3, 7))           # FASTCALL|KEYWORDS builtin, 3 args
        out.append(max(x, default=0) if x else 0)
        out.append(divmod(i, 3))
        out.append(getattr(x, "nope", None))
        out.append(list.__len__(x))        # descriptor with self of list or subclass
        out.append(sorted(x, reverse=True)[:2])
    return out

def errors(n):
    r = []
    for i in range(n):
        try:
            len(i)
        except TypeError as e:
            r.append(type(e).__name__)
        try:
            abs()
        except TypeError as e:
            r.append("abs0")
        try:
            [].append(1, 2)
        except TypeError as e:
            r.append("append2")
        try:
            int("x%d" % i)
        except ValueError as e:
            r.append("int")
        try:
            {}.pop(i)
        except KeyError as e:
            r.append(repr(e))
    return r

def refs(n):
    o = object()
    counts = []
    for i in range(n):
        counts.append(sys.getrefcount(o))
        x = o
        counts.append(sys.getrefcount(x))
        del x
    return counts

xs = [[1, 2, 3], L([4, 5]), [], L()]
d = {0: "a", 2: "b"}
for k in range(30):
    r = work(xs * 3, d, "Ab")
print(r)
print(errors(25)[-10:])
c = refs(40)
print(c[-4:], len(set(c)))
