# Stress (perf8): compiled code calling builtin functions and method descriptors through
# the shared call routine (C functions called directly, then the eval-breaker poll)
# while other threads keep requesting collections. Generators suspended in the middle of
# such loops are traversed by the collector, which reads their frames' stack_top.
# Regression: the poll after a direct C call restored stack_top from a register the
# release loop had reused (a pointer), and the parallel collector indexed past the
# frame's slots (abort in gen_traverse).
import gc
import sys
import threading
import time

DURATION = float(sys.argv[1]) if len(sys.argv) > 1 else 6.0
stop = time.monotonic() + DURATION
errors = []


def gen(xs, d, fns=(len, max, min, sum, sorted, id)):
    get = d.get
    for x in xs:
        t = 0
        for f in fns:            # callables unknown to the compiler: the call routine
            t += f(x) and 1
        yield t + abs(-x[0]) + get(x[0], 0) + isinstance(x, list)


def caller(k):
    try:
        xs = [[i, i + 1, k] for i in range(50)]
        d = {i: i for i in range(0, 50, 3)}
        total = 0
        while time.monotonic() < stop:
            g = gen(xs, d)
            for _ in range(20):
                total += next(g)
            total += sum(gen(xs, d))
            s = "ab" * (k + 1)
            total += len(s.upper()) + s.count("a")
        assert total > 0
    except BaseException as e:
        errors.append(repr(e))


def collector():
    try:
        while time.monotonic() < stop:
            gc.collect()
            time.sleep(0.001)
    except BaseException as e:
        errors.append(repr(e))


threads = [threading.Thread(target=caller, args=(k,)) for k in range(4)]
threads += [threading.Thread(target=collector) for _ in range(2)]
for t in threads:
    t.start()
for t in threads:
    t.join()
if errors:
    print("FAIL", errors[:3])
else:
    print("ok ccall_poll_race")
