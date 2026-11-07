# Stress: after Thread.join() the references the thread dropped must be released
# (test_threading.test_no_refcycle_through_target). They were retired through QSBR
# while two threads existed and released only at a later poll (about 1 in 300 joins
# under load); join now merges and releases them before returning.
import threading, weakref, sys
N = int(sys.argv[1]) if len(sys.argv) > 1 else 2000
class R:
    def __init__(self):
        self.thread = threading.Thread(target=self._run, args=(self,), kwargs={'y': self})
        self.thread.start()
    def _run(self, other, y):
        pass
bad = 0
for i in range(N):
    o = R(); w = weakref.ref(o); o.thread.join(); del o
    if w() is not None:
        bad += 1
assert bad == 0, f"{bad} of {N} joins left references behind"
print("ok")
