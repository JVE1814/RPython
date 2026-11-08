# Root cause (pandas: ChainedAssignmentError not raised for df["a"].fillna(..., inplace=True),
# update, where/mask, replace, interpolate, clip; 15 tests): pandas' NDFrame defines
# __getattr__, so `temp.method(...)` loads a bound method instead of (function, self).
# CPython's CALL unpacks a bound method into (function, self) and drops it before the
# call, so the temporary is referenced once (by the callee's frame) and pandas compares
# sys.getrefcount(self) <= 1. Our interpreter called the bound method, which kept a
# second reference. The interpreter's Call now unpacks it as the JIT tiers already did.
# Also (STORE_SUBSCR): a named object is counted 3 times inside its class's __setitem__.
import sys

seen = {}


class C:
    def __getattr__(self, name):  # makes method loads produce bound methods
        raise AttributeError(name)

    def meth(self, tag, inplace=False):
        seen[tag] = sys.getrefcount(self)

    def __getitem__(self, k):
        return C()

    def __setitem__(self, k, v):
        seen[k] = sys.getrefcount(self)


def f():
    c = C()
    c[1].meth("temp")
    c[1].meth("temp_kw", inplace=True)
    c.meth("named")
    m = c[1].meth  # the bound method stays alive in a local: 2
    m("kept")
    c["setitem_named"] = 1
    c[1]["setitem_temp"] = 1


for _ in range(300):  # also through the JIT tiers
    f()
    expected = {"temp": 1, "temp_kw": 1, "named": 2, "kept": 2, "setitem_named": 3, "setitem_temp": 2}
    assert seen == expected, seen
print("OK")
