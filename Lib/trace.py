"""Trace or count the lines executed by a Python program.

Programmatic use:

    tracer = trace.Trace(count=True, trace=False,
                         ignoredirs=[sys.base_prefix])
    tracer.run('main()')
    tracer.results().write_results(show_missing=True, coverdir='/tmp')

Command line use: python -m trace --help
"""

__all__ = ['Trace', 'CoverageResults']

import gc
import inspect
import io
import linecache
import os
import sys
import sysconfig
import threading
import token
import tokenize
from time import monotonic as _clock

PRAGMA_NOCOVER = "#pragma NO COVER"


class _Ignore:
    """Decides (with memoization) whether a module is excluded from tracing.

    A module is ignored if its name is one of `modules` or a submodule of
    one, if it has no file name (built-in), or if its file lies in one of
    `dirs`."""

    def __init__(self, modules=None, dirs=None):
        self._modules = frozenset(modules or ())
        self._dirs = [os.path.normpath(d) for d in (dirs or ())]
        self._known = {'<string>': 1}

    def names(self, filename, modulename):
        try:
            return self._known[modulename]
        except KeyError:
            pass
        verdict = 1 if self._excluded(filename, modulename) else 0
        self._known[modulename] = verdict
        return verdict

    def _excluded(self, filename, modulename):
        if modulename in self._modules:
            return True
        if any(modulename.startswith(mod + '.') for mod in self._modules):
            return True
        if filename is None:
            return True
        return any(filename.startswith(d + os.sep) for d in self._dirs)


def _modname(path):
    """The file name of `path` without directory and extension."""
    return os.path.splitext(os.path.basename(path))[0]


def _fullmodname(path):
    """A dotted module name for `path`, relative to the longest matching
    sys.path entry."""
    normalized = os.path.normcase(path)
    best = ""
    for entry in sys.path:
        entry = os.path.normcase(entry)
        if (len(entry) > len(best) and normalized.startswith(entry)
                and normalized[len(entry):len(entry) + 1] == os.sep):
            best = entry
    relative = path[len(best) + 1:] if best else path
    relative = os.path.splitdrive(relative)[1]
    relative = relative.replace(os.sep, ".")
    if os.altsep:
        relative = relative.replace(os.altsep, ".")
    return os.path.splitext(relative)[0].lstrip(".")


