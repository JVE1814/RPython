# Stress: commonly shared library objects used from several threads at once without
# user locking. Results may be racy; the interpreter must not crash, hang or corrupt
# memory. Exceptions a racy program can legitimately see (RuntimeError for "changed
# during iteration", IndexError/KeyError/ValueError from check-then-act) are ignored.
import array
import collections
import functools
import heapq
import io
import itertools
import json
import random
import sys
import threading
import time

DURATION = float(sys.argv[1]) if len(sys.argv) > 1 else 5.0
stop = time.monotonic() + DURATION

rng = random.Random(1)
counter = itertools.count()
od = collections.OrderedDict()
dd = collections.defaultdict(list)
ctr = collections.Counter()
sio = io.StringIO()
bio = io.BytesIO()
arr = array.array("q")
heap = []
lst = []
dct = {}
st = set()
cyc = itertools.cycle(range(10))
tee_a, tee_b = itertools.tee(itertools.count())


@functools.lru_cache(maxsize=64)
def cached(x):
    return x * 2


OK_EXC = (RuntimeError, IndexError, KeyError, ValueError, StopIteration, BufferError)


def worker(k):
    i = 0
    while time.monotonic() < stop:
        i += 1
        try:
            rng.random()
            rng.randrange(100)
            next(counter)
            next(cyc)
            next(tee_a if k % 2 else tee_b)
            cached(i % 100)
            od[i % 50] = i
            od.move_to_end((i + 3) % 50, last=bool(i & 1))
            if i % 3 == 0:
                od.popitem(last=False)
            dd[i % 20].append(i)
            if i % 50 == 0:
                dd.pop(i % 20, None)
            ctr.update((i % 7, i % 5))
            sio.write("x")
            if i % 100 == 0:
                sio.getvalue()
                sio.seek(0)
                sio.truncate()
            bio.write(b"y")
            if i % 100 == 0:
                bio.getvalue()
                bio.seek(0)
                bio.truncate()
            arr.append(i)
            if len(arr) > 100:
                arr.pop(0)
            heapq.heappush(heap, i)
            if len(heap) > 50:
                heapq.heappop(heap)
            lst.append(i)
            if i % 10 == 0:
                lst.sort()
                sorted(lst)
                tuple(lst)
                "".join(map(str, lst[:5]))
            if len(lst) > 200:
                del lst[:100]
            dct[i % 100] = [i]
            if i % 20 == 0:
                json.dumps(dct)
                list(dct.items())
                dct.copy()
            st.add(i % 100)
            st.discard((i + 50) % 100)
            if i % 20 == 0:
                frozenset(st)
                st.copy()
        except OK_EXC:
            pass


threads = [threading.Thread(target=worker, args=(k,)) for k in range(4)]
for t in threads:
    t.start()
for t in threads:
    t.join()
print("ok")
