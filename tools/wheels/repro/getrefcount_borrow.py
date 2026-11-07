# Root cause: pandas detects chained assignment (df["a"][0] = 1, df["a"].fillna(...,
# inplace=True)) by comparing sys.getrefcount(self) with CPython 3.14+'s counts,
# where a local argument is loaded borrowed (LOAD_FAST_BORROW) and a Python call
# moves its arguments from the caller's stack into the callee.  Our interpreter
# copies them, so the counts were one or two higher and the warnings never came
# (~30 pandas tests; also spec test_list.test_list_overwrite_local).
import sys

seen = {}


class C:
    def __setitem__(self, k, v):
        seen["setitem", k] = sys.getrefcount(self)

    def __getitem__(self, k):
        return C()

    def meth(self, tag):
        seen["meth", tag] = sys.getrefcount(self)


def f():
    c = C()
    c[0] = 1            # named: 3 (caller local, STORE_SUBSCR operand, self)
    c[1]["t"] = 1       # temporary: 2
    c[1].meth("t")      # temporary: 1
    c.meth("n")         # named: 2
    x = object()
    seen["local"] = sys.getrefcount(x)          # 1
    seen["temp"] = sys.getrefcount(object())    # 1


def g(x):
    seen["arg"] = sys.getrefcount(x)


f()
g(object())
expected = {("setitem", 0): 3, ("setitem", "t"): 2, ("meth", "t"): 1, ("meth", "n"): 2,
            "local": 1, "temp": 1, "arg": 1}
assert seen == expected, seen
print("OK")
