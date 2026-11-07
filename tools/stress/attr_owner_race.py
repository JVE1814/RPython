# Stress (scale2): attribute values replaced by the owning thread while other threads
# read them lock-free. The owner frees a replaced value at once only while no other
# thread has read the object's attribute storage (qsbr XREAD bit); a reader that starts
# later must still never see a freed value. Objects are created private (fast path)
# and handed to readers while the owner keeps writing, for instance-dict attributes
# (inline values) and __slots__, through the interpreter and both JIT tiers.
import sys
import threading
import time

DURATION = float(sys.argv[1]) if len(sys.argv) > 1 else 4.0


class Payload:
    __slots__ = ("tag", "data")

    def __init__(self, tag):
        self.tag = tag
        self.data = [tag] * 3

    def check(self):
        d = self.data
        return d[0] == self.tag and d[2] == self.tag


class Inst:
    def __init__(self):
        self.a = Payload(0)
        self.b = Payload(1)


class Slotted:
    __slots__ = ("a", "b")

    def __init__(self):
        self.a = Payload(0)
        self.b = Payload(1)


def write_loop(o, n):
    for i in range(n):
        o.a = Payload(i)
        o.b = Payload(-i)


def read_loop(o, n):
    bad = 0
    for _ in range(n):
        p = o.a
        q = o.b
        if not (p.check() and q.check()):
            bad += 1
    return bad


errors = []
failures = [0]
handoff = []
cv = threading.Condition()
stop = time.monotonic() + DURATION
done = [False]


def owner():
    try:
        while time.monotonic() < stop:
            for cls in (Inst, Slotted):
                o = cls()
                write_loop(o, 200)          # private: immediate frees
                with cv:
                    handoff.append(o)       # now shared: readers may start any time
                    cv.notify_all()
                write_loop(o, 2000)         # racing the readers
    except BaseException as e:
        errors.append(repr(e))
    finally:
        with cv:
            done[0] = True
            cv.notify_all()


def reader():
    try:
        seen = 0
        while True:
            with cv:
                while len(handoff) <= seen and not done[0]:
                    cv.wait(0.1)
                if len(handoff) <= seen:
                    return
                o = handoff[-1]
                seen = len(handoff)
                if len(handoff) > 64:
                    del handoff[:32]
                    seen = len(handoff)
            failures[0] += read_loop(o, 3000)
    except BaseException as e:
        errors.append(repr(e))


# warm both JIT tiers on private objects first
for cls in (Inst, Slotted):
    w = cls()
    for _ in range(50):
        write_loop(w, 200)
        read_loop(w, 200)

threads = [threading.Thread(target=owner) for _ in range(2)] + [threading.Thread(target=reader) for _ in range(3)]
for t in threads:
    t.start()
for t in threads:
    t.join()
if errors or failures[0]:
    print("FAIL", errors[:3], failures[0])
    sys.exit(1)
print("ok")
