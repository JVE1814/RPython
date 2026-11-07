# Stress: one BytesIO shared by writers and readers (test_free_threading.test_io). Its
# methods and iterator must run under the object lock (a merge once dropped it: heap
# corruption), and buffer exports must not race with resizing writes.
import io
import threading

for rep in range(10):
    shared = io.BytesIO(b"x" * 64)
    barrier = threading.Barrier(11)

    def writer():
        barrier.wait()
        for i in range(2000):
            shared.seek(0)
            shared.write(b"a" * (64 + (i & 63)))

    def reader():
        barrier.wait()
        for _ in range(2000):
            shared.seek(0)
            shared.read()
            shared.seek(0)
            shared.getvalue()

    def exporter():
        barrier.wait()
        for _ in range(500):
            try:
                with shared.getbuffer() as m:
                    bytes(m[:8])
            except BufferError:
                pass

    ts = [threading.Thread(target=writer) for _ in range(2)]
    ts += [threading.Thread(target=reader) for _ in range(8)]
    ts.append(threading.Thread(target=exporter))
    for t in ts:
        t.start()
    for t in ts:
        t.join()
print("ok")
