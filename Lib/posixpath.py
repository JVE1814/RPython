"""Pathname manipulation for POSIX systems.

Usually accessed as os.path rather than imported directly."""

# Strings representing various path-related bits and pieces.
curdir = '.'
pardir = '..'
extsep = '.'
sep = '/'
pathsep = ':'
defpath = '/bin:/usr/bin'
altsep = None
devnull = '/dev/null'

import errno
import os
import sys
import stat
import genericpath
from genericpath import *

__all__ = ["normcase", "isabs", "join", "splitdrive", "splitroot", "split", "splitext",
           "basename", "dirname", "commonprefix", "getsize", "getmtime",
           "getatime", "getctime", "islink", "exists", "lexists", "isdir", "isfile",
           "ismount", "expanduser", "expandvars", "normpath", "abspath",
           "samefile", "sameopenfile", "samestat",
           "curdir", "pardir", "sep", "pathsep", "defpath", "altsep", "extsep",
           "devnull", "realpath", "supports_unicode_filenames", "relpath",
           "commonpath", "isjunction", "isdevdrive",
           "ALL_BUT_LAST", "ALLOW_MISSING"]


def _get_sep(path):
    return b'/' if isinstance(path, bytes) else '/'


def normcase(s, /):
    """Return the path unchanged (POSIX paths are case sensitive)."""
    return os.fspath(s)


def isabs(s, /):
    """Return True if the path starts with a slash."""
    s = os.fspath(s)
    return s.startswith(_get_sep(s))


def join(a, /, *p):
    """Join path components with slashes.  An absolute component discards
    everything before it; an empty final component yields a trailing slash."""
    a = os.fspath(a)
    sep = _get_sep(a)
    path = a
    try:
        for b in p:
            b = os.fspath(b)
            if b.startswith(sep) or not path:
                path = b
            elif path.endswith(sep):
                path += b
            else:
                path += sep + b
    except (TypeError, AttributeError, BytesWarning):
        genericpath._check_arg_types('join', a, *p)
        raise
    return path


def split(p, /):
    """Return (head, tail): tail is the part after the last slash."""
    p = os.fspath(p)
    sep = _get_sep(p)
    i = p.rfind(sep) + 1
    head, tail = p[:i], p[i:]
    if head and head != sep * len(head):
        head = head.rstrip(sep)
    return head, tail


def splitext(p, /):
    p = os.fspath(p)
    if isinstance(p, bytes):
        return genericpath._splitext(p, b'/', None, b'.')
    return genericpath._splitext(p, '/', None, '.')
splitext.__doc__ = genericpath._splitext.__doc__


def splitdrive(p, /):
    """Return (drive, path); the drive is always empty on POSIX."""
    p = os.fspath(p)
    return p[:0], p


try:
    from posix import _path_splitroot_ex as splitroot
except ImportError:
    def splitroot(p, /):
        """Return (drive, root, tail) for p."""
        p = os.fspath(p)
        if isinstance(p, bytes):
            sep = b'/'
            empty = b''
        else:
            sep = '/'
            empty = ''
        if p[:1] != sep:
            return empty, empty, p
        if p[1:2] != sep or p[2:3] == sep:
            return empty, sep, p[1:]
        return empty, p[:2], p[2:]


def basename(p, /):
    """Return the part of the path after the last slash."""
    p = os.fspath(p)
    sep = _get_sep(p)
    return p[p.rfind(sep) + 1:]


def dirname(p, /):
    """Return the part of the path before the last slash."""
    p = os.fspath(p)
    sep = _get_sep(p)
    i = p.rfind(sep) + 1
    head = p[:i]
    if head and head != sep * len(head):
        head = head.rstrip(sep)
    return head


def ismount(path):
    """Return True if path is a mount point."""
    try:
        s1 = os.lstat(path)
    except (OSError, ValueError):
        return False
    if stat.S_ISLNK(s1.st_mode):
        return False
    path = os.fspath(path)
    if isinstance(path, bytes):
        parent = join(path, b'..')
    else:
        parent = join(path, '..')
    try:
        s2 = os.lstat(parent)
    except OSError:
        parent = realpath(parent)
        try:
            s2 = os.lstat(parent)
        except OSError:
            return False
    # a different device, or the same i-node as the parent: a mount point
    return s1.st_dev != s2.st_dev or s1.st_ino == s2.st_ino


