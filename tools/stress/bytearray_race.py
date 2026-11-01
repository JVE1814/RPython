# Stress: concurrent mutation and reading of one bytearray (in the spirit of
# test_bytes.test_free_threading_bytearray). Racy results are fine; crashes,
# heap corruption and hangs are not.
import sys
import threading
import time

DURATION = float(sys.argv[1]) if len(sys.argv) > 1 else 5.0
stop = time.monotonic() + DURATION
errors = []
a = bytearray(b"0123456789" * 100)
c = bytearray(b"0" * 0x4000)


def guard(f):
    def run():
        try:
            while time.monotonic() < stop:
                try:
                    f()
                except (BufferError, IndexError, ValueError):
                    pass
        except BaseException as e:
            errors.append(e)
    return run


ops = [
    lambda: a.clear(),
    lambda: a.append(48),
    lambda: a.insert(0, 49),
    lambda: a.extend(c),
    lambda: a.extend([1, 2, 3]),
    lambda: a.pop(),
    lambda: a.remove(48),
    lambda: a.reverse(),
    lambda: a.__reduce__(),
    lambda: a.count(b"0"),
    lambda: a.find(b"12"),
    lambda: a.replace(b"0", b"ab"),
    lambda: a.split(b"5"),
    lambda: bytes(a),
    lambda: a[1:50],
    lambda: a.__setitem__(slice(0, 10), b"xyz"),
    lambda: a.__delitem__(slice(0, 20, 3)),
    lambda: a.__iadd__(b"tail" * 50),
    lambda: a.take_bytes(len(a) // 2),
    lambda: memoryview(a).tobytes(),
    lambda: a.resize(100),
    lambda: a.upper(),
    lambda: list(a[:30]),
    lambda: a == c,
]

threads = [threading.Thread(target=guard(f)) for f in ops]
for t in threads:
    t.start()
for t in threads:
    t.join()
if errors:
    raise errors[0]
print("ok")
