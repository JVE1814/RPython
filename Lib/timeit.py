"""Tool for measuring execution time of small code snippets.

Library use: Timer(stmt, setup, timer, globals) and the shortcut functions
timeit() and repeat().  Command line use: python -m timeit --help.

The statement is compiled into a function that runs it in a loop, so the
measured time excludes the setup and most of the loop overhead; garbage
collection is switched off while timing.  When the number of loops is not
given on the command line it is chosen automatically (1, 2, 5, 10, 20, ...)
so that one measurement takes at least 0.2 seconds.
"""

import gc
import itertools
import sys
import time

__all__ = ["Timer", "timeit", "repeat", "default_timer"]

dummy_src_name = "<timeit-src>"
default_number = 1000000
default_repeat = 5
default_timer = time.perf_counter

_globals = globals

# Source of the generated timing function.  {setup} is placed at an
# indentation of 4 spaces and {stmt} at 8 spaces (see _indent()).
template = """
def inner(_it, _timer{init}):
    {setup}
    _t0 = _timer()
    for _i in _it:
        {stmt}
        pass
    _t1 = _timer()
    return _t1 - _t0
"""


def reindent(src, indent):
    """Indent every line of `src` but the first by `indent` spaces."""
    return src.replace("\n", "\n" + " " * indent)


def _prepare(code, what, indent, prefix, namespace, init):
    """Turn a statement (string or callable) into template text.

    Returns (text, init) where init lists extra parameters of inner()."""
    if isinstance(code, str):
        # it must also compile on its own, outside of the generated function
        compile(prefix + code, dummy_src_name, "exec")
        return reindent(code, indent), init
    if callable(code):
        key = '_' + what
        namespace[key] = code
        return key + '()', init + f', {key}={key}'
    raise ValueError(f"{what} is neither a string nor callable")


class Timer:
    """Time the execution of a statement.

    Timer(stmt='pass', setup='pass', timer=default_timer, globals=None)

    stmt and setup are source strings (which may contain newlines but no
    multi-line string literals) or argument-less callables.  The code runs
    in the namespace `globals` (default: this module's namespace).  Use
    timeit() for one measurement, repeat() for several and autorange() to
    pick a loop count.
    """

    def __init__(self, stmt="pass", setup="pass", timer=default_timer,
                 globals=None):
        self.timer = timer
        namespace = {}
        global_ns = _globals() if globals is None else globals
        setup_text, init = _prepare(setup, 'setup', 4, '', namespace, '')
        prefix = setup + '\n' if isinstance(setup, str) else ''
        stmt_text, init = _prepare(stmt, 'stmt', 8, prefix, namespace, init)
        self.src = template.format(stmt=stmt_text, setup=setup_text, init=init)
        exec(compile(self.src, dummy_src_name, "exec"), global_ns, namespace)
        self.inner = namespace["inner"]

    def print_exc(self, file=None):
        """Print the traceback of the exception being handled, showing the
        lines of the generated timing code (file defaults to sys.stderr)."""
        import linecache
        import traceback
        if self.src is not None:
            lines = self.src.split("\n")
            linecache.cache[dummy_src_name] = (len(self.src), None, lines,
                                               dummy_src_name)
        traceback.print_exc(file=file)

    def timeit(self, number=default_number):
        """Run the setup once and the statement `number` times; return the
        elapsed time (in seconds for the default timer)."""
        loops = itertools.repeat(None, number)
        collecting = gc.isenabled()
        gc.disable()
        try:
            return self.inner(loops, self.timer)
        finally:
            if collecting:
                gc.enable()

    def repeat(self, repeat=default_repeat, number=default_number):
        """Return a list of `repeat` results of timeit(number).

        The minimum is usually the most meaningful value; larger ones are
        mostly caused by other activity on the machine."""
        return [self.timeit(number) for _ in range(repeat)]

    def autorange(self, callback=None, target_time=0.2):
        """Find a loop count from 1, 2, 5, 10, 20, 50, ... for which one
        measurement takes at least `target_time` seconds; return
        (number, time_taken).  `callback(number, time_taken)` is called
        after each attempt if given."""
        magnitude = 1
        while True:
            for step in (1, 2, 5):
                number = magnitude * step
                taken = self.timeit(number)
                if callback:
                    callback(number, taken)
                if taken >= target_time:
                    return (number, taken)
            magnitude *= 10


def timeit(stmt="pass", setup="pass", timer=default_timer,
           number=default_number, globals=None):
    """Shortcut for Timer(stmt, setup, timer, globals).timeit(number)."""
    return Timer(stmt, setup, timer, globals).timeit(number)