def expanduser(path):
    """Replace a leading ~ or ~user by the home directory, if it is known."""
    path = os.fspath(path)
    if isinstance(path, bytes):
        tilde = b'~'
        sep = b'/'
    else:
        tilde = '~'
        sep = '/'
    if not path.startswith(tilde):
        return path
    i = path.find(sep, 1)
    if i < 0:
        i = len(path)
    if i == 1:
        if 'HOME' in os.environ:
            userhome = os.environ['HOME']
        else:
            try:
                import pwd
            except ImportError:
                return path
            try:
                userhome = pwd.getpwuid(os.getuid()).pw_dir
            except KeyError:
                return path
    else:
        try:
            import pwd
        except ImportError:
            return path
        name = path[1:i]
        if isinstance(name, bytes):
            name = os.fsdecode(name)
        try:
            entry = pwd.getpwnam(name)
        except KeyError:
            return path
        userhome = entry.pw_dir
    if userhome is None and sys.platform in ("emscripten", "wasi"):
        return path
    if isinstance(path, bytes):
        userhome = os.fsencode(userhome)
    userhome = userhome.rstrip(sep)
    return (userhome + path[i:]) or sep


_varpattern = None
_varpatternb = None

def _var_regex(is_bytes):
    global _varpattern, _varpatternb
    import re
    if is_bytes:
        if _varpatternb is None:
            _varpatternb = re.compile(rb'\$(\w+|\{[^}]*\}?)', re.ASCII)
        return _varpatternb
    if _varpattern is None:
        _varpattern = re.compile(r'\$(\w+|\{[^}]*\}?)', re.ASCII)
    return _varpattern


def expandvars(path):
    """Substitute $name and ${name} by environment variables; unknown names stay."""
    path = os.fspath(path)
    if isinstance(path, bytes):
        if b'$' not in path:
            return path
        regex = _var_regex(True)
        lbrace, rbrace = b'{', b'}'
        environ = getattr(os, 'environb', None)
    else:
        if '$' not in path:
            return path
        regex = _var_regex(False)
        lbrace, rbrace = '{', '}'
        environ = os.environ

    def replace(m):
        name = m[1]
        if name.startswith(lbrace):
            if not name.endswith(rbrace):
                return m[0]
            name = name[1:-1]
        try:
            if environ is None:
                return os.fsencode(os.environ[os.fsdecode(name)])
            return environ[name]
        except KeyError:
            return m[0]

    return regex.sub(replace, path)


try:
    from posix import _path_normpath as normpath
except ImportError:
    def normpath(path):
        """Collapse redundant separators and . / .. components."""
        path = os.fspath(path)
        if isinstance(path, bytes):
            sep, empty, dot, dotdot = b'/', b'', b'.', b'..'
        else:
            sep, empty, dot, dotdot = '/', '', '.', '..'
        if path == empty:
            return dot
        _, root, rest = splitroot(path)
        parts = []
        for comp in rest.split(sep):
            if not comp or comp == dot:
                continue
            if comp == dotdot:
                if parts and parts[-1] != dotdot:
                    parts.pop()
                    continue
                if root:
                    continue
            parts.append(comp)
        return (root + sep.join(parts)) or dot


def abspath(path):
    """Return a normalized absolute version of path."""
    path = os.fspath(path)
    if isinstance(path, bytes):
        if not path.startswith(b'/'):
            path = join(os.getcwdb(), path)
    else:
        if not path.startswith('/'):
            path = join(os.getcwd(), path)
    return normpath(path)