class CoverageResults:
    """Line counts, called functions and caller relationships of a run.

    counts:      {(filename, lineno): hits}
    calledfuncs: {(filename, modulename, funcname): 1}
    callers:     {(caller_triple, callee_triple): 1}

    If `infile` names a file written by an earlier run (via `outfile`),
    its data is merged in.
    """

    def __init__(self, counts=None, calledfuncs=None, infile=None,
                 callers=None, outfile=None):
        self.counts = dict(counts or {})
        self.counter = self.counts.copy()
        self.calledfuncs = dict(calledfuncs or {})
        self.callers = dict(callers or {})
        self.infile = infile
        self.outfile = outfile
        if infile:
            self._merge_file(infile)

    def _merge_file(self, path):
        import pickle
        try:
            with open(path, 'rb') as stream:
                counts, calledfuncs, callers = pickle.load(stream)
        except (OSError, EOFError, ValueError) as err:
            print(f"Skipping counts file {path!r}: {err}", file=sys.stderr)
            return
        self.update(self.__class__(counts, calledfuncs, callers=callers))

    def is_ignored_filename(self, filename):
        """True for pseudo file names such as '<string>'."""
        return filename.startswith('<') and filename.endswith('>')

    def update(self, other):
        """Add the data of another CoverageResults to this one."""
        for key, hits in other.counts.items():
            self.counts[key] = self.counts.get(key, 0) + hits
        for key in other.calledfuncs:
            self.calledfuncs[key] = 1
        for key in other.callers:
            self.callers[key] = 1

    def _print_functions(self):
        print()
        print("functions called:")
        for filename, modulename, funcname in sorted(self.calledfuncs):
            print(f"filename: {filename}, modulename: {modulename}, "
                  f"funcname: {funcname}")

    def _print_callers(self):
        print()
        print("calling relationships:")
        current_file = current_callee_file = None
        for (caller, callee) in sorted(self.callers):
            pfile, pmod, pfunc = caller
            cfile, cmod, cfunc = callee
            if pfile != current_file:
                print()
                print("***", pfile, "***")
                current_file = pfile
                current_callee_file = None
            if cfile != pfile and cfile != current_callee_file:
                print("  -->", cfile)
                current_callee_file = cfile
            print(f"    {pmod}.{pfunc} -> {cmod}.{cfunc}")

    def write_results(self, show_missing=True, summary=False, coverdir=None, *,
                      ignore_missing_files=False):
        """Write a <module>.cover file per traced module.

        show_missing: mark executable lines that never ran with '>>>>>>'
        summary:      print per-module coverage percentages
        coverdir:     directory for the .cover files (default: next to
                      each module)
        ignore_missing_files: skip modules whose source no longer exists
        """
        if self.calledfuncs:
            self._print_functions()
        if self.callers:
            self._print_callers()

        hits_by_file = {}
        for (filename, lineno), hits in self.counts.items():
            hits_by_file.setdefault(filename, {})[lineno] = hits

        rows = {}
        for filename, hits in hits_by_file.items():
            if self.is_ignored_filename(filename):
                continue
            if filename.endswith(".pyc"):
                filename = filename[:-1]
            if ignore_missing_files and not os.path.isfile(filename):
                continue
            if coverdir is None:
                target_dir = os.path.dirname(os.path.abspath(filename))
                modulename = _modname(filename)
            else:
                target_dir = coverdir
                os.makedirs(target_dir, exist_ok=True)
                modulename = _fullmodname(filename)
            executable = _find_executable_linenos(filename) if show_missing else {}
            source = linecache.getlines(filename)
            with open(filename, 'rb') as stream:
                encoding = tokenize.detect_encoding(stream.readline)[0]
            coverpath = os.path.join(target_dir, modulename + ".cover")
            n_hits, n_lines = self.write_results_file(coverpath, source,
                                                      executable, hits, encoding)
            if summary and n_lines:
                percent = 100 if n_hits == n_lines else round(100 * n_hits / n_lines, 1)
                rows[modulename] = (n_lines, percent, modulename, filename)

        if summary and rows:
            print("lines   cov%   module   (path)")
            for name in sorted(rows):
                print("%5d   %5.1f%%   %s   (%s)" % rows[name])

        if self.outfile:
            import pickle
            try:
                with open(self.outfile, 'wb') as stream:
                    pickle.dump((self.counts, self.calledfuncs, self.callers),
                                stream, 1)
            except OSError as err:
                print(f"Can't save counts files because {err}", file=sys.stderr)

    def write_results_file(self, path, lines, lnotab, lines_hit, encoding=None):
        """Write one annotated source listing; returns (hit, executable)
        line counts."""
        try:
            out = open(path, "w", encoding=encoding)
        except OSError as err:
            print(f"trace: Could not open {path!r} for writing: {err} "
                  "- skipping", file=sys.stderr)
            return 0, 0
        hit = executable = 0
        with out:
            for lineno, line in enumerate(lines, 1):
                if lineno in lines_hit:
                    prefix = "%5d: " % lines_hit[lineno]
                    hit += 1
                    executable += 1
                elif lineno in lnotab and PRAGMA_NOCOVER not in line:
                    prefix = ">>>>>> "
                    executable += 1
                else:
                    prefix = "       "
                out.write(prefix + line.expandtabs(8))
        return hit, executable


def _find_lines_from_code(code, strs):
    """Line numbers of `code` (one level) that are not docstring lines."""
    return {lineno: 1 for _, _, lineno in code.co_lines()
            if lineno is not None and lineno not in strs}


