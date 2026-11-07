"""Path name operations for POSIX systems.

Normally used as os.path.  str and bytes paths are both accepted (and
anything with __fspath__); a function returns the type it was given.
"""

# The POSIX spellings of the special names.
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
from genericpath import (
    commonprefix, exists, getatime, getctime, getmtime, getsize, isdevdrive,
    isdir, isfile, isjunction, islink, lexists, samefile, sameopenfile,
    samestat, ALLOW_MISSING, ALL_BUT_LAST)

__all__ = ["normcase", "isabs", "join", "splitdrive", "splitroot", "split",
           "splitext", "basename", "dirname", "commonprefix", "getsize",
           "getmtime", "getatime", "getctime", "islink", "exists", "lexists",
           "isdir", "isfile", "ismount", "expanduser", "expandvars",
           "normpath", "abspath", "samefile", "sameopenfile", "samestat",
           "curdir", "pardir", "sep", "pathsep", "defpath", "altsep",
           "extsep", "devnull", "realpath", "supports_unicode_filenames",
           "relpath", "commonpath", "isjunction", "isdevdrive",
           "ALL_BUT_LAST", "ALLOW_MISSING"]

_reject_mixed_types = genericpath._reject_mixed_types


def _slash(path):
    return b'/' if isinstance(path, bytes) else '/'


def normcase(s, /):
    """Return s unchanged: POSIX file names are case sensitive."""
    return os.fspath(s)


def isabs(s, /):
    """Return True if s is an absolute path (starts with a slash)."""
    s = os.fspath(s)
    return s.startswith(_slash(s))


def join(a, /, *p):
    """Join path components with slashes.  A component that starts with a
    slash discards everything before it; a slash is added between parts
    only where needed (so a last empty part leaves a trailing slash)."""
    a = os.fspath(a)
    slash = b'/' if isinstance(a, bytes) else '/'
    path = a
    try:
        for part in p:
            part = os.fspath(part)
            if part.startswith(slash) or not path:
                path = part
            elif path.endswith(slash):
                path += part
            else:
                path += slash + part
    except (TypeError, AttributeError, BytesWarning):
        _reject_mixed_types('join', a, *p)
        raise
    return path


def split(p, /):
    """Split p into (head, tail): tail is what follows the last slash,
    head the rest without trailing slashes (unless it is all slashes)."""
    p = os.fspath(p)
    slash = _slash(p)
    cut = p.rfind(slash) + 1
    head, tail = p[:cut], p[cut:]
    trimmed = head.rstrip(slash)
    return (trimmed or head), tail


def splitext(p, /):
    """Split p into (root, ext) where ext is the extension of the last
    component ('' if none; leading dots do not start an extension)."""
    p = os.fspath(p)
    if isinstance(p, bytes):
        return genericpath._split_extension(p, b'/', None, b'.')
    return genericpath._split_extension(p, '/', None, '.')


def splitdrive(p, /):
    """Split p into (drive, path); POSIX has no drives, so the drive is
    always empty."""
    p = os.fspath(p)
    return p[:0], p


def _splitroot(p, /):
    """Split p into (drive, root, tail): the drive is always empty, the
    root the leading slash, or both slashes when there are exactly two."""
    p = os.fspath(p)
    slash = _slash(p)
    empty = p[:0]
    if p[:1] != slash:
        return empty, empty, p
    if p[1:2] != slash or p[2:3] == slash:
        return empty, slash, p[1:]
    return empty, p[:2], p[2:]


try:
    from posix import _path_splitroot_ex as splitroot
except ImportError:
    splitroot = _splitroot


def basename(p, /):
    """Return the last component of p (empty if p ends with a slash)."""
    p = os.fspath(p)
    return p[p.rfind(_slash(p)) + 1:]


def dirname(p, /):
    """Return everything of p before its last component, without trailing
    slashes (unless it consists of slashes only)."""
    p = os.fspath(p)
    slash = _slash(p)
    head = p[:p.rfind(slash) + 1]
    return head.rstrip(slash) or head


def ismount(path):
    """Return True if path is a mount point: a directory (not a symbolic
    link) on another device than its parent, or the root itself."""
    try:
        own = os.lstat(path)
    except (OSError, ValueError):
        return False
    if stat.S_ISLNK(own.st_mode):
        return False
    path = os.fspath(path)
    parent = join(path, b'..' if isinstance(path, bytes) else '..')
    try:
        above = os.lstat(parent)
    except (OSError, ValueError):
        # the directory may be unreadable; ask about its parent by name
        try:
            above = os.lstat(realpath(parent))
        except (OSError, ValueError):
            return False
    return own.st_dev != above.st_dev or own.st_ino == above.st_ino


