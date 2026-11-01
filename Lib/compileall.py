"""Byte-compile whole directory trees (or single files) of Python sources.

The cached bytecode is written with py_compile (see that module for the
format of our .pyc files).  Directories are walked recursively up to a
configurable depth; files can be compiled for several optimization levels,
optionally hard-linking identical results.  Run as a script for the
command line interface.
"""

import importlib.util
import os
import py_compile
import struct
import sys

__all__ = ["compile_dir", "compile_file", "compile_path"]


def _levels(optimize):
    if isinstance(optimize, int):
        return [optimize]
    return sorted(set(optimize)) if optimize is not None else [-1]


def _check_hardlink(optimize, hardlink_dupes):
    if not hardlink_dupes:
        return
    seq = [optimize] if isinstance(optimize, int) else list(optimize)
    if len(seq) < 2 or len(set(seq)) != len(seq):
        raise ValueError("Hardlinking of duplicated bytecode makes sense "
                         "only for more than one optimization level")


def _sources_in(directory, maxlevels, quiet):
    """Yield the paths of the files to consider below `directory`."""
    pending = [(os.fspath(directory), maxlevels)]
    while pending:
        current, levels = pending.pop(0)
        if not quiet:
            print(f'Listing {current!r}...')
        try:
            names = sorted(os.listdir(current))
        except OSError:
            if quiet < 2:
                print(f"Can't list {current!r}")
            continue
        subdirs = []
        for name in names:
            if name == '__pycache__':
                continue
            path = os.path.join(current, name)
            if not os.path.isdir(path):
                yield path
            elif (levels > 0 and name not in (os.curdir, os.pardir)
                  and not os.path.islink(path)):
                subdirs.append((path, levels - 1))
        pending[0:0] = subdirs


def compile_dir(dir, maxlevels=None, ddir=None, force=False,
                rx=None, quiet=0, legacy=False, optimize=-1, workers=1,
                invalidation_mode=None, *, stripdir=None,
                prependdir=None, limit_sl_dest=None, hardlink_dupes=False):
    """Byte-compile all modules in the given directory tree.

    dir:       the directory to compile
    maxlevels: how deep to descend into subdirectories (default: the
               interpreter's recursion limit)
    ddir:      directory name shown in tracebacks instead of `dir`
    force:     compile even if the cached bytecode looks up to date
    rx:        regular expression; files whose path it matches are skipped
    quiet:     0 lists files and errors, 1 only errors, 2 nothing
    legacy:    write foo.pyc next to foo.py instead of into __pycache__
    optimize:  optimization level, or a sequence of levels
    workers:   number of parallel worker processes (0 = one per CPU)
    invalidation_mode: a py_compile.PycInvalidationMode
    stripdir, prependdir: rewrite the recorded file names
    limit_sl_dest: skip symlinks whose target lies outside this directory
    hardlink_dupes: hard-link the files of optimization levels whose
               bytecode is identical
    """
    if workers < 0:
        raise ValueError('workers must be greater or equal to 0')
    if ddir is not None:
        if stripdir is not None or prependdir is not None:
            raise ValueError("Destination dir (ddir) cannot be used "
                             "in combination with stripdir or prependdir")
        stripdir = dir
        prependdir = ddir
        ddir = None
    _check_hardlink(optimize, hardlink_dupes)
    if maxlevels is None:
        maxlevels = sys.getrecursionlimit()
    files = list(_sources_in(dir, maxlevels, quiet))
    options = dict(ddir=ddir, force=force, rx=rx, quiet=quiet,
                   legacy=legacy, optimize=optimize,
                   invalidation_mode=invalidation_mode,
                   stripdir=stripdir, prependdir=prependdir,
                   limit_sl_dest=limit_sl_dest,
                   hardlink_dupes=hardlink_dupes)
    pool_class = None
    if workers != 1 and len(files) > 1:
        try:
            from concurrent.futures import ProcessPoolExecutor as pool_class
        except ImportError:
            pool_class = None
    if pool_class is not None:
        if workers == 0:
            workers = None
        import functools
        job = functools.partial(compile_file, **options)
        with pool_class(max_workers=workers) as pool:
            return min(pool.map(job, files), default=True)
    success = True
    for path in files:
        if not compile_file(path, **options):
            success = False
    return success


