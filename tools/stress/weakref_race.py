# Stress: weak references dereferenced and created on some threads while the
# referents die on others (asyncio's WeakSet task registry). A weakref must never
# hand out an object that is already being deallocated, and weakref lists must stay
# consistent when referents and weakrefs die concurrently.
import threading
import time
import weakref

DURATION = 4.0
stop = time.monotonic() + DURATION
shared = [None] * 64
registry = weakref.WeakSet()
errors = []


class Obj:
    def __init__(self, i):
        self.i = i


def churn(k):
    i = 0
    while time.monotonic() < stop:
        o = Obj(i)
        registry.add(o)
        shared[(i * 7 + k) % 64] = o  # drop the previous one: may die here
        i += 1


def reader():
    while time.monotonic() < stop:
        for o in list(shared):
            if o is None:
                continue
            r = weakref.ref(o)
            del o
            x = r()
            if x is not None and not isinstance(x.i, int):
                errors.append("bad object from weakref")
        try:
            len(registry)
            for x in registry:
                x.i
        except RuntimeError:
            pass  # changed size during iteration: allowed


threads = [threading.Thread(target=churn, args=(k,)) for k in range(2)]
threads += [threading.Thread(target=reader) for _ in range(2)]
for t in threads:
    t.start()
for t in threads:
    t.join()
assert not errors, errors[:3]
print("ok")
