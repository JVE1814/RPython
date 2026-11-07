"""Pathname manipulation for Windows paths (os.path on Windows).

All functions accept str or bytes (or path-like objects) and return the
same type.  Both '\\' and '/' are accepted as separators; results use
'\\'.  On other systems this module is purely lexical: the functions that
need the file system fall back to os-independent emulations.
"""

# Path vocabulary; defined before the imports (os imports this module).
curdir = '.'
pardir = '..'
extsep = '.'
sep = '\\'
pathsep = ';'
altsep = '/'
defpath = '.;C:\\bin'
devnull = 'nul'

import os
import sys
import genericpath
from genericpath import *

__all__ = ["normcase", "isabs", "join", "splitdrive", "splitroot", "split", "splitext",
           "basename", "dirname", "commonprefix", "getsize", "getmtime",
           "getatime", "getctime", "islink", "exists", "lexists", "isdir", "isfile",
           "ismount", "isreserved", "expanduser", "expandvars", "normpath",
           "abspath", "curdir", "pardir", "sep", "pathsep", "defpath", "altsep",
           "extsep", "devnull", "realpath", "supports_unicode_filenames", "relpath",
           "samefile", "sameopenfile", "samestat", "commonpath", "isjunction",
           "isdevdrive", "ALLOW_MISSING", "ALL_BUT_LAST"]

supports_unicode_filenames = True


class _Vocab:
    """The separator and special strings in either str or bytes form."""
    __slots__ = ('sep', 'alt', 'both', 'colon', 'dot', 'dotdot', 'empty', 'unc')

    def __init__(self, kind):
        conv = (lambda s: s.encode('ascii')) if kind is bytes else (lambda s: s)
        self.sep = conv('\\')
        self.alt = conv('/')
        self.both = conv('\\/')
        self.colon = conv(':')
        self.dot = conv('.')
        self.dotdot = conv('..')
        self.empty = conv('')
        self.unc = conv('\\\\?\\UNC\\')


_STR = _Vocab(str)
_BYTES = _Vocab(bytes)


def _vocab(path):
    return _BYTES if isinstance(path, bytes) else _STR


def normcase(s, /):
    """Lower-case the path and turn '/' into '\\'."""
    s = os.fspath(s)
    if isinstance(s, bytes):
        return os.fsencode(os.fsdecode(s).replace('/', '\\').lower())
    return s.replace('/', '\\').lower()


def splitroot(p, /):
    """Split a path into (drive, root, tail).

    drive is a drive letter with colon ('C:'), a UNC share
    ('\\\\server\\share') or a device namespace prefix; root is a single
    separator or empty; tail is the rest.  drive + root + tail == p.
    """
    p = os.fspath(p)
    v = _vocab(p)
    unified = p.replace(v.alt, v.sep)
    if unified[:1] == v.sep:
        if unified[1:2] != v.sep:
            # rooted but drive-less, e.g. \Windows
            return v.empty, p[:1], p[1:]
        # UNC or device path: \\server\share\... or \\?\UNC\server\share\...
        first = 8 if unified[:8].upper() == v.unc else 2
        end_server = unified.find(v.sep, first)
        if end_server < 0:
            return p, v.empty, v.empty
        end_share = unified.find(v.sep, end_server + 1)
        if end_share < 0:
            return p, v.empty, v.empty
        return p[:end_share], p[end_share:end_share + 1], p[end_share + 1:]
    if unified[1:2] == v.colon:
        if unified[2:3] == v.sep:
            return p[:2], p[2:3], p[3:]
        return p[:2], v.empty, p[2:]
    return v.empty, v.empty, p


def splitdrive(p, /):
    """Split a path into (drive, rest); drive + rest == p.

    The drive is a drive letter with colon or a UNC share such as
    '//host/share'.
    """
    drive, root, tail = splitroot(p)
    return drive, root + tail


def isabs(s, /):
    """True if the path has both a drive (or UNC share) and a root."""
    s = os.fspath(s)
    v = _vocab(s)
    head = s[:3].replace(v.alt, v.sep)
    return head[1:3] == v.colon + v.sep or head[:2] == v.sep * 2


def join(path, /, *paths):
    """Join path components the way Windows resolves them."""
    path = os.fspath(path)
    v = _vocab(path)
    try:
        drive, root, tail = splitroot(path)
        for piece in paths:
            p_drive, p_root, p_tail = splitroot(piece)
            if p_root:
                # an absolute piece replaces everything (keeping our drive
                # if it has none of its own)
                if p_drive or not drive:
                    drive = p_drive
                root, tail = p_root, p_tail
                continue
            if p_drive and p_drive != drive:
                if p_drive.lower() != drive.lower():
                    drive, root, tail = p_drive, p_root, p_tail
                    continue
                drive = p_drive   # same drive, different case
            if tail and tail[-1:] not in v.both:
                tail += v.sep
            tail += p_tail
        if tail and not root and drive and drive[-1:] not in v.colon + v.both:
            # relative tail after a UNC share needs a separator
            return drive + v.sep + tail
        return drive + root + tail
    except (TypeError, AttributeError, BytesWarning):
        genericpath._check_arg_types('join', path, *paths)
        raise