def _display_name(fullname, ddir, stripdir, prependdir, quiet):
    """The file name recorded in the code object (co_filename)."""
    if ddir is not None:
        name = os.path.join(os.fspath(ddir), os.path.basename(fullname))
    else:
        name = None
    if stripdir is not None:
        source_parts = os.path.normpath(fullname).split(os.sep)
        strip_parts = os.path.normpath(os.fspath(stripdir)).split(os.sep)
        if source_parts[:len(strip_parts)] == strip_parts:
            name = os.path.join(*source_parts[len(strip_parts):]) \
                if len(source_parts) > len(strip_parts) else ''
        else:
            if quiet < 2:
                print(f"The stripdir path {stripdir!r} is not a valid prefix "
                      f"for source path {fullname!r}; ignoring")
    if prependdir is not None:
        base = name if name is not None else fullname
        name = os.path.join(os.fspath(prependdir), base)
    return name


def _cache_files(fullname, levels, legacy):
    out = {}
    for level in levels:
        if legacy:
            out[level] = fullname + 'c'
        elif level >= 0:
            opt = level if level >= 1 else ''
            out[level] = importlib.util.cache_from_source(fullname, optimization=opt)
        else:
            out[level] = importlib.util.cache_from_source(fullname)
    return out


def _is_current(fullname, cfiles):
    """True if every cache file carries the source's current timestamp."""
    try:
        mtime = int(os.stat(fullname).st_mtime)
    except OSError:
        return False
    expected = struct.pack('<4sLL', importlib.util.MAGIC_NUMBER, 0,
                           mtime & 0xFFFF_FFFF)
    for cfile in cfiles:
        try:
            with open(cfile, 'rb') as handle:
                if handle.read(12) != expected:
                    return False
        except OSError:
            return False
    return True


def _same_bytes(a, b):
    with open(a, "rb") as fa, open(b, "rb") as fb:
        return fa.read() == fb.read()


def _report(text):
    # The message may contain characters the console cannot encode.
    encoding = getattr(sys.stdout, 'encoding', None) or sys.getdefaultencoding()
    print(text.encode(encoding, 'backslashreplace').decode(encoding))


def compile_file(fullname, ddir=None, force=False, rx=None, quiet=0,
                 legacy=False, optimize=-1,
                 invalidation_mode=None, *, stripdir=None, prependdir=None,
                 limit_sl_dest=None, hardlink_dupes=False):
    """Byte-compile one file; the arguments are those of compile_dir().

    Returns True on success (including files that are skipped), False if
    compilation failed.
    """
    if ddir is not None and (stripdir is not None or prependdir is not None):
        raise ValueError("Destination dir (ddir) cannot be used "
                         "in combination with stripdir or prependdir")
    _check_hardlink(optimize, hardlink_dupes)
    fullname = os.fspath(fullname)
    if rx is not None and rx.search(fullname):
        return True
    if limit_sl_dest is not None and os.path.islink(fullname):
        target = os.path.realpath(fullname)
        limit = os.path.realpath(os.fspath(limit_sl_dest))
        if os.path.commonpath([target, limit]) != limit:
            return True
    if not (os.path.isfile(fullname) and fullname.endswith('.py')):
        return True

    dfile = _display_name(fullname, ddir, stripdir, prependdir, quiet)
    levels = [optimize] if isinstance(optimize, int) else list(optimize)
    cfiles = _cache_files(fullname, levels, legacy)
    if not force and _is_current(fullname, cfiles.values()):
        return True

    if not quiet:
        print(f'Compiling {fullname!r}...')
    success = True
    previous = None
    for level in levels:
        cfile = cfiles[level]
        try:
            ok = py_compile.compile(fullname, cfile, dfile, True,
                                    optimize=level,
                                    invalidation_mode=invalidation_mode)
        except py_compile.PyCompileError as err:
            success = False
            if quiet >= 2:
                return success
            if quiet:
                print(f'*** Error compiling {fullname!r}...')
            else:
                print('*** ', end='')
            _report(err.msg)
            continue
        except (SyntaxError, UnicodeError, OSError) as err:
            success = False
            if quiet >= 2:
                return success
            if quiet:
                print(f'*** Error compiling {fullname!r}...')
            else:
                print('*** ', end='')
            _report(f'{type(err).__name__}: {err}')
            continue
        if ok == 0:
            success = False
            continue
        if hardlink_dupes and previous is not None and \
                _same_bytes(previous, cfile):
            os.unlink(cfile)
            os.link(previous, cfile)
        previous = cfile
    return success


