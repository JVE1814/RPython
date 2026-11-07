# Stress: several threads assigning __class__ of one shared object back and forth while
# others call type() on it (test_super). Each assignment must release exactly the type
# it replaced, after concurrent type() calls can no longer be using it.
import gc
import sys
import threading

# regrtest installs an audit hook; with one, __class__ assignment calls Python code in
# the middle of the operation (wider race window)
sys.addaudithook(lambda event, args: None)


class Foo:
    pass


class Bar:
    pass


foo = Foo()


def worker():
    for _ in range(3000):
        foo.__class__ = Bar
        type(foo)
        foo.__class__ = Foo
        type(foo)


for _ in range(5):
    ts = [threading.Thread(target=worker) for _ in range(6)]
    for t in ts:
        t.start()
    for t in ts:
        t.join()
    gc.collect()
assert type(foo) in (Foo, Bar)
print("ok")
