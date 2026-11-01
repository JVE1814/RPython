# Stress: finalizers and weakref callbacks of cyclic garbage that take a lock
# another (running) thread holds, or wait for another thread. If the collector
# ran them while the world is stopped, the lock holder could never release it
# (deadlock / hang).
import gc
import sys
import threading
import time
import weakref

DURATION = float(sys.argv[1]) if len(sys.argv) > 1 else 5.0
lock = threading.Lock()
stop = time.monotonic() + DURATION
finalized = 0
callbacks = 0


class WithDel:
    def __init__(self):
        self.me = self

    def __del__(self):
        global finalized
        with lock:
            finalized += 1


class Plain:
    def __init__(self):
        self.me = self


def holder():
    # holds the lock while running Python code (polling safepoints)
    while time.monotonic() < stop:
        with lock:
            x = 0
            for i in range(2000):
                x += i
        time.sleep(0)


def on_dead(ref):
    global callbacks
    with lock:
        callbacks += 1


def maker():
    keep = []
    while time.monotonic() < stop:
        for i in range(50):
            WithDel()
            p = Plain()
            keep.append(weakref.ref(p, on_dead))
            del p
        if len(keep) > 1000:
            del keep[:500]
        gc.collect()


threads = [threading.Thread(target=holder) for _ in range(2)]
threads += [threading.Thread(target=maker) for _ in range(2)]
for t in threads:
    t.start()
for t in threads:
    t.join()
gc.collect()
print("ok", finalized > 0, callbacks > 0)
