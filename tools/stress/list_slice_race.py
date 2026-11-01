# Stress: one thread replaces the contents of a list (subclass) with slice
# assignment while other threads read it (index, iterate, slice, len, in,
# sort-free reads). Racy reads may see old or new contents but must never
# crash or observe freed memory.
import sys
import threading
import time

DURATION = float(sys.argv[1]) if len(sys.argv) > 1 else 5.0
stop = time.monotonic() + DURATION
errors = []


class L(list):
    def refill(self, n):
        self[:] = [object() for _ in range(n)]


lst = L()
lst.refill(10)
plain = [object() for _ in range(10)]


def writer():
    n = 0
    while time.monotonic() < stop:
        n = (n + 7) % 64
        lst.refill(n)
        plain[:] = [str(i) * 3 for i in range(n)]
        plain.append(1)
        plain.extend(range(n))
        del plain[n // 2:]


def reader():
    try:
        while time.monotonic() < stop:
            for seq in (lst, plain):
                for x in seq:
                    pass
                k = len(seq)
                for i in range(k):
                    try:
                        seq[i]
                        seq[-1]
                    except IndexError:
                        pass
                seq[1:5]
                seq[::2]
                list(seq)
                tuple(seq)
                None in seq
                seq.count(None)
                repr(len(seq))
    except BaseException as e:
        errors.append(e)


ths = [threading.Thread(target=writer)] + [threading.Thread(target=reader) for _ in range(3)]
for t in ths:
    t.start()
for t in ths:
    t.join()
if errors:
    raise errors[0]
print("ok")
