# Stress: two threads lock two objects in opposite orders through Python callbacks.
# deque.index() holds the deque's object lock while it compares, and the comparison
# searches the *other* deque. With locks that are simply held, thread 1 (holding A,
# wanting B) and thread 2 (holding B, wanting A) wait for each other forever; objlock
# gives up the locks a blocked thread holds and takes them back in address order.
import collections
import threading
import time

A = collections.deque(range(50))
B = collections.deque(range(50))
stop = time.monotonic() + 4.0


class Probe:
    def __init__(self, other):
        self.other = other

    def __eq__(self, x):
        self.other.count(-1)  # takes the other deque's lock while ours is held
        return False

    __hash__ = object.__hash__


def worker(mine, other):
    p = Probe(other)
    n = 0
    while time.monotonic() < stop:
        try:
            mine.index(p)
        except ValueError:
            pass
        n += 1
    done.append(n)


done = []
ts = [threading.Thread(target=worker, args=(A, B), daemon=True), threading.Thread(target=worker, args=(B, A), daemon=True)]
for t in ts:
    t.start()
for t in ts:
    t.join(30)
assert all(not t.is_alive() for t in ts), "deadlock: workers still blocked"
assert len(done) == 2
print("ok")