def _find_lines(code, strs):
    """Line numbers of `code` and of all code objects nested in it."""
    found = {}
    pending = [code]
    while pending:
        current = pending.pop()
        found.update(_find_lines_from_code(current, strs))
        pending.extend(c for c in current.co_consts if inspect.iscode(c))
    return found


def _find_strings(filename, encoding=None):
    """Lines occupied by string-only statements (e.g. docstrings)."""
    lines = {}
    previous = token.INDENT  # a string at the very start is a docstring too
    with open(filename, encoding=encoding) as stream:
        for tok in tokenize.generate_tokens(stream.readline):
            if tok.type == token.STRING and previous == token.INDENT:
                for lineno in range(tok.start[0], tok.end[0] + 1):
                    lines[lineno] = 1
            previous = tok.type
    return lines


def _find_executable_linenos(filename):
    """Lines of `filename` that carry executable code."""
    try:
        with tokenize.open(filename) as stream:
            text = stream.read()
            encoding = stream.encoding
    except OSError as err:
        print(f"Not printing coverage data for {filename!r}: {err}",
              file=sys.stderr)
        return {}
    code = compile(text, filename, "exec")
    return _find_lines(code, _find_strings(filename, encoding))


class Trace:
    """Runs code under a trace function that counts or prints lines, or
    records called functions or caller/callee pairs."""

    def __init__(self, count=1, trace=1, countfuncs=0, countcallers=0,
                 ignoremods=(), ignoredirs=(), infile=None, outfile=None,
                 timing=False):
        """
        count:        count executed lines
        trace:        print each line before it executes
        countfuncs:   only record which functions were called
        countcallers: only record caller/callee pairs
        ignoremods:   module names (and their submodules) not to trace
        ignoredirs:   directories whose modules are not traced
        infile, outfile: files to merge earlier results from / save to
        timing:       prefix printed lines with the elapsed time
        """
        self.infile = infile
        self.outfile = outfile
        self.ignore = _Ignore(ignoremods, ignoredirs)
        self.counts = {}
        self.pathtobasename = {}
        self.trace = trace
        self._calledfuncs = {}
        self._callers = {}
        self._class_names = {}
        self.start_time = _clock() if timing else None
        self.donothing = 0
        if countcallers:
            self.globaltrace = self.globaltrace_trackcallers
        elif countfuncs:
            self.globaltrace = self.globaltrace_countfuncs
        elif trace or count:
            self.globaltrace = self.globaltrace_lt
            if trace and count:
                self.localtrace = self.localtrace_trace_and_count
            elif trace:
                self.localtrace = self.localtrace_trace
            else:
                self.localtrace = self.localtrace_count
        else:
            self.donothing = 1

    # ---- running code

    def run(self, cmd):
        import __main__
        namespace = __main__.__dict__
        self.runctx(cmd, namespace, namespace)

    def runctx(self, cmd, globals=None, locals=None):
        if globals is None:
            globals = {}
        if locals is None:
            locals = {}
        active = not self.donothing
        if active:
            threading.settrace(self.globaltrace)
            sys.settrace(self.globaltrace)
        try:
            exec(cmd, globals, locals)
        finally:
            # uninstall before any other Python-level call
            if active:
                sys.settrace(None)
                threading.settrace(None)

    def runfunc(self, func, /, *args, **kw):
        active = not self.donothing
        if active:
            sys.settrace(self.globaltrace)
        try:
            return func(*args, **kw)
        finally:
            if active:
                sys.settrace(None)

    # ---- identifying functions

    def file_module_function_of(self, frame):
        """(filename, modulename, funcname) of the code running in `frame`;
        methods are named Class.method where the class can be found."""
        code = frame.f_code
        filename = code.co_filename
        modulename = _modname(filename) if filename else None
        funcname = code.co_name
        if code not in self._class_names:
            self._class_names[code] = self._owning_class(code)
        owner = self._class_names[code]
        if owner is not None:
            funcname = f"{owner}.{funcname}"
        return filename, modulename, funcname

    @staticmethod
    def _owning_class(code):
        # code -> the one function using it -> the one dict holding that
        # function -> the one class owning that dict
        functions = [f for f in gc.get_referrers(code) if inspect.isfunction(f)]
        if len(functions) != 1:
            return None
        dicts = [d for d in gc.get_referrers(functions[0]) if isinstance(d, dict)]
        if len(dicts) != 1:
            return None
        classes = [c for c in gc.get_referrers(dicts[0]) if hasattr(c, "__bases__")]
        if len(classes) != 1:
            return None
        return classes[0].__name__

    # ---- trace functions

    def globaltrace_trackcallers(self, frame, why, arg):
        """'call' handler recording (caller, callee) pairs."""
        if why == 'call':
            callee = self.file_module_function_of(frame)
            caller = self.file_module_function_of(frame.f_back)
            self._callers[(caller, callee)] = 1

    def globaltrace_countfuncs(self, frame, why, arg):
        """'call' handler recording called functions."""
        if why == 'call':
            self._calledfuncs[self.file_module_function_of(frame)] = 1

    def globaltrace_lt(self, frame, why, arg):
        """'call' handler returning the local trace function for frames of
        modules that are not ignored."""
        if why != 'call':
            return None
        filename = frame.f_globals.get('__file__', None)
        if not filename:
            return None
        modulename = _modname(filename)
        if modulename is None or self.ignore.names(filename, modulename):
            return None
        if self.trace:
            print(f" --- modulename: {modulename}, funcname: {frame.f_code.co_name}")
        return self.localtrace

    def _count_line(self, frame):
        key = (frame.f_code.co_filename, frame.f_lineno)
        self.counts[key] = self.counts.get(key, 0) + 1

    def _print_line(self, frame):
        filename = frame.f_code.co_filename
        lineno = frame.f_lineno
        if self.start_time:
            print('%.2f' % (_clock() - self.start_time), end=' ')
        text = linecache.getline(filename, lineno)
        location = f"{os.path.basename(filename)}({lineno})"
        if text:
            print(location + ": ", text, end='')
        else:
            print(location)

    def localtrace_trace_and_count(self, frame, why, arg):
        if why == "line":
            self._count_line(frame)
            self._print_line(frame)
        return self.localtrace

    def localtrace_trace(self, frame, why, arg):
        if why == "line":
            self._print_line(frame)
        return self.localtrace

    def localtrace_count(self, frame, why, arg):
        if why == "line":
            self._count_line(frame)
        return self.localtrace

    def results(self):
        return CoverageResults(self.counts, infile=self.infile,
                               outfile=self.outfile,
                               calledfuncs=self._calledfuncs,
                               callers=self._callers)