def expanduser(path):
    """Replace a leading ~ or ~user by that user's home directory.

    ~ alone uses $HOME, or the password database when it is unset.  The
    path is returned unchanged when it does not start with ~ or the user
    is unknown.
    """
    path = os.fspath(path)
    tilde, slash = (b'~', b'/') if isinstance(path, bytes) else ('~', '/')
    if not path.startswith(tilde):
        return path
    end = path.find(slash, 1)
    if end < 0:
        end = len(path)
    if end == 1:
        home = os.environ.get('HOME')
        if home is None:
            try:
                import pwd
                home = pwd.getpwuid(os.getuid()).pw_dir
            except (ImportError, KeyError):
                return path
    else:
        try:
            import pwd
        except ImportError:
            return path
        user = path[1:end]
        if isinstance(user, bytes):
            user = os.fsdecode(user)
        try:
            home = pwd.getpwnam(user).pw_dir
        except KeyError:
            return path
    if isinstance(path, bytes):
        home = os.fsencode(home)
    home = home.rstrip(slash)
    return (home + path[end:]) or slash


_var_finders = {}


def expandvars(path):
    """Replace $name and ${name} by the value of the environment variable
    name; references to unknown variables are left alone."""
    path = os.fspath(path)
    is_bytes = isinstance(path, bytes)
    if (b'$' if is_bytes else '$') not in path:
        return path
    finder = _var_finders.get(is_bytes)
    if finder is None:
        import re
        pattern = r'\$(\w+|\{[^}]*\})'
        if is_bytes:
            pattern = pattern.encode('ascii')
        finder = _var_finders[is_bytes] = re.compile(pattern, re.ASCII)
    if is_bytes:
        environ = getattr(os, 'environb', None)
        if environ is None:
            environ = {os.fsencode(k): os.fsencode(v)
                       for k, v in os.environ.items()}
        braces = (b'{', b'}')
    else:
        environ = os.environ
        braces = ('{', '}')

    def lookup(match):
        name = match.group(1)
        if name.startswith(braces[0]) and name.endswith(braces[1]):
            name = name[1:-1]
        try:
            return environ[name]
        except KeyError:
            return match.group(0)

    return finder.sub(lookup, path)


def _normpath(path):
    """Collapse redundant slashes and '.' components and resolve '..'
    lexically (which can change the meaning of paths with symbolic links)."""
    path = os.fspath(path)
    if isinstance(path, bytes):
        slash, here, up = b'/', b'.', b'..'
    else:
        slash, here, up = '/', '.', '..'
    if not path:
        return here
    _, root, rest = _splitroot(path)
    kept = []
    for part in rest.split(slash):
        if not part or part == here:
            continue
        if part == up and kept and kept[-1] != up:
            kept.pop()
        elif part != up or not root:
            kept.append(part)
    return (root + slash.join(kept)) or here


try:
    from posix import _path_normpath as normpath
except ImportError:
    normpath = _normpath


def abspath(path):
    """Return an absolute, normalized version of path."""
    path = os.fspath(path)
    if isinstance(path, bytes):
        if not path.startswith(b'/'):
            path = join(os.getcwdb(), path)
    elif not path.startswith('/'):
        path = join(os.getcwd(), path)
    return normpath(path)


# realpath() walks the components of the path from left to right, keeping
# the part resolved so far.  A symbolic link is replaced by its target,
# whose components are then walked in turn; `seen` maps each link to its
# resolution (None while it is being resolved, which detects loops).

class _Finished:
    """Marker on the work stack: the link has been fully resolved."""
    __slots__ = ('link',)

    def __init__(self, link):
        self.link = link


