# Stress: one collections.deque shared by producer and consumer threads (asyncio's
# ready queue, call_soon_threadsafe). Every appended item must come out exactly
# once; the deque's storage used to be mutated without any lock (heap corruption).
import collections
import threading

N = 100000
PRODUCERS = 3
d = collections.deque()
bounded = collections.deque(maxlen=50)
got = []
done = threading.Event()


def producer(k):
    for i in range(N):
        d.append((k, i))
        bounded.append(i)
        if i % 5000 == 0:
            bounded.rotate(3)
            len(bounded)


def consumer():
    n = 0
    while True:
        try:
            d.popleft()
            n += 1
        except IndexError:
            if done.is_set() and not d:
                break
        if n % 7000 == 0:
            bounded.copy()
            bounded.extend(range(10))
    got.append(n)


cons = [threading.Thread(target=consumer) for _ in range(2)]
prods = [threading.Thread(target=producer, args=(k,)) for k in range(PRODUCERS)]
for t in cons + prods:
    t.start()
for t in prods:
    t.join()
done.set()
for t in cons:
    t.join()
assert sum(got) == PRODUCERS * N, (sum(got), PRODUCERS * N)
assert len(bounded) <= 50
print("ok")
