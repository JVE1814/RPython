# Root cause (the full pandas suite aborted with "memory allocation of 8589934592 bytes
# failed" in young::compact, after core's scale3 merge): every object that becomes
# tracked is appended to its thread's young buffer; a block freed and reused for the
# next tracked object is appended again while the old entry still matches, and
# compaction kept every such duplicate. A thread that allocates and frees tracked
# objects without collections in between (here: the collector disabled) grew the buffer
# by one entry per allocation. Compaction now drops duplicate addresses.
import gc
import resource

gc.disable()
before = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
for i in range(20_000_000):
    x = [i]  # a tracked object, freed at the next iteration
    del x
after = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
gc.enable()
grown_mb = (after - before) / 1024
assert grown_mb < 64, f"max RSS grew by {grown_mb:.0f} MB"
print("OK")
