"""pyperf stand-in for whole-process measurements (jit workstream).
Runs each benchmark function a fixed number of loops (BENCH_LOOPS, default 1) without
timing anything itself, so it needs no stdlib modules beyond sys/os-free builtins.
The caller measures the process (wall time best-of-N, or callgrind instructions) and
runs the identical script on the baseline interpreters."""
import sys

_LOOPS = 1
for _a in sys.argv:
    pass


def perf_counter():
    return 0.0


def _loops():
    import sys as _s
    for a in _s.argv:
        if a.startswith('--jitshim-loops='):
            return int(a.split('=', 1)[1])
    return _LOOPS


class _Args:
    def __getattr__(self, name):
        return None


class _ArgParser:
    def add_argument(self, *a, **k):
        pass

    def parse_args(self, *a, **k):
        return _Args()

    def parse_known_args(self, *a, **k):
        return _Args(), []


class Runner:
    def __init__(self, *args, **kwargs):
        self.metadata = {}
        self.argparser = _ArgParser()
        self.args = _Args()

    def parse_args(self, args=None):
        return self.args

    def bench_func(self, name, func, *args, inner_loops=None, **kw):
        for _ in range(_loops()):
            func(*args)
        print("BENCH", name, "done")

    def bench_time_func(self, name, time_func, *args, inner_loops=None, **kw):
        time_func(_loops(), *args)
        print("BENCH", name, "done")

    def bench_async_func(self, name, func, *args, inner_loops=None, **kw):
        import asyncio
        for _ in range(_loops()):
            asyncio.run(func(*args))
        print("BENCH", name, "done")


def python_implementation():
    return sys.implementation.name


def python_has_jit():
    return False
