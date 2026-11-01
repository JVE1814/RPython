"""Path helpers shared by the platform path modules (posixpath, ntpath).

Not meant to be imported directly."""
import os
import stat

__all__ = ['commonprefix', 'exists', 'getatime', 'getctime', 'getmtime',
           'getsize', 'isdevdrive', 'isdir', 'isfile', 'isjunction', 'islink',
           'lexists', 'samefile', 'sameopenfile', 'samestat', 'ALLOW_MISSING']


def exists(path):
    """Return True if path names an existing file system object (False for a dangling symlink)."""
    try:
        os.stat(path)
    except (OSError, ValueError):
        return False
    return True


def lexists(path):
    """Return True if path exists, without following a final symbolic link."""
    try:
        os.lstat(path)
    except (OSError, ValueError):
        return False
    return True


def isfile(path):
    """Return True if path names an existing regular file."""
    try:
        st = os.stat(path)
    except (OSError, ValueError):
        return False
    return stat.S_ISREG(st.st_mode)


def isdir(s):
    """Return True if path names an existing directory."""
    try:
        st = os.stat(s)
    except (OSError, ValueError):
        return False
    return stat.S_ISDIR(st.st_mode)


def islink(path):
    """Return True if path names a symbolic link."""
    try:
        st = os.lstat(path)
    except (OSError, ValueError, AttributeError):
        return False
    return stat.S_ISLNK(st.st_mode)


def isjunction(path):
    """Return False: junctions do not exist on this platform."""
    os.fspath(path)
    return False


def isdevdrive(path):
    """Return False: Dev Drives do not exist on this platform."""
    os.fspath(path)
    return False


def getsize(filename):
    """Return st_size of os.stat(filename)."""
    return os.stat(filename).st_size


def getmtime(filename):
    """Return st_mtime of os.stat(filename)."""
    return os.stat(filename).st_mtime


def getatime(filename):
    """Return st_atime of os.stat(filename)."""
    return os.stat(filename).st_atime


def getctime(filename):
    """Return st_ctime of os.stat(filename)."""
    return os.stat(filename).st_ctime


def commonprefix(m, /):
    """Return the longest string that is a prefix of every item of m (character-wise)."""
    import warnings
    warnings.warn('os.path.commonprefix() is deprecated. Use '
                  'os.path.commonpath() for longest path prefix.',
                  category=DeprecationWarning,
                  stacklevel=2)
    return _commonprefix(m)


def _commonprefix(m, /):
    if not m:
        return ''
    if not isinstance(m[0], (list, tuple)):
        m = tuple(map(os.fspath, m))
    lo = min(m)
    hi = max(m)
    n = min(len(lo), len(hi))
    i = 0
    while i < n and lo[i] == hi[i]:
        i += 1
    return lo[:i]


def samestat(s1, s2):
    """Return True if two stat results describe the same file."""
    return (s1.st_ino == s2.st_ino and
            s1.st_dev == s2.st_dev)


def samefile(f1, f2):
    """Return True if both paths refer to the same file (same device and inode)."""
    return samestat(os.stat(f1), os.stat(f2))


def sameopenfile(fp1, fp2):
    """Return True if two file descriptors refer to the same file."""
    return samestat(os.fstat(fp1), os.fstat(fp2))


def _splitext(p, sep, altsep, extsep):
    """Split p into (root, ext) where ext starts at the last dot of the final
    component (leading dots of the component do not count); ext may be empty."""
    last_sep = p.rfind(sep)
    if altsep:
        last_sep = max(last_sep, p.rfind(altsep))
    dot = p.rfind(extsep)
    if dot > last_sep:
        # the name must contain something other than dots before the dot
        start = last_sep + 1
        while start < dot:
            if p[start:start+1] != extsep:
                return p[:dot], p[dot:]
            start += 1
    return p, p[:0]


def _check_arg_types(funcname, *args):
    has_str = has_bytes = False
    for s in args:
        if isinstance(s, str):
            has_str = True
        elif isinstance(s, bytes):
            has_bytes = True
        else:
            raise TypeError(f'{funcname}() argument must be str, bytes, or '
                            f'os.PathLike object, not {s.__class__.__name__!r}') from None
    if has_str and has_bytes:
        raise TypeError("Can't mix strings and bytes in path components") from None


class _AllowMissing:
    __slots__ = ()

    def __repr__(self):
        return 'os.path.ALLOW_MISSING'

    def __reduce__(self):
        return self.__class__.__name__

ALLOW_MISSING = _AllowMissing()
