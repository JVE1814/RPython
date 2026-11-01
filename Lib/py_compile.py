"""Byte-compile Python source files into cached bytecode (.pyc) files.

The cached file holds the 16-byte header used by importlib (magic number,
flags, then either the source mtime/size or a source hash) followed by the
marshalled code object.  Our code objects carry this implementation's own
bytecode, so the files are only meaningful to this interpreter; the magic
number in importlib keeps them apart from other interpreters' caches.
"""

import enum
import importlib._bootstrap_external
import importlib.machinery
import importlib.util
import os
import os.path
import sys
import traceback

__all__ = ["compile", "main", "PyCompileError", "PycInvalidationMode"]


class PyCompileError(Exception):
    """Raised by compile() (with doraise=True) when a file cannot be compiled.

    Attributes: exc_type_name (name of the underlying exception type),
    exc_value (the exception), file (the source file name) and msg (the
    text that str() of this exception returns).
    """

    def __init__(self, exc_type, exc_value, file, msg=''):
        self.exc_type_name = exc_type.__name__
        self.exc_value = exc_value
        self.file = file
        if not msg:
            msg = self._describe(exc_type, exc_value, file)
        self.msg = msg
        Exception.__init__(self, msg, self.exc_type_name, exc_value, file)

    @staticmethod
    def _describe(exc_type, exc_value, file):
        if issubclass(exc_type, SyntaxError):
            lines = traceback.format_exception_only(exc_type, exc_value)
            text = ''.join(lines)
            # report the user-visible file name rather than the compile() label
            return text.replace('File "<string>"', f'File "{file}"')
        return f"Sorry: {exc_type.__name__}: {exc_value}"

    def __str__(self):
        return self.msg


class PycInvalidationMode(enum.Enum):
    TIMESTAMP = 1
    CHECKED_HASH = 2
    UNCHECKED_HASH = 3


def _get_default_invalidation_mode():
    # Reproducible builds: a set SOURCE_DATE_EPOCH selects hash-based pycs.
    if os.environ.get('SOURCE_DATE_EPOCH'):
        return PycInvalidationMode.CHECKED_HASH
    return PycInvalidationMode.TIMESTAMP


def _target_path(file, cfile, optimize):
    if cfile is not None:
        return cfile
    if optimize >= 0:
        level = optimize if optimize >= 1 else ''
        return importlib.util.cache_from_source(file, optimization=level)
    return importlib.util.cache_from_source(file)


def _refuse_special_target(cfile):
    # Never replace a symlink or a device/FIFO with bytecode.
    if os.path.islink(cfile):
        raise FileExistsError(f'{cfile} is a symlink and will be changed into '
                              'a regular file if import writes a byte-compiled '
                              'file to it')
    if os.path.exists(cfile) and not os.path.isfile(cfile):
        raise FileExistsError(f'{cfile} is a non-regular file and will be '
                              'changed into a regular one if import writes a '
                              'byte-compiled file to it')


def compile(file, cfile=None, dfile=None, doraise=False, optimize=-1,
            invalidation_mode=None, quiet=0):
    """Byte-compile one Python source file to a .pyc file.

    file:     source file name
    cfile:    target file name (default: the PEP 3147/488 cache path)
    dfile:    file name shown in tracebacks (default: file)
    doraise:  raise PyCompileError on compilation errors instead of
              writing a message to sys.stderr
    optimize: optimization level (-1 means the interpreter's level)
    invalidation_mode: a PycInvalidationMode (default: TIMESTAMP, or
              CHECKED_HASH when SOURCE_DATE_EPOCH is set)
    quiet:    0 reports errors, 1 suppresses messages when not raising,
              2 suppresses everything

    Returns the path of the written file, or None on error (when not
    raising).
    """
    if invalidation_mode is None:
        invalidation_mode = _get_default_invalidation_mode()
    cfile = _target_path(file, cfile, optimize)
    _refuse_special_target(cfile)

    loader = importlib.machinery.SourceFileLoader('<py_compile>', file)
    source_bytes = loader.get_data(file)
    try:
        code = loader.source_to_code(source_bytes, dfile or file,
                                     _optimize=optimize)
    except Exception as err:
        problem = PyCompileError(type(err), err, dfile or file)
        if quiet < 2:
            if doraise:
                raise problem
            sys.stderr.write(problem.msg + '\n')
        return None

    directory = os.path.dirname(cfile)
    if directory:
        try:
            os.makedirs(directory)
        except FileExistsError:
            pass

    ext = importlib._bootstrap_external
    if invalidation_mode == PycInvalidationMode.TIMESTAMP:
        st = loader.path_stats(file)
        payload = ext._code_to_timestamp_pyc(code, st['mtime'], st['size'])
    else:
        digest = importlib.util.source_hash(source_bytes)
        payload = ext._code_to_hash_pyc(
            code, digest,
            invalidation_mode == PycInvalidationMode.CHECKED_HASH)
    mode = ext._calc_mode(file)
    ext._write_atomic(cfile, payload, mode)
    return cfile


def main():
    """Command line entry point: compile the named files (or the file names
    read from standard input when the only argument is '-')."""
    import argparse

    parser = argparse.ArgumentParser(
        description='A simple command-line interface for py_compile module.',
        color=True)
    parser.add_argument('-q', '--quiet', action='store_true',
                        help='Suppress error output')
    parser.add_argument('filenames', nargs='+',
                        help='Files to compile')
    args = parser.parse_args()
    if args.filenames == ['-']:
        names = [line.rstrip('\n') for line in sys.stdin.readlines()]
    else:
        names = args.filenames
    status = 0
    for name in names:
        try:
            compile(name, doraise=True)
        except PyCompileError as error:
            status = 1
            if not args.quiet:
                sys.stderr.write(error.msg)
        except OSError as error:
            status = 1
            if not args.quiet:
                sys.stderr.write(f"{error}\n")
    if status:
        sys.exit(status)


if __name__ == "__main__":
    main()
