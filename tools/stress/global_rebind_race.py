# Stress (scale2): compiled code reads module globals and builtins without the dict
# lock while other threads rebind them to fresh objects, add and delete globals (dict
# resizes) and shadow a builtin. A reader must only ever see a live, consistent value.
import sys
import threading
import time

DURATION = float(sys.argv[1]) if len(sys.argv) > 1 else 4.0


class Box:
    __slots__ = ("v", "w")

    def __init__(self, v):
        self.v = v
        self.w = -v


CUR = Box(0)
stop = time.monotonic() + DURATION
errors = []
bad = [0]


def reader():
    try:
        n = 0
        while time.monotonic() < stop:
            for _ in range(2000):
                b = CUR
                if b.v != -b.w:
                    bad[0] += 1
                n += len("ab")          # builtin (shadowed at times)
        return n
    except BaseException as e:
        errors.append(repr(e))


def writer(k):
    try:
        i = 0
        g = globals()
        while time.monotonic() < stop:
            i += 1
            global CUR
            CUR = Box(i)
            name = "tmp_%d_%d" % (k, i % 300)
            g[name] = Box(i)
            if i % 3 == 0:
                g.pop("tmp_%d_%d" % (k, (i * 7) % 300), None)
            if i % 500 == 0:
                g["len"] = lambda s: 2      # shadows the builtin, same result
            elif i % 500 == 250:
                g.pop("len", None)
    except BaseException as e:
        errors.append(repr(e))


# warm the JIT tiers single-threaded
for _ in range(30):
    b = CUR
    for _ in range(2000):
        b = CUR
        len("ab")

threads = [threading.Thread(target=reader) for _ in range(3)] + [threading.Thread(target=writer, args=(k,)) for k in range(2)]
for t in threads:
    t.start()
for t in threads:
    t.join()
if errors or bad[0]:
    print("FAIL", errors[:3], bad[0])
    sys.exit(1)
print("ok")