def _build_parser():
    import argparse

    parser = argparse.ArgumentParser(color=True)
    parser.add_argument('--version', action='version', version='trace 2.0')

    main_group = parser.add_argument_group(
        'Main options', 'One of these (or --report) must be given')
    main_group.add_argument('-c', '--count', action='store_true',
                            help='count executed lines and write <module>.cover '
                                 'files (see --coverdir, --file, --no-report)')
    main_group.add_argument('-t', '--trace', action='store_true',
                            help='print each line before it is executed')
    main_group.add_argument('-l', '--listfuncs', action='store_true',
                            help='list the functions executed at least once '
                                 '(not together with --trace or --count)')
    main_group.add_argument('-T', '--trackcalls', action='store_true',
                            help='list caller/callee pairs')

    modifiers = parser.add_argument_group('Modifiers')
    report_choice = modifiers.add_mutually_exclusive_group()
    report_choice.add_argument('-r', '--report', action='store_true',
                               help='report from a counts file (--file) '
                                    'without running any code')
    report_choice.add_argument('-R', '--no-report', action='store_true',
                               help='do not write coverage report files')
    modifiers.add_argument('-f', '--file',
                           help='file to accumulate counts in over several runs')
    modifiers.add_argument('-C', '--coverdir',
                           help='directory for the .cover report files')
    modifiers.add_argument('-m', '--missing', action='store_true',
                           help="mark lines that never ran with '>>>>>> '")
    modifiers.add_argument('-s', '--summary', action='store_true',
                           help='print a per-module summary (with --count '
                                'or --report)')
    modifiers.add_argument('-g', '--timing', action='store_true',
                           help='prefix traced lines with the elapsed time')

    filters = parser.add_argument_group('Filters', 'Can be specified multiple times')
    filters.add_argument('--ignore-module', action='append', default=[],
                         help='comma separated module names to ignore '
                              '(including submodules)')
    filters.add_argument('--ignore-dir', action='append', default=[],
                         help='directories to ignore (os.pathsep separated; '
                              '$prefix and $exec_prefix are expanded)')

    parser.add_argument('--module', action='store_true', default=False,
                        help='trace a module (progname is a module name)')
    parser.add_argument('progname', nargs='?',
                        help='file to run as main program')
    import argparse as _argparse
    parser.add_argument('arguments', nargs=_argparse.REMAINDER,
                        help='arguments to the program')
    return parser


