"""Deterministic profiler written in Python (deprecated).

This module hooks sys.setprofile() and is much slower than
profiling.tracing (cProfile), which should be used instead.  Its
statistics have the same layout, except that the caller tables hold plain
call counts.
"""

import sys
import time
import marshal
import warnings

warnings.warn(
    "The profile module is deprecated and will be removed in Python 3.17; "
    "use profiling.tracing (cProfile) instead.",
    DeprecationWarning, stacklevel=2)

__all__ = ["run", "runctx", "Profile"]


def run(statement, filename=None, sort=-1):
    """Profile exec(statement) in __main__ and print or save the report."""
    import __main__
    namespace = __main__.__dict__
    return _report(Profile(), statement, namespace, namespace, filename, sort)


def runctx(statement, globals, locals, filename=None, sort=-1):
    """Like run() with explicit namespaces."""
    return _report(Profile(), statement, globals, locals, filename, sort)


def _report(profiler, statement, globals, locals, filename, sort):
    try:
        profiler.runctx(statement, globals, locals)
    except SystemExit:
        pass
    if filename is not None:
        profiler.dump_stats(filename)
    else:
        return profiler.print_stats(sort)


class _Record:
    """An active call on the profiler's stack."""
    __slots__ = ('key', 'ident', 'own', 'children')

    def __init__(self, key, ident):
        self.key = key          # (file, line, name)
        self.ident = ident      # the frame, or the builtin being called
        self.own = 0            # time spent in the function itself
        self.children = 0       # time spent in profiled callees


class Profile:
    """Profile(timer=None, bias=None)

    `timer` returns the current time (a number, or a sequence of numbers
    that are added up); the default is time.process_time.  `bias` is
    subtracted from every measured interval to compensate the profiler's
    own overhead (see calibrate())."""

    bias = 0

    def __init__(self, timer=None, bias=None):
        self.timings = {}
        self.stats = {}
        self._stack = []
        self._active = {}
        if bias is not None:
            self.bias = bias
        if timer is None:
            timer = time.process_time
        self.timer = timer
        sample = timer()
        if isinstance(sample, (tuple, list)):
            self.get_time = lambda: sum(timer())
        else:
            self.get_time = timer
        self._last = self.get_time()
        self.set_cmd('')

    # ---- event handling

    def trace_dispatch(self, frame, event, arg):
        elapsed = self.get_time() - self._last - self.bias
        if event == 'call':
            self._push(elapsed, self._frame_key(frame), frame)
        elif event == 'return':
            self._pop(elapsed, frame)
        elif event == 'c_call':
            self._push(elapsed, ('', 0, getattr(arg, '__name__', repr(arg))), arg)
        elif event in ('c_return', 'c_exception'):
            self._pop(elapsed, arg)
        else:
            self._charge(elapsed)
        self._last = self.get_time()

    dispatcher = trace_dispatch

    @staticmethod
    def _frame_key(frame):
        code = frame.f_code
        return (code.co_filename, code.co_firstlineno, code.co_name)

    def _charge(self, elapsed):
        if self._stack:
            self._stack[-1].own += elapsed

    def _push(self, elapsed, key, ident):
        self._charge(elapsed)
        self._stack.append(_Record(key, ident))
        self._active[key] = self._active.get(key, 0) + 1

    def _pop(self, elapsed, ident):
        self._charge(elapsed)
        stack = self._stack
        if not any(record.ident is ident for record in stack):
            return              # a call that started before profiling
        while stack:
            record = stack.pop()
            self._finish(record)
            if record.ident is ident:
                break

    def _finish(self, record):
        key = record.key
        total = record.own + record.children
        parent = self._stack[-1] if self._stack else None
        if parent is not None:
            parent.children += total
        depth = self._active[key] - 1
        self._active[key] = depth
        primitive, ncalls, own, cumulative, callers = self.timings.get(
            key, (0, 0, 0, 0, {}))
        if depth == 0:
            primitive += 1
            cumulative += total
        if parent is not None:
            callers[parent.key] = callers.get(parent.key, 0) + 1
        self.timings[key] = (primitive, ncalls + 1, own + record.own,
                             cumulative, callers)

    def _flush(self):
        while self._stack:
            self._finish(self._stack.pop())

    # ---- results

    def set_cmd(self, cmd):
        self.cmd = cmd

    def simulate_call(self, name):
        """Open a pseudo call named `name` (kept for compatibility)."""
        self._push(0, ('profile', 0, name), name)

    def simulate_cmd_complete(self):
        """Close every open call, charging the time spent so far."""
        self._charge(self.get_time() - self._last - self.bias)
        self._flush()
        self._last = self.get_time()

    def print_stats(self, sort=-1):
        import pstats
        keys = sort if isinstance(sort, tuple) else (sort,)
        pstats.Stats(self).strip_dirs().sort_stats(*keys).print_stats()

    def dump_stats(self, file):
        with open(file, 'wb') as stream:
            self.create_stats()
            marshal.dump(self.stats, stream)

    def create_stats(self):
        self.simulate_cmd_complete()
        self.snapshot_stats()

    def snapshot_stats(self):
        self.stats = {key: (primitive, ncalls, own, cumulative, dict(callers))
                      for key, (primitive, ncalls, own, cumulative, callers)
                      in self.timings.items()}

    # ---- running code

    def run(self, cmd):
        import __main__
        namespace = __main__.__dict__
        return self.runctx(cmd, namespace, namespace)

    def runctx(self, cmd, globals, locals):
        self.set_cmd(cmd)
        self._last = self.get_time()
        sys.setprofile(self.dispatcher)
        try:
            exec(cmd, globals, locals)
        finally:
            sys.setprofile(None)
        return self

    def runcall(self, func, /, *args, **kw):
        self.set_cmd(repr(func))
        self._last = self.get_time()
        sys.setprofile(self.dispatcher)
        try:
            return func(*args, **kw)
        finally:
            sys.setprofile(None)

    # ---- overhead calibration

    def calibrate(self, m, verbose=0):
        """Estimate the profiler overhead per event, in timer units, from `m`
        calls of a small test function; the result is a suitable `bias`."""
        if self.__class__ is not Profile:
            raise TypeError("Subclasses must override .calibrate().")
        saved = self.bias
        self.bias = 0
        try:
            return self._measure_overhead(m, verbose)
        finally:
            self.bias = saved

    def _measure_overhead(self, m, verbose):
        get_time = self.get_time

        def leaf(x):
            for _ in range(x):
                pass

        def driver(m):
            for _ in range(m):
                leaf(100)

        start = get_time()
        driver(m)
        plain = get_time() - start
        if verbose:
            print("elapsed time without profiling =", plain)

        profiler = Profile(self.timer, 0)
        start = get_time()
        profiler.runctx('driver(m)', globals(), {'driver': driver, 'm': m})
        profiled = get_time() - start
        if verbose:
            print("elapsed time with profiling =", profiled)

        # time the profiler attributed to the two test functions
        reported = sum(own for key, (_, _, own, _, _) in profiler.timings.items()
                       if key[2] in ('leaf', 'driver'))
        if verbose:
            print("'CPU seconds' profiler reported =", reported)
            print("total # calls =", m + 1)
        if m + 1 == 0:
            raise ValueError("internal error: total calls = %d" % (m + 1))
        # every call generates two events
        per_event = (reported - plain) / (2 * (m + 1))
        if verbose:
            print("mean stopwatch overhead per profile event =", per_event)
        return per_event


def main():
    from profiling.tracing import _cli
    _cli.main(runctx, "profile.py")


if __name__ == '__main__':
    main()
