"""pyperf stand-in used by the jit workstream (tools/bench/jitbench.sh).
Same measurement as tools/bench/shim (calibrated loops, warmup, median of BENCH_RUNS
runs, prints `BENCH <name> <seconds per loop> <runs>`) but needs only `sys` and `time`
(no argparse), so it runs while the stdlib is still being written.
With BENCH_LOOPS=N (and BENCH_RUNS=1 BENCH_WARMUPS=0) it runs a fixed amount of work,
for whole-process instruction counts under callgrind."""
import sys
try:
    import time
    perf_counter = time.perf_counter
except ImportError:
    def perf_counter():
        return 0.0
try:
    import os
    _env = os.environ
except ImportError:
    _env = {}


def _get(k, d):
    try:
        return _env.get(k, d)
    except Exception:
        return d


class _Args:
    def __getattr__(self, name):
        return None


class _ArgParser:
    def __init__(self):
        self.ns = _Args()

    def add_argument(self, *names, **k):
        dest = k.get('dest')
        if dest is None:
            for n in names:
                if n[:2] == '--':
                    dest = n[2:].replace('-', '_')
                    break
        if dest is not None:
            setattr(self.ns, dest, k.get('default'))

    def parse_args(self, *a, **k):
        return self.ns

    def parse_known_args(self, *a, **k):
        return self.ns, []


class Runner:
    def __init__(self, *args, **kwargs):
        self.metadata = {}
        self.argparser = _ArgParser()
        self.args = self.argparser.ns
        self.loops = int(_get('BENCH_LOOPS', 0)) or None
        self.runs = int(_get('BENCH_RUNS', 5))
        self.warmups = int(_get('BENCH_WARMUPS', 1))
        self.min_time = float(_get('BENCH_MIN_TIME', 0.1))

    def parse_args(self, args=None):
        return self.args

    def _calibrate(self, run_once):
        loops = 1
        while True:
            dt = run_once(loops)
            if dt >= self.min_time or loops >= 1 << 20:
                return loops
            loops *= 2

    def _measure(self, name, run_once, inner_loops=1):
        loops = self.loops or self._calibrate(run_once)
        for _ in range(self.warmups):
            run_once(loops)
        samples = []
        for _ in range(self.runs):
            dt = run_once(loops)
            samples.append(dt / (loops * inner_loops))
        samples.sort()
        med = samples[len(samples) // 2]
        print("BENCH %s %.9g %d" % (name, med, len(samples)))
        return med

    def bench_func(self, name, func, *args, inner_loops=None, **kw):
        def run_once(loops):
            t0 = perf_counter()
            for _ in range(loops):
                func(*args)
            return perf_counter() - t0
        return self._measure(name, run_once, inner_loops or 1)

    def bench_time_func(self, name, time_func, *args, inner_loops=None, **kw):
        def run_once(loops):
            return time_func(loops, *args)
        return self._measure(name, run_once, inner_loops or 1)

    def bench_async_func(self, name, func, *args, inner_loops=None, **kw):
        import asyncio
        def run_once(loops):
            t0 = perf_counter()
            for _ in range(loops):
                asyncio.run(func(*args))
            return perf_counter() - t0
        return self._measure(name, run_once, inner_loops or 1)


def python_implementation():
    return sys.implementation.name


def python_has_jit():
    return False
