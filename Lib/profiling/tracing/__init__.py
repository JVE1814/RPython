"""Deterministic profiler built on the _lsprof extension.

Profile() records every Python and builtin function call while it is
enabled; pstats.Stats(profile) turns the result into reports.  run() and
runctx() profile a single statement.
"""

import _lsprof

__all__ = ["run", "runctx", "Profile"]


def run(statement, filename=None, sort=-1):
    """Profile exec(statement) in __main__'s namespace, then print the
    report (or save it to `filename`)."""
    import __main__
    namespace = __main__.__dict__
    return _profile_and_report(Profile(), statement, namespace, namespace,
                               filename, sort)


def runctx(statement, globals, locals, filename=None, sort=-1):
    """Like run() with explicit global and local namespaces."""
    return _profile_and_report(Profile(), statement, globals, locals,
                               filename, sort)


def _profile_and_report(profiler, statement, globals, locals, filename, sort):
    try:
        profiler.runctx(statement, globals, locals)
    except SystemExit:
        pass
    finally:
        if filename is not None:
            profiler.dump_stats(filename)
        else:
            profiler.print_stats(sort)


def label(code):
    """The (file, line, name) key pstats uses for a stats entry."""
    if isinstance(code, str):
        return ('~', 0, code)
    return (code.co_filename, code.co_firstlineno, code.co_name)


class Profile(_lsprof.Profiler):
    """Profile(timer=None, timeunit=0.0, subcalls=True, builtins=True)

    Deterministic profiler.  Use enable()/disable(), the context manager
    protocol, runcall() or runctx(); then create_stats() fills `stats` in
    the format pstats expects."""

    def print_stats(self, sort=-1):
        import pstats
        keys = sort if isinstance(sort, tuple) else (sort,)
        pstats.Stats(self).strip_dirs().sort_stats(*keys).print_stats()

    def dump_stats(self, file):
        import marshal
        self.create_stats()
        with open(file, 'wb') as stream:
            marshal.dump(self.stats, stream)

    def create_stats(self):
        self.disable()
        self.snapshot_stats()

    def snapshot_stats(self):
        entries = self.getstats()
        stats = {}
        callers_by_key = {}
        for entry in entries:
            key = label(entry.code)
            callers = {}
            callers_by_key[key] = callers
            primitive = entry.callcount - entry.reccallcount
            stats[key] = (primitive, entry.callcount, entry.inlinetime,
                          entry.totaltime, callers)
        for entry in entries:
            if not entry.calls:
                continue
            caller = label(entry.code)
            for sub in entry.calls:
                callee = label(sub.code)
                callers = callers_by_key.get(callee)
                if callers is None:
                    continue
                record = (sub.callcount, sub.callcount - sub.reccallcount,
                          sub.inlinetime, sub.totaltime)
                if caller in callers:
                    record = tuple(a + b for a, b in zip(callers[caller], record))
                callers[caller] = record
        self.stats = stats

    # ---- convenience runners

    def run(self, cmd):
        import __main__
        namespace = __main__.__dict__
        return self.runctx(cmd, namespace, namespace)

    def runctx(self, cmd, globals, locals):
        self.enable()
        try:
            exec(cmd, globals, locals)
        finally:
            self.disable()
        return self

    def runcall(self, func, /, *args, **kw):
        self.enable()
        try:
            return func(*args, **kw)
        finally:
            self.disable()

    def __enter__(self):
        self.enable()
        return self

    def __exit__(self, *exc_info):
        self.disable()


def _runctx_cli(code, globals, locals, filename, sort):
    return runctx(code, globals, locals, filename, sort)


def main():
    from profiling.tracing import _cli
    _cli.main(_runctx_cli, "python -m profiling.tracing")


if __name__ == '__main__':
    main()
