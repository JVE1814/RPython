# Stress: builtin iterator objects shared by several threads (each next() races the
# others, including the one that exhausts the iterator and drops its sequence).
import threading

N = 3000


def drain(it, out):
    n = 0
    while True:
        try:
            next(it)
            n += 1
        except StopIteration:
            break
        except ValueError:
            pass  # "generator already executing" and the like
    out.append(n)


def make():
    data = tuple(range(N))
    lst = list(range(N))
    return [
        iter(data), iter(lst), reversed(data), reversed(lst), enumerate(data),
        zip(data, lst), map(abs, data), filter(None, lst), iter(range(N)),
        iter(dict.fromkeys(data)), iter(set(data)), iter("x" * N), iter(b"y" * N),
    ]


for rep in range(10):
    for it in make():
        out = []
        ts = [threading.Thread(target=drain, args=(it, out)) for _ in range(6)]
        for t in ts:
            t.start()
        for t in ts:
            t.join()
        assert sum(out) <= N, (type(it), sum(out))
print("ok")
