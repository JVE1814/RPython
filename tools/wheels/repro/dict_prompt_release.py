# Root cause (multidict's finalizer-reentrancy tests under pytest-run-parallel): once a
# second thread existed, every key/value removed from any dict was released through
# QSBR (a grace period for lock-free readers), so its finalizer and weakref callbacks
# ran some time later instead of at the removal as in CPython. The grace period is now
# only taken when a lock-free reader may hold the pointer (types/dict.rs
# removal_needs_grace: a non-owner thread marked the table shared_read, or the writer is
# not the owner). A dict used by one thread releases at once.
# (Lists still defer their removed items while other threads run: known difference.)
import threading, weakref

stop = threading.Event()
t = threading.Thread(target=stop.wait, daemon=True)
t.start()  # now the process is multi-threaded


class V:
    pass


def check():
    d = {}
    for op in ("del", "pop", "popitem", "replace", "clear"):
        v = V()
        r = weakref.ref(v)
        d["k"] = v
        del v
        if op == "del":
            del d["k"]
        elif op == "pop":
            d.pop("k")
        elif op == "popitem":
            d.popitem()
        elif op == "replace":
            d["k"] = 0
        else:
            d.clear()
        assert r() is None, op


check()
done = []


def in_thread():
    check()
    done.append(True)


w = threading.Thread(target=in_thread)
w.start()  # also in a thread that is not the main one
w.join()
assert done == [True], "check failed in a thread"
stop.set()
t.join()
print("OK")