def realpath(filename, /, *, strict=False):
    """Return the canonical path of filename, with every symbolic link
    resolved and '.', '..' and redundant slashes removed.

    strict selects what happens when a component cannot be resolved:
    False (default) keeps the rest of the path as given; True raises the
    OSError; ALL_BUT_LAST tolerates only a missing last component;
    ALLOW_MISSING tolerates missing components anywhere (other errors,
    such as symlink loops or non-directories, are still raised).
    """
    path = os.fspath(filename)
    if isinstance(path, bytes):
        slash, here, up = b'/', b'.', b'..'
        getcwd = os.getcwdb
    else:
        slash, here, up = '/', '.', '..'
        getcwd = os.getcwd
    if strict is ALLOW_MISSING:
        lenient, may_miss = False, 'anywhere'
    elif strict is ALL_BUT_LAST:
        lenient, may_miss = False, 'last'
    elif strict:
        lenient, may_miss = False, None
    else:
        lenient, may_miss = True, None

    def more_follows():
        # whether a component other than '' or '.' is still to come
        for item in todo:
            if item.__class__ is not _Finished and item and item != here:
                return True
        return False

    def not_a_directory():
        raise NotADirectoryError(errno.ENOTDIR, os.strerror(errno.ENOTDIR),
                                 resolved)

    if path.startswith(slash):
        resolved = slash
    else:
        resolved = getcwd()
    # what `resolved` is: 'dir', 'other' (not a directory), or None
    # (missing or unknown)
    kind = 'dir'
    seen = {}
    todo = path.split(slash)
    todo.reverse()
    while todo:
        part = todo.pop()
        if part.__class__ is _Finished:
            seen[part.link] = (resolved, kind)
            continue
        if not part or part == here:
            if kind == 'other' and not lenient:
                not_a_directory()
            continue
        if part == up:
            if kind == 'other' and not lenient:
                not_a_directory()
            resolved = resolved.rpartition(slash)[0] or slash
            if kind is not None:
                kind = 'dir'
            continue
        if resolved == slash:
            candidate = slash + part
        else:
            candidate = resolved + slash + part
        if candidate in seen:
            known = seen[candidate]
            if known is not None:
                resolved, kind = known
                continue
            # the link needs itself to be resolved: a loop
            if not lenient:
                raise OSError(errno.ELOOP, os.strerror(errno.ELOOP),
                              candidate)
            resolved, kind = candidate, None
            continue
        try:
            info = os.lstat(candidate)
        except OSError as exc:
            if lenient:
                resolved, kind = candidate, None
                continue
            if isinstance(exc, FileNotFoundError) and (
                    may_miss == 'anywhere' or
                    (may_miss == 'last' and not more_follows())):
                resolved, kind = candidate, None
                continue
            raise
        mode = info.st_mode
        if not stat.S_ISLNK(mode):
            resolved = candidate
            kind = 'dir' if stat.S_ISDIR(mode) else 'other'
            continue
        try:
            target = os.readlink(candidate)
        except OSError:
            if lenient:
                resolved, kind = candidate, None
                continue
            raise
        seen[candidate] = None
        todo.append(_Finished(candidate))
        if target.startswith(slash):
            resolved, kind = slash, 'dir'
        steps = target.split(slash)
        steps.reverse()
        todo.extend(steps)
    return resolved


supports_unicode_filenames = (sys.platform == 'darwin')


def relpath(path, start=None):
    """Return path relative to start (default: the current directory).
    Both are made absolute first; no symbolic links are resolved."""
    path = os.fspath(path)
    if not path:
        raise ValueError("no path specified")
    if isinstance(path, bytes):
        here, slash, up = b'.', b'/', b'..'
    else:
        here, slash, up = '.', '/', '..'
    start = here if start is None else os.fspath(start)
    if type(path) is not type(start):
        _reject_mixed_types('relpath', path, start)
    target = [part for part in abspath(path).split(slash) if part]
    origin = [part for part in abspath(start).split(slash) if part]
    shared = 0
    for mine, theirs in zip(target, origin):
        if mine != theirs:
            break
        shared += 1
    steps = [up] * (len(origin) - shared) + target[shared:]
    if not steps:
        return here
    return slash.join(steps)


def commonpath(paths):
    """Return the longest sub-path shared by every path of the iterable
    (all absolute or all relative; '.' and redundant slashes ignored)."""
    paths = tuple(map(os.fspath, paths))
    if not paths:
        raise ValueError('commonpath() arg is an empty iterable')
    kinds = {type(p) for p in paths}
    if len(kinds) > 1 or not kinds <= {str, bytes}:
        _reject_mixed_types('commonpath', *paths)
    if isinstance(paths[0], bytes):
        slash, here = b'/', b'.'
    else:
        slash, here = '/', '.'
    rooted = {p.startswith(slash) for p in paths}
    if len(rooted) > 1:
        raise ValueError("Can't mix absolute and relative paths")
    split_paths = [[part for part in p.split(slash) if part and part != here]
                   for p in paths]
    common = split_paths[0]
    for parts in split_paths[1:]:
        size = 0
        for mine, theirs in zip(common, parts):
            if mine != theirs:
                break
            size += 1
        common = common[:size]
    prefix = slash if rooted.pop() else slash[:0]
    return prefix + slash.join(common)