def split(p, /):
    """Split into (head, tail) where tail is the last component."""
    p = os.fspath(p)
    v = _vocab(p)
    drive, root, rest = splitroot(p)
    cut = len(rest)
    while cut and rest[cut - 1:cut] not in v.both:
        cut -= 1
    head, tail = rest[:cut], rest[cut:]
    return drive + root + head.rstrip(v.both), tail


def splitext(p, /):
    """Split the extension from a pathname: (root, ext) with root + ext == p."""
    p = os.fspath(p)
    if isinstance(p, bytes):
        return genericpath._splitext(p, b'\\', b'/', b'.')
    return genericpath._splitext(p, '\\', '/', '.')


def basename(p, /):
    """The final component of a pathname."""
    return split(p)[1]


def dirname(p, /):
    """The directory component of a pathname."""
    return split(p)[0]


def ismount(path):
    """True for drive roots and UNC share roots."""
    path = os.fspath(path)
    v = _vocab(path)
    drive, root, rest = splitroot(abspath(path))
    if drive[:1] in (v.sep, v.alt) and drive:
        return not rest
    return bool(root) and not rest


_RESERVED_CHARS = frozenset('"*:<>?|/\\' + ''.join(chr(c) for c in range(32)))
_RESERVED_NAMES = frozenset(
    ['CON', 'PRN', 'AUX', 'NUL', 'CONIN$', 'CONOUT$'] +
    [prefix + digit for prefix in ('COM', 'LPT')
     for digit in '123456789\xb9\xb2\xb3'])


def _reserved_component(name):
    if name in ('.', '..'):
        return False
    if name[-1:] in ('.', ' '):
        return True
    if _RESERVED_CHARS.intersection(name):
        return True
    stem = name.partition('.')[0].rstrip(' ').upper()
    return stem in _RESERVED_NAMES


def isreserved(path):
    """True if some component of the path is a name Windows reserves."""
    tail = os.fsdecode(splitroot(path)[2]).replace('/', '\\')
    return any(_reserved_component(part) for part in tail.split('\\'))


def expanduser(path):
    """Replace a leading '~' or '~user' with the user's profile directory."""
    path = os.fspath(path)
    v = _vocab(path)
    tilde = b'~' if v is _BYTES else '~'
    if not path.startswith(tilde):
        return path
    end = 1
    while end < len(path) and path[end:end + 1] not in v.both:
        end += 1
    env = os.environ
    if 'USERPROFILE' in env:
        home = env['USERPROFILE']
    elif 'HOMEPATH' in env:
        home = join(env.get('HOMEDRIVE', ''), env['HOMEPATH'])
    else:
        return path
    if end > 1:
        wanted = path[1:end]
        if isinstance(wanted, bytes):
            wanted = os.fsdecode(wanted)
        if wanted != env.get('USERNAME'):
            # guess a sibling profile directory, only if home looks standard
            if env.get('USERNAME') != basename(home):
                return path
            home = join(dirname(home), wanted)
    if isinstance(path, bytes):
        home = os.fsencode(home)
    return home + path[end:]


def expandvars(path):
    """Expand $name, ${name} and %name% (unknown names are kept as is).

    Text in single quotes is not expanded; '$$' and '%%' stand for literal
    '$' and '%'."""
    path = os.fspath(path)
    as_bytes = isinstance(path, bytes)
    text = os.fsdecode(path) if as_bytes else path
    if '$' not in text and '%' not in text:
        return path

    def lookup(name, original):
        value = os.environ.get(name)
        return original if value is None else value

    word_chars = set('abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-')
    out = []
    i = 0
    n = len(text)
    while i < n:
        c = text[i]
        if c == "'":
            close = text.find("'", i + 1)
            if close < 0:
                out.append(text[i:])
                break
            out.append(text[i:close + 1])
            i = close + 1
        elif c == '%':
            if text[i + 1:i + 2] == '%':
                out.append('%')
                i += 2
                continue
            close = text.find('%', i + 1)
            if close < 0:
                out.append(text[i:])
                break
            name = text[i + 1:close]
            out.append(lookup(name, '%' + name + '%'))
            i = close + 1
        elif c == '$':
            nxt = text[i + 1:i + 2]
            if nxt == '$':
                out.append('$')
                i += 2
            elif nxt == '{':
                close = text.find('}', i + 2)
                if close < 0:
                    out.append(text[i:])
                    break
                name = text[i + 2:close]
                out.append(lookup(name, '${' + name + '}'))
                i = close + 1
            else:
                j = i + 1
                while j < n and text[j] in word_chars:
                    j += 1
                name = text[i + 1:j]
                out.append(lookup(name, '$' + name))
                i = j
        else:
            out.append(c)
            i += 1
    result = ''.join(out)
    return os.fsencode(result) if as_bytes else result