def compile_path(skip_curdir=1, maxlevels=0, force=False, quiet=0,
                 legacy=False, optimize=-1,
                 invalidation_mode=None):
    """Byte-compile every directory on sys.path (by default without
    descending into subdirectories and skipping the current directory)."""
    success = True
    for entry in sys.path:
        if (not entry or entry == os.curdir) and skip_curdir:
            if quiet < 2:
                print('Skipping current directory')
            continue
        if not compile_dir(entry, maxlevels, None, force, quiet=quiet,
                           legacy=legacy, optimize=optimize,
                           invalidation_mode=invalidation_mode):
            success = False
    return success


def _parse_args():
    import argparse

    parser = argparse.ArgumentParser(
        description='Utilities to support installing Python libraries.',
        color=True)
    parser.add_argument('-l', action='store_const', const=0,
                        default=None, dest='maxlevels',
                        help="don't recurse into subdirectories")
    parser.add_argument('-r', type=int, dest='recursion',
                        help=('control the maximum recursion level. '
                              'if `-l` and `-r` options are specified, '
                              'then `-r` takes precedence.'))
    parser.add_argument('-f', action='store_true', dest='force',
                        help='force rebuild even if timestamps are up to date')
    parser.add_argument('-q', action='count', dest='quiet', default=0,
                        help='output only error messages; -qq will suppress '
                             'the error messages as well.')
    parser.add_argument('-b', action='store_true', dest='legacy',
                        help='use legacy (pre-PEP3147) compiled file locations')
    parser.add_argument('-d', metavar='DESTDIR', dest='ddir', default=None,
                        help=('directory to prepend to file paths for use in '
                              'compile-time tracebacks and in runtime '
                              'tracebacks in cases where the source file is '
                              'unavailable'))
    parser.add_argument('-s', metavar='STRIPDIR', dest='stripdir',
                        default=None,
                        help=('part of path to left-strip from path '
                              'to source file - for example buildroot. '
                              '`-d` and `-s` options cannot be '
                              'specified together.'))
    parser.add_argument('-p', metavar='PREPENDDIR', dest='prependdir',
                        default=None,
                        help=('path to add as prefix to path '
                              'to source file - for example / to make '
                              'it absolute when some part is removed '
                              'by `-s` option. '
                              '`-d` and `-p` options cannot be '
                              'specified together.'))
    parser.add_argument('-x', metavar='REGEXP', dest='rx', default=None,
                        help=('skip files matching the regular expression; '
                              'the regexp is searched for in the full path '
                              'of each file considered for compilation'))
    parser.add_argument('-i', metavar='FILE', dest='flist',
                        help=('add all the files and directories listed in '
                              'FILE to the list considered for compilation; '
                              'if "-", names are read from stdin'))
    parser.add_argument('compile_dest', metavar='FILE|DIR', nargs='*',
                        help=('zero or more file and directory names '
                              'to compile; if no arguments given, defaults '
                              'to the equivalent of -l sys.path'))
    parser.add_argument('-j', '--workers', default=1,
                        type=int, help='Run compileall concurrently')
    modes = [mode.name.lower().replace('_', '-')
             for mode in py_compile.PycInvalidationMode]
    parser.add_argument('--invalidation-mode', choices=sorted(modes),
                        help=('set .pyc invalidation mode; defaults to '
                              '"checked-hash" if the SOURCE_DATE_EPOCH '
                              'environment variable is set, and '
                              '"timestamp" otherwise.'))
    parser.add_argument('-o', action='append', type=int, dest='opt_levels',
                        help=('Optimization levels to run compilation with. '
                              'Default is -1 which uses the optimization level '
                              'of the Python interpreter itself (see -O).'))
    parser.add_argument('-e', metavar='DIR', dest='limit_sl_dest',
                        help='Ignore symlinks pointing outsite of the DIR')
    parser.add_argument('--hardlink-dupes', action='store_true',
                        dest='hardlink_dupes',
                        help='Hardlink duplicated pyc files')

    args = parser.parse_args()
    if args.ddir is not None and (args.stripdir is not None or
                                  args.prependdir is not None):
        parser.error("-d cannot be used in combination with -s or -p")
    levels = args.opt_levels or [-1]
    if args.hardlink_dupes and (len(levels) < 2 or len(set(levels)) != len(levels)):
        parser.error("Hardlinking of duplicated bytecode makes sense "
                     "only for more than one optimization level.")
    return args


