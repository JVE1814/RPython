"""Path functions that are the same on every platform.

posixpath and ntpath re-export these; use them through os.path.
"""
import os
import stat

__all__ = ['commonprefix', 'exists', 'getatime', 'getctime', 'getmtime',
           'getsize', 'isdevdrive', 'isdir', 'isfile', 'isjunction', 'islink',
           'lexists', 'samefile', 'sameopenfile', 'samestat', 'ALLOW_MISSING',
           'ALL_BUT_LAST']


# The predicates answer False instead of raising for paths the OS rejects
# (missing files, permission problems, embedded NULs, unencodable names).

def exists(path):
    """Return True if path names an existing file or directory (broken
    symbolic links count as missing)."""
    try:
        os.stat(path)
    except (OSError, ValueError):
        return False
    return True


def lexists(path):
    """Return True if path exists; a broken symbolic link counts."""
    try:
        os.lstat(path)
    except (OSError, ValueError):
        return False
    return True


def _mode_is(path, check, follow=True):
    try:
        info = os.stat(path) if follow else os.lstat(path)
    except (OSError, ValueError):
        return False
    return check(info.st_mode)


def isfile(path):
    """Return True if path is an existing regular file (following links)."""
    return _mode_is(path, stat.S_ISREG)


def isdir(s):
    """Return True if s is an existing directory (following links)."""
    return _mode_is(s, stat.S_ISDIR)


def islink(path):
    """Return True if path is a symbolic link."""
    return _mode_is(path, stat.S_ISLNK, follow=False)


def isjunction(path):
    """Return True if path is a junction; junctions only exist on
    Windows, so this is always False here."""
    os.fspath(path)
    return False


def isdevdrive(path):
    """Return True if path lives on a Windows Dev Drive; always False
    on other systems."""
    os.fspath(path)
    return False


def getsize(filename):
    """Return the size of a file in bytes, as reported by os.stat()."""
    return os.stat(filename).st_size


def getmtime(filename):
    """Return the time of last modification, as reported by os.stat()."""
    return os.stat(filename).st_mtime


def getatime(filename):
    """Return the time of last access, as reported by os.stat()."""
    return os.stat(filename).st_atime


def getctime(filename):
    """Return the metadata change time (Unix) or creation time (Windows),
    as reported by os.stat()."""
    return os.stat(filename).st_ctime


def commonprefix(m, /):
    """Return the longest common leading part of the items of m
    (deprecated: it compares item by item, characters for strings, so the
    result need not be a valid path; use commonpath() for paths)."""
    import warnings
    warnings.warn('os.path.commonprefix() is deprecated; use '
                  'os.path.commonpath() to find a common path',
                  DeprecationWarning, stacklevel=2)
    return _common_start(m)


def _common_start(items):
    # Longest common prefix of a list of sequences (strings, lists, ...).
    if not items:
        return ''
    if not isinstance(items[0], (list, tuple)):
        items = [os.fspath(item) for item in items]
    shortest = min(items, key=len)
    for index, column in enumerate(zip(*items)):
        first = column[0]
        for other in column:
            if other != first:
                return shortest[:index]
    return shortest


def samestat(s1, s2):
    """Return True if two os.stat() results describe the same file."""
    return s1.st_ino == s2.st_ino and s1.st_dev == s2.st_dev


def samefile(f1, f2):
    """Return True if two paths name the same file or directory (compared
    by device and inode number)."""
    return samestat(os.stat(f1), os.stat(f2))


def sameopenfile(fp1, fp2):
    """Return True if two open file descriptors refer to the same file."""
    return samestat(os.fstat(fp1), os.fstat(fp2))


def _split_extension(path, sep, altsep, extsep):
    """splitext() for the platform modules: the extension is everything
    from the last extsep of the last component on, unless that component
    consists of leading extseps only before it (as in '.profile')."""
    cut = path.rfind(sep)
    if altsep:
        cut = max(cut, path.rfind(altsep))
    dot = path.rfind(extsep)
    if dot > cut:
        for index in range(cut + 1, dot):
            if path[index:index + 1] != extsep:
                return path[:dot], path[dot:]
    return path, path[:0]


def _reject_mixed_types(funcname, *args):
    """Raise a helpful TypeError when the arguments of a path function are
    not all str or all bytes (after os.fspath()); return quietly if they
    are fine, so the caller can re-raise its original error."""
    seen_str = seen_bytes = False
    for arg in args:
        if isinstance(arg, os.PathLike):
            arg = os.fspath(arg)
        if isinstance(arg, str):
            seen_str = True
        elif isinstance(arg, bytes):
            seen_bytes = True
        else:
            raise TypeError(f'{funcname}() argument must be str, bytes, or '
                            f'os.PathLike object, not '
                            f'{type(arg).__name__!r}') from None
    if seen_str and seen_bytes:
        raise TypeError("Can't mix strings and bytes in path components") from None


class _StrictMode:
    """A named value for realpath()'s strict argument."""

    __slots__ = ('_name',)

    def __init__(self, name):
        self._name = name

    def __repr__(self):
        return 'os.path.' + self._name

    __str__ = __repr__

    def __reduce__(self):
        # pickled (and copied) by name: the module attribute is the object
        return self._name


# realpath(strict=ALLOW_MISSING): missing components are fine, any other
# error is raised; realpath(strict=ALL_BUT_LAST): only the last component
# may be missing.
ALLOW_MISSING = _StrictMode('ALLOW_MISSING')
ALL_BUT_LAST = _StrictMode('ALL_BUT_LAST')
