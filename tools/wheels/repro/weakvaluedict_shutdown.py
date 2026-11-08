# Root cause (the end of every full pandas run printed "Exception ignored while calling
# weakref callback ... '_remove_dead_weakref' ... 'NoneType' object is not callable"):
# WeakValueDictionary's removal callback looked up weakref's module global
# _remove_dead_weakref when it ran; at interpreter shutdown, after the weakref module's
# globals were cleared, values dying later found None. It is bound at definition now.
import subprocess, sys

CHILD = r"""
import sys, weakref
class V:
    pass
d = weakref.WeakValueDictionary()
vals = [V() for _ in range(100)]
for i, v in enumerate(vals):
    d[i] = v
# keep d and the values alive until the very end of shutdown: through a module that is
# cleared after weakref (the values are dropped when that module's dict goes)
holder = type(sys)("zz_holder")
holder.d = d
holder.vals = vals
sys.modules["zz_holder"] = holder
del d, vals, holder
# clear weakref's globals as shutdown does, then let the values die
weakref.__dict__.clear()
del sys.modules["zz_holder"].vals
"""
r = subprocess.run([sys.executable, "-I", "-c", CHILD], capture_output=True, text=True)
assert r.returncode == 0 and "Exception ignored" not in r.stderr, r.stderr[-2000:]
print("OK")
