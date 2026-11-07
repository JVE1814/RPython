# Root cause (propcache test_cached_property_concurrent_eviction, also with
# functools.cached_property): the LoadAttr cache's instance-dict fast path read the hinted
# dict entry (key, then value) without the dict's sequence lock. A thread popping that
# key could clear the entry between the two reads: the reader got a NULL value and
# crashed taking a reference to it. The fast path now validates the read like the other
# lock-free dict readers (types/dict.rs value_at_if_key / lookup_index_value).
import threading
from functools import cached_property

N = 20000


class A:
    pass


class B:
    @cached_property
    def prop(self):
        return [0] * 8


def run(obj, read):
    cache = obj.__dict__
    barrier = threading.Barrier(8)

    def evict():
        barrier.wait()
        for _ in range(N):
            cache["prop"] = [1] * 8
            cache.pop("prop", None)

    def reader():
        barrier.wait()
        for _ in range(N):
            read(obj)

    ts = [threading.Thread(target=f) for f in [evict] * 4 + [reader] * 4]
    for t in ts:
        t.start()
    for t in ts:
        t.join()


def read_plain(a):
    try:
        x = a.prop
    except AttributeError:
        return
    assert len(x) == 8


def read_cached(b):
    assert len(b.prop) == 8


run(A(), read_plain)
run(B(), read_cached)
print("OK")