def normpath(path):
    """Collapse redundant separators and '.'/'..' components."""
    path = os.fspath(path)
    v = _vocab(path)
    drive, root, tail = splitroot(path)
    kept = []
    for part in tail.replace(v.alt, v.sep).split(v.sep):
        if not part or part == v.dot:
            continue
        if part == v.dotdot:
            if kept and kept[-1] != v.dotdot:
                kept.pop()
                continue
            if root:
                continue      # '..' at the root goes nowhere
        kept.append(part)
    if not drive and not root and not kept:
        return v.dot
    return drive + root + v.sep.join(kept)


def _emulated_abspath(path):
    path = os.fspath(path)
    if not isabs(path):
        cwd = os.getcwdb() if isinstance(path, bytes) else os.getcwd()
        path = join(cwd, path)
    return normpath(path)


try:
    from nt import _getfullpathname
except ImportError:
    abspath = _emulated_abspath
else:
    def abspath(path):
        """Return an absolute, normalized version of the path."""
        try:
            return _getfullpathname(normpath(path))
        except (OSError, ValueError):
            return _emulated_abspath(path)

ALLOW_MISSING = genericpath.ALLOW_MISSING
ALL_BUT_LAST = genericpath.ALL_BUT_LAST


def realpath(path, /, *, strict=False):
    """Canonical path (lexical emulation: symbolic links are not resolved
    off Windows)."""
    return abspath(path)


def relpath(path, start=None):
    """A relative path from `start` (default: current directory) to `path`."""
    path = os.fspath(path)
    if not path:
        raise ValueError("no path specified")
    v = _vocab(path)
    start = v.dot if start is None else os.fspath(start)
    try:
        start_drive, _, start_tail = splitroot(abspath(start))
        path_drive, _, path_tail = splitroot(abspath(path))
        if normcase(start_drive) != normcase(path_drive):
            raise ValueError(f"path is on mount {path_drive!r}, "
                             f"start on mount {start_drive!r}")
        start_parts = start_tail.split(v.sep) if start_tail else []
        path_parts = path_tail.split(v.sep) if path_tail else []
        common = 0
        for a, b in zip(start_parts, path_parts):
            if normcase(a) != normcase(b):
                break
            common += 1
        pieces = [v.dotdot] * (len(start_parts) - common) + path_parts[common:]
        return v.sep.join(pieces) if pieces else v.dot
    except (TypeError, ValueError, AttributeError, BytesWarning, DeprecationWarning):
        genericpath._check_arg_types('relpath', path, start)
        raise


def commonpath(paths):
    """The longest common sub-path of the given paths (case-insensitive,
    using the spelling of the first path)."""
    paths = tuple(map(os.fspath, paths))
    if not paths:
        raise ValueError('commonpath() arg is an empty iterable')
    v = _vocab(paths[0])
    try:
        parsed = [splitroot(p.replace(v.alt, v.sep).lower()) for p in paths]
        if len({drive for drive, _, _ in parsed}) != 1:
            raise ValueError("Paths don't have the same drive")
        drive, root, first_tail = splitroot(paths[0].replace(v.alt, v.sep))
        if len({r for _, r, _ in parsed}) != 1:
            if drive:
                raise ValueError("Can't mix absolute and relative paths")
            raise ValueError("Can't mix rooted and not-rooted paths")

        def components(tail):
            return [c for c in tail.split(v.sep) if c and c != v.dot]

        lowered = [components(tail) for _, _, tail in parsed]
        spelled = components(first_tail)
        shared = 0
        shortest = min(len(c) for c in lowered)
        while shared < shortest and all(c[shared] == lowered[0][shared] for c in lowered):
            shared += 1
        return drive + root + v.sep.join(spelled[:shared])
    except (TypeError, AttributeError):
        genericpath._check_arg_types('commonpath', *paths)
        raise


try:
    # Faster native predicates on Windows.
    from nt import _path_isdir as isdir
    from nt import _path_isfile as isfile
    from nt import _path_islink as islink
    from nt import _path_isjunction as isjunction
    from nt import _path_exists as exists
    from nt import _path_lexists as lexists
except ImportError:
    pass

try:
    from nt import _path_isdevdrive

    def isdevdrive(path):
        """True if the path is on a Windows Dev Drive."""
        try:
            return _path_isdevdrive(abspath(path))
        except OSError:
            return False
except ImportError:
    pass