def repeat(stmt="pass", setup="pass", timer=default_timer,
           repeat=default_repeat, number=default_number, globals=None):
    """Shortcut for Timer(stmt, setup, timer, globals).repeat(repeat, number)."""
    return Timer(stmt, setup, timer, globals).repeat(repeat, number)


_UNITS = {"nsec": 1e-9, "usec": 1e-6, "msec": 1e-3, "sec": 1.0}


def _command_line_parser():
    import argparse
    parser = argparse.ArgumentParser(
        prog="python -m timeit",
        description="Tool for measuring execution time of small code snippets.",
        epilog="Without -n, the loop count is the first of 1, 2, 5, 10, 20, "
               "50, ... for which the total time is at least 0.2 seconds.",
        color=True)
    parser.add_argument("-n", "--number", metavar="N", type=int, default=0,
                        help="how many times to execute 'statement'")
    parser.add_argument("-r", "--repeat", metavar="N", type=int,
                        default=default_repeat,
                        help="how many times to repeat the timer "
                             f"(default {default_repeat})")
    parser.add_argument("-s", "--setup", metavar="S", action="append",
                        default=[],
                        help="statement run once before timing (default "
                             "'pass'); may be given several times")
    parser.add_argument("-u", "--unit", choices=tuple(_UNITS),
                        help="time unit for the output")
    parser.add_argument("-p", "--process", action="store_true",
                        help="measure process time (time.process_time) "
                             "instead of wall-clock time")
    parser.add_argument("-v", "--verbose", action="count", default=0,
                        help="print raw timing results; repeat for more "
                             "digits of precision")
    parser.add_argument("statement", nargs="*",
                        help="statement to be timed (default 'pass')")
    return parser


def main(args=None, *, _wrap_timer=None):
    """Command line entry point.

    `args` defaults to sys.argv[1:].  Returns None on success or an exit
    status: 1 when the timed code raised (the traceback goes to
    sys.stderr), 2 for usage errors.  `_wrap_timer`, used by the tests,
    maps the timer function to a replacement.
    """
    if args is None:
        args = sys.argv[1:]
    try:
        options = _command_line_parser().parse_args(args)
    except SystemExit as exit:
        return exit.code

    stmt = "\n".join(options.statement) or "pass"
    setup = "\n".join(options.setup) or "pass"
    number = options.number
    repeat = max(options.repeat, 1)
    verbose = options.verbose
    unit = options.unit
    digits = 3 + max(verbose - 1, 0)
    timer = time.process_time if options.process else default_timer

    from _colorize import get_theme
    theme = get_theme(tty_file=sys.stdout).timeit

    # make modules in the current directory importable
    import os
    sys.path.insert(0, os.curdir)
    if _wrap_timer is not None:
        timer = _wrap_timer(timer)

    def fmt(seconds):
        if unit is not None:
            chosen, scale = unit, _UNITS[unit]
        else:
            # the largest unit not exceeding the value (seconds for tiny values)
            chosen, scale = 'nsec', 1e-9
            for name, size in sorted(_UNITS.items(), key=lambda kv: kv[1]):
                if seconds >= size:
                    chosen, scale = name, size
        return "%.*g %s" % (digits, seconds / scale, chosen)

    t = Timer(stmt, setup, timer)
    if number == 0:
        report = None
        if verbose:
            def report(loops, taken):
                plural = '' if loops == 1 else 's'
                print(f"{loops} loop{plural} {theme.punctuation}-> "
                      f"{theme.timing}{taken:.{digits}g} sec{theme.reset}")
        try:
            number, _ = t.autorange(report)
        except:
            t.print_exc()
            return 1
        if verbose:
            print()

    try:
        totals = t.repeat(repeat, number)
    except:
        t.print_exc()
        return 1

    if verbose:
        shown = f"{theme.punctuation}, ".join(theme.timing + fmt(x) for x in totals)
        print(f"raw times: {shown}{theme.reset}")
        print()

    per_loop = [x / number for x in totals]
    best, worst = min(per_loop), max(per_loop)
    plural = '' if number == 1 else 's'
    print(f"{number} loop{plural}, best of {repeat}: "
          f"{theme.best}{fmt(best)}{theme.reset} "
          f"{theme.per_loop}per loop{theme.reset}")
    if worst >= best * 4:
        import warnings
        warnings.warn_explicit(
            f"The test results are likely unreliable. The worst time "
            f"({fmt(worst)}) was more than four times slower than the "
            f"best time ({fmt(best)}).", UserWarning, '', 0)
    return None


if __name__ == "__main__":
    sys.exit(main())
