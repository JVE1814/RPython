# Stress: cycle collections while other threads keep entering and leaving
# blocking calls (lock waits, sleeps) and create/destroy GC objects.
# Regression for the stop-the-world handshake: a thread returning from a
# blocking call must not run while the collector walks the GC lists
# (symptom: abort in gc::untrack with a misaligned pointer, or list corruption).
import gc
import sys
import threading
import time

DURATION = float(sys.argv[1]) if len(sys.argv) > 1 else 8.0
NTHREADS = 6
stop = time.monotonic() + DURATION
errors = []


class Node:
    def __init__(self, n):
        self.n = n
        self.other = None


def churn(lock, k):
    try:
        while time.monotonic() < stop:
            # cyclic garbage and short-lived containers
            for i in range(200):
                a = Node(i)
                b = Node(i)
                a.other = b
                b.other = a
                l = [a, b, {i: a}]
                del a, b, l
            # bound builtin methods created and dropped (report from test_ssl)
            for i in range(200):
                m = [].append
                m(i)
                del m
            # enter and leave blocking mode many times
            for i in range(20):
                if lock.acquire(timeout=0.0005):
                    lock.release()
                time.sleep(0)
    except BaseException as e:
        errors.append(e)


def collector():
    while time.monotonic() < stop:
        gc.collect()
        time.sleep(0.001)


lock = threading.Lock()
threads = [threading.Thread(target=churn, args=(lock, k)) for k in range(NTHREADS)]
threads.append(threading.Thread(target=collector))
for t in threads:
    t.start()
# the main thread holds the lock most of the time so the others block on it
while time.monotonic() < stop:
    with lock:
        time.sleep(0.002)
    time.sleep(0.0005)
for t in threads:
    t.join()
if errors:
    raise errors[0]
print("ok", gc.collect() >= 0)
