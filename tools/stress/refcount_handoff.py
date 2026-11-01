# Stress: objects created by one thread and released by others (biased
# reference counting: the non-owner's decref of a reference counted in the
# owner's local count goes through the owner's merge queue). Objects must not
# be freed while they sit in a merge queue.
import queue
import sys
import threading
import time

DURATION = float(sys.argv[1]) if len(sys.argv) > 1 else 5.0
stop = time.monotonic() + DURATION
q = queue.Queue(maxsize=1000)
shared = []
errors = []


class Payload:
    __slots__ = ("a", "b")

    def __init__(self, a):
        self.a = a
        self.b = [a]


def producer():
    i = 0
    while time.monotonic() < stop:
        p = Payload(i)
        # extra references owned by this thread
        shared.append(p)
        q.put(p)
        if len(shared) > 200:
            del shared[:100]
        i += 1
    for _ in range(4):
        q.put(None)


def consumer():
    try:
        while True:
            p = q.get()
            if p is None:
                return
            # read it, hand it around, drop it
            x = p.b[0] + p.a
            t = (p, p, p)
            del t, p
    except BaseException as e:
        errors.append(e)


ths = [threading.Thread(target=producer)] + [threading.Thread(target=consumer) for _ in range(4)]
for t in ths:
    t.start()
for t in ths:
    t.join()
if errors:
    raise errors[0]
print("ok")
