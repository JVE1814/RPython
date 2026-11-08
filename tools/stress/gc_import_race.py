# (scale3) Fresh modules imported and dropped by several threads at once: module
# dicts, functions, classes and code objects become cyclic garbage while other threads'
# reference-count tables (sharded counting) and parallel young collections are busy.
# A delta left in a helper thread's table for an object the collector unshared and
# freed corrupted the heap (double free / invalid pointer aborts).
import importlib, os, shutil, sys, tempfile, threading

NTHREADS = 4
PER_THREAD = int(sys.argv[1]) if len(sys.argv) > 1 else 300
BODY = "".join("def f%d(x):\n    return x + %d\n\nclass K%d:\n    a = %d\n    def m(self):\n        return f%d\n\n"
               % (i, i, i, i, i) for i in range(20))

d = tempfile.mkdtemp(prefix="gc_import_race_")
errors = []


def worker(tid):
    try:
        for k in range(PER_THREAD):
            name = "gcir_%d_%d" % (tid, k)
            m = importlib.import_module(name)
            if m.f3(1) != 4 or m.K7.a != 7 or m.K5().m()(1) != 6:
                raise SystemExit("module contents broken")
            del sys.modules[name]
    except BaseException as e:
        errors.append(repr(e))


def main():
    try:
        for t in range(NTHREADS):
            for k in range(PER_THREAD):
                with open(os.path.join(d, "gcir_%d_%d.py" % (t, k)), "w") as f:
                    f.write(BODY)
        sys.path.insert(0, d)
        ts = [threading.Thread(target=worker, args=(i,)) for i in range(NTHREADS)]
        for t in ts:
            t.start()
        for t in ts:
            t.join()
    finally:
        shutil.rmtree(d, ignore_errors=True)
    if errors:
        raise SystemExit("worker failed: " + errors[0])
    print("ok")


main()
