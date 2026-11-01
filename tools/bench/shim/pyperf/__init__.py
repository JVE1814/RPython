"""Minimal pyperf-compatible runner used to run pyperformance benchmarks identically
on our interpreter and on the baseline CPython builds (fair comparison).
Prints one line: BENCH <name> <median seconds per inner loop> <runs>."""
import sys, time, argparse

perf_counter = time.perf_counter

class Runner:
    def __init__(self, *args, **kwargs):
        self.metadata = {}
        self.argparser = argparse.ArgumentParser() if 'argparse' in sys.modules else None
        self._add_cmdline_args = kwargs.get('add_cmdline_args')
        self.args = None
        self.loops = int(_env('BENCH_LOOPS', 0)) or None
        self.runs = int(_env('BENCH_RUNS', 5))
        self.warmups = int(_env('BENCH_WARMUPS', 1))
        self.min_time = float(_env('BENCH_MIN_TIME', 0.1))

    def parse_args(self, args=None):
        if self.argparser is not None and self.args is None:
            self.args, _ = self.argparser.parse_known_args(args if args is not None else [])
        return self.args

    def _calibrate(self, run_once):
        loops = 1
        while True:
            dt = run_once(loops)
            if dt >= self.min_time or loops >= 1 << 20:
                return loops
            loops *= 2

    def _measure(self, name, run_once, inner_loops=1):
        self.parse_args()
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
        sys.stdout.flush()
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

    def bench_command(self, name, command, **kw):
        import subprocess
        def run_once(loops):
            t0 = perf_counter()
            for _ in range(loops):
                subprocess.run(command, check=True)
            return perf_counter() - t0
        return self._measure(name, run_once)


def _env(k, d):
    import os
    return os.environ.get(k, d)


def python_implementation():
    return sys.implementation.name


def python_has_jit():
    return False