def main():
    """Script entry point; returns True on success."""
    args = _parse_args()
    if args.recursion is not None:
        maxlevels = args.recursion
    elif args.maxlevels is not None:
        maxlevels = args.maxlevels
    else:
        maxlevels = sys.getrecursionlimit()
    rx = None
    if args.rx:
        import re
        rx = re.compile(args.rx)
    if args.limit_sl_dest == "":
        args.limit_sl_dest = None
    optimize = args.opt_levels or -1
    if isinstance(optimize, list) and len(optimize) == 1:
        optimize = optimize[0]

    targets = list(args.compile_dest)
    if args.flist:
        try:
            if args.flist == '-':
                stream = sys.stdin
                targets.extend(line.strip() for line in stream)
            else:
                with open(args.flist, encoding="utf-8") as stream:
                    targets.extend(line.strip() for line in stream)
        except OSError:
            if args.quiet < 2:
                print(f"Error reading file list {args.flist}")
            return False

    if args.invalidation_mode:
        mode_name = args.invalidation_mode.replace('-', '_').upper()
        invalidation_mode = py_compile.PycInvalidationMode[mode_name]
    else:
        invalidation_mode = None

    options = dict(quiet=args.quiet, legacy=args.legacy,
                   optimize=optimize, invalidation_mode=invalidation_mode)
    success = True
    try:
        if not targets:
            return compile_path(legacy=args.legacy, force=args.force,
                                quiet=args.quiet,
                                invalidation_mode=invalidation_mode)
        for dest in targets:
            if os.path.isfile(dest):
                ok = compile_file(dest, args.ddir, args.force, rx,
                                  stripdir=args.stripdir,
                                  prependdir=args.prependdir,
                                  limit_sl_dest=args.limit_sl_dest,
                                  hardlink_dupes=args.hardlink_dupes,
                                  **options)
            else:
                ok = compile_dir(dest, maxlevels, args.ddir, args.force, rx,
                                 workers=args.workers,
                                 stripdir=args.stripdir,
                                 prependdir=args.prependdir,
                                 limit_sl_dest=args.limit_sl_dest,
                                 hardlink_dupes=args.hardlink_dupes,
                                 **options)
            if not ok:
                success = False
        return success
    except KeyboardInterrupt:
        if args.quiet < 2:
            print("\n[interrupted]")
        return False


if __name__ == '__main__':
    sys.exit(int(not main()))
