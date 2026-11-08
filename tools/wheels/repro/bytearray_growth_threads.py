# Root cause (tornado's decompression-bomb tests: tracemalloc peak 968 MB for a 100 MB
# body): while other threads existed, every storage a growing bytearray replaced was kept
# until the next garbage collection (for readers in other threads that may still hold a
# slice), and growth never reallocated in place. Now only a bytearray that another
# thread has read is treated so; its owner otherwise reallocates/releases at once.
import threading
import tracemalloc

stop = threading.Event()
t = threading.Thread(target=stop.wait, daemon=True)
t.start()  # a multi-threaded process

chunk = b"x" * 65536
tracemalloc.start()
tracemalloc.reset_peak()
buf = bytearray()
for _ in range(512):  # 32 MB
    buf += chunk
size, peak = tracemalloc.get_traced_memory()
tracemalloc.stop()
stop.set()
assert len(buf) == 32 << 20
assert peak < 2 * len(buf), (size, peak)
print("OK")
