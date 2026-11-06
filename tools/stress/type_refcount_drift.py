# Stress: references to a class taken and dropped through different counting paths.
# Instances count their reference to the class per thread (generic allocation) while
# `__class__` assignment (like an extension's Py_DECREF(Py_TYPE(self))) drops it with a
# plain decref. The class must stay alive as long as anything refers to it; with a
# one-reference bias its own count fell to zero and it was freed under live instances.
import gc
import sys
import threading

ROUNDS = int(sys.argv[1]) if len(sys.argv) > 1 else 40
classes = {}


def make_classes():
    # created on another thread: the main thread is not the classes' owner
    class A:
        def __init__(self):
            self.x = 1
            self.y = 2

    class B:
        def __init__(self):
            self.x = 1
            self.y = 2

    classes["A"] = A
    classes["B"] = B


gc.disable()  # no merges of the per-thread counts in between
for r in range(ROUNDS):
    t = threading.Thread(target=make_classes)
    t.start()
    t.join()
    A, B = classes.pop("A"), classes.pop("B")
    a_objs = [A() for _ in range(2000)]
    b_objs = [A() for _ in range(2000)]
    for o in a_objs:
        o.__class__ = B
    # use the classes heavily after the counts moved around
    for o in a_objs:
        assert type(o) is B and o.x == 1
    for o in b_objs:
        assert type(o) is A and o.y == 2
    a_objs.clear()
    gc.collect()
    b_objs.extend(A() for _ in range(100))
    assert all(type(o) is A for o in b_objs)
    b_objs.clear()
    gc.collect()
    extra = [A() for _ in range(100)] + [B() for _ in range(100)]
    assert sum(o.x for o in extra) == 200
    del extra, A, B
print("ok")