def main():
    parser = _build_parser()
    opts = parser.parse_args()

    stdlib_dir = sysconfig.get_path("stdlib")
    platstdlib_dir = sysconfig.get_path("platstdlib")

    def expand_dir(text):
        text = os.path.expanduser(os.path.expandvars(text))
        text = text.replace('$prefix', stdlib_dir).replace('$exec_prefix', platstdlib_dir)
        return os.path.normpath(text)

    ignore_modules = [name.strip() for item in opts.ignore_module
                      for name in item.split(',')]
    ignore_dirs = [expand_dir(d) for item in opts.ignore_dir
                   for d in item.split(os.pathsep)]

    if opts.report:
        if not opts.file:
            parser.error('-r/--report requires -f/--file')
        results = CoverageResults(infile=opts.file, outfile=opts.file)
        return results.write_results(opts.missing, opts.summary, opts.coverdir)

    if not (opts.trace or opts.count or opts.listfuncs or opts.trackcalls):
        parser.error('must specify one of --trace, --count, --report, '
                     '--listfuncs, or --trackcalls')
    if opts.listfuncs and (opts.count or opts.trace):
        parser.error('cannot specify both --listfuncs and (--trace or --count)')
    if opts.summary and not opts.count:
        parser.error('--summary can only be used with --count or --report')
    if opts.progname is None:
        parser.error('progname is missing: required with the main options')

    tracer = Trace(opts.count, opts.trace, countfuncs=opts.listfuncs,
                   countcallers=opts.trackcalls, ignoremods=ignore_modules,
                   ignoredirs=ignore_dirs, infile=opts.file,
                   outfile=opts.file, timing=opts.timing)
    try:
        if opts.module:
            import runpy
            _, spec, code = runpy._get_module_details(opts.progname)
            sys.argv = [code.co_filename, *opts.arguments]
            namespace = {
                '__name__': '__main__',
                '__file__': code.co_filename,
                '__package__': spec.parent,
                '__loader__': spec.loader,
                '__spec__': spec,
                '__cached__': None,
            }
        else:
            sys.argv = [opts.progname, *opts.arguments]
            sys.path[0] = os.path.dirname(opts.progname)
            with io.open_code(opts.progname) as stream:
                code = compile(stream.read(), opts.progname, 'exec')
            namespace = {
                '__file__': opts.progname,
                '__name__': '__main__',
                '__package__': None,
                '__cached__': None,
            }
        tracer.runctx(code, namespace, namespace)
    except OSError as err:
        sys.exit(f"Cannot run file {sys.argv[0]!r} because: {err}")
    except SystemExit:
        pass

    if not opts.no_report:
        tracer.results().write_results(opts.missing, opts.summary, opts.coverdir)


if __name__ == '__main__':
    main()