def realpath(filename, /, *, strict=False):
    """Return the canonical path of filename with all symbolic links resolved."""
    filename = os.fspath(filename)
    if isinstance(filename, bytes):
        sep, curdir, pardir = b'/', b'.', b'..'
        getcwd = os.getcwdb
    else:
        sep, curdir, pardir = '/', '.', '..'
        getcwd = os.getcwd
    if strict is ALLOW_MISSING:
        ignore = FileNotFoundError
        last_only = False
        strict = True
    elif strict is ALL_BUT_LAST:
        ignore = FileNotFoundError
        last_only = True
        strict = True
    elif strict:
        ignore = ()
        last_only = False
        strict = True
    else:
        ignore = OSError
        last_only = False
        strict = False

    lstat = os.lstat
    readlink = os.readlink

    # Components still to be resolved, in reverse order.  A 1-tuple holding a
    # link path marks the end of that link's target.
    todo = filename.split(sep)
    todo.reverse()
    if filename.startswith(sep):
        path = sep
    else:
        path = getcwd()
    # resolved links: link path -> resolved path (None while being resolved)
    links = {}
    # True when `path` is known to name something that is not a directory
    not_dir = False

    def more_components():
        for c in todo:
            if c.__class__ is tuple:
                continue
            if c and c != curdir:
                return True
        return False

    while todo:
        name = todo.pop()
        if name.__class__ is tuple:
            links[name[0]] = path
            continue
        if strict and not_dir:
            raise NotADirectoryError(errno.ENOTDIR, os.strerror(errno.ENOTDIR), path)
        if not name or name == curdir:
            continue
        if name == pardir:
            i = path.rfind(sep)
            path = path[:i] or sep
            not_dir = False
            continue
        if path == sep:
            newpath = path + name
        else:
            newpath = path + sep + name
        try:
            st_mode = lstat(newpath).st_mode
        except ignore as e:
            if last_only and more_components():
                raise
            path = newpath
            not_dir = False
            continue
        if not stat.S_ISLNK(st_mode):
            path = newpath
            not_dir = not stat.S_ISDIR(st_mode)
            continue
        if newpath in links:
            resolved = links[newpath]
            if resolved is not None:
                path = resolved
                not_dir = False
                if strict:
                    try:
                        not_dir = not stat.S_ISDIR(os.stat(path).st_mode)
                    except OSError:
                        pass
                continue
            # symlink loop
            if strict:
                raise OSError(errno.ELOOP, os.strerror(errno.ELOOP), newpath)
            path = newpath
            not_dir = False
            continue
        try:
            target = readlink(newpath)
        except ignore:
            if last_only and more_components():
                raise
            path = newpath
            not_dir = False
            continue
        links[newpath] = None
        todo.append((newpath,))
        if target.startswith(sep):
            path = sep
        parts = target.split(sep)
        parts.reverse()
        todo.extend(parts)
        not_dir = False
    return path


supports_unicode_filenames = (sys.platform == 'darwin')


def relpath(path, start=None):
    """Return path expressed relative to start (default: the current directory)."""
    path = os.fspath(path)
    if not path:
        raise ValueError("no path specified")
    if isinstance(path, bytes):
        curdir, sep, pardir = b'.', b'/', b'..'
    else:
        curdir, sep, pardir = '.', '/', '..'
    if start is None:
        start = curdir
    else:
        start = os.fspath(start)
    try:
        start_parts = [x for x in abspath(start).split(sep) if x]
        path_parts = [x for x in abspath(path).split(sep) if x]
        n = 0
        for a, b in zip(start_parts, path_parts):
            if a != b:
                break
            n += 1
        rel = [pardir] * (len(start_parts) - n) + path_parts[n:]
        if not rel:
            return curdir
        return sep.join(rel)
    except (TypeError, AttributeError, BytesWarning, DeprecationWarning):
        genericpath._check_arg_types('relpath', path, start)
        raise


def commonpath(paths):
    """Return the longest sub-path shared by every path in paths."""
    paths = tuple(map(os.fspath, paths))
    if not paths:
        raise ValueError('commonpath() arg is an empty iterable')
    if isinstance(paths[0], bytes):
        sep, curdir = b'/', b'.'
    else:
        sep, curdir = '/', '.'
    try:
        split_paths = [p.split(sep) for p in paths]
        try:
            isabs, = {p.startswith(sep) for p in paths}
        except ValueError:
            raise ValueError("Can't mix absolute and relative paths") from None
        split_paths = [[c for c in s if c and c != curdir] for s in split_paths]
        lo = min(split_paths)
        hi = max(split_paths)
        common = lo
        for i, c in enumerate(lo):
            if c != hi[i]:
                common = lo[:i]
                break
        prefix = sep if isabs else sep[:0]
        return prefix + sep.join(common)
    except (TypeError, AttributeError):
        genericpath._check_arg_types('commonpath', *paths)
        raise
