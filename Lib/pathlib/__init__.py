"""Object-oriented filesystem paths.

Pure path classes (PurePath, PurePosixPath, PureWindowsPath) do purely
lexical work; concrete classes (Path, PosixPath, WindowsPath) add file
system access.  PurePath() and Path() create the flavour of the running
system.
"""

import io
import ntpath
import os
import posixpath
import sys
from errno import EXDEV
from stat import S_ISBLK, S_ISCHR, S_ISDIR, S_ISFIFO, S_ISREG, S_ISSOCK
from _collections_abc import Sequence

try:
    import pwd
except ImportError:
    pwd = None
try:
    import grp
except ImportError:
    grp = None

from pathlib import _glob
from pathlib._os import (
    PathInfo, DirEntryInfo, vfsopen, vfspath, copyfileobj, copy_info,
    ensure_different_files, ensure_distinct_paths,
)
from pathlib.types import _JoinablePath, _ReadablePath, _WritablePath

__all__ = [
    "UnsupportedOperation",
    "PurePath", "PurePosixPath", "PureWindowsPath",
    "Path", "PosixPath", "WindowsPath",
]


class UnsupportedOperation(NotImplementedError):
    """A path operation is not available on this system or path type."""


class _PathParents(Sequence):
    """Read-only sequence view of a path's ancestors (nearest first)."""
    __slots__ = ('_path', '_drv', '_root', '_tail')

    def __init__(self, path):
        self._path = path
        self._drv = path.drive
        self._root = path.root
        self._tail = path._tail

    def __len__(self):
        return len(self._tail)

    def __getitem__(self, index):
        if isinstance(index, slice):
            return tuple(self[i] for i in range(*index.indices(len(self))))
        count = len(self)
        if not -count <= index < count:
            raise IndexError(index)
        if index < 0:
            index += count
        return self._path._from_parsed_parts(self._drv, self._root,
                                             self._tail[:count - index - 1])

    def __repr__(self):
        return f"<{type(self._path).__name__}.parents>"


class PurePath:
    """A filesystem path without I/O; PurePath(...) returns the flavour
    of the running system (PurePosixPath or PureWindowsPath)."""

    # Instances keep the raw constructor arguments and parse them lazily:
    #   _raw_paths   unjoined argument strings (joined on first use)
    #   _drv, _root, _tail_cached   parsed drive, root and component list
    #   _str         the normalized string
    #   _str_normcase_cached, _parts_normcase_cached, _hash   comparison keys
    __slots__ = (
        '_raw_paths', '_drv', '_root', '_tail_cached', '_str',
        '_str_normcase_cached', '_parts_normcase_cached', '_hash',
    )
    parser = os.path

    def __new__(cls, *args, **kwargs):
        if cls is PurePath:
            cls = PureWindowsPath if os.name == 'nt' else PurePosixPath
        return object.__new__(cls)

    def __init__(self, *args):
        raw = []
        own_sep = self.parser.sep
        for arg in args:
            if isinstance(arg, PurePath):
                if arg.parser is self.parser:
                    raw.extend(arg._raw_paths)
                else:
                    # translate separators of a foreign flavour
                    raw.extend(p.replace(arg.parser.sep, own_sep)
                               for p in arg._raw_paths)
                continue
            try:
                text = os.fspath(arg)
            except TypeError:
                text = arg
            if not isinstance(text, str):
                raise TypeError(
                    "argument should be a str or an os.PathLike object where "
                    f"__fspath__ returns a str, not {type(text).__name__!r}")
            raw.append(str(text))
        self._raw_paths = raw

    def with_segments(self, *pathsegments):
        return type(self)(*pathsegments)

    # ---- string forms

    @property
    def _raw_path(self):
        raw = self._raw_paths
        if len(raw) != 1:
            joined = self.parser.join(*raw) if raw else ''
            raw[:] = [joined]
        return raw[0]

    @classmethod
    def _parse_path(cls, path):
        """(drive, root, components) of a path string."""
        if not path:
            return '', '', []
        sep = cls.parser.sep
        if cls.parser.altsep:
            path = path.replace(cls.parser.altsep, sep)
        drive, root, rest = cls.parser.splitroot(path)
        if not root and drive.startswith(sep) and not drive.endswith(sep):
            # a UNC drive always implies a root
            pieces = drive.split(sep)
            if (len(pieces) == 4 and pieces[2] not in '?.') or len(pieces) == 6:
                root = sep
        return drive, root, [part for part in rest.split(sep)
                             if part and part != '.']

    def _ensure_parsed(self):
        try:
            return self._drv, self._root, self._tail_cached
        except AttributeError:
            parsed = self._parse_path(self._raw_path)
            self._drv, self._root, self._tail_cached = parsed
            return parsed

    @property
    def drive(self):
        """The drive or UNC share ('' if none)."""
        return self._ensure_parsed()[0]

    @property
    def root(self):
        """The root separator ('' if none)."""
        return self._ensure_parsed()[1]

    @property
    def _tail(self):
        return self._ensure_parsed()[2]

    @classmethod
    def _format_parsed_parts(cls, drv, root, tail):
        sep = cls.parser.sep
        if drv or root:
            return drv + root + sep.join(tail)
        if tail and cls.parser.splitdrive(tail[0])[0]:
            # a first component like 'c:' must not read as a drive
            tail = ['.'] + tail
        return sep.join(tail)

    def _from_parsed_string(self, text):
        path = self.with_segments(text)
        path._str = text or '.'
        return path

    def _from_parsed_parts(self, drv, root, tail):
        path = self._from_parsed_string(self._format_parsed_parts(drv, root, tail))
        path._drv, path._root, path._tail_cached = drv, root, tail
        return path

    def __str__(self):
        try:
            return self._str
        except AttributeError:
            drv, root, tail = self._ensure_parsed()
            self._str = self._format_parsed_parts(drv, root, tail) or '.'
            return self._str

    __fspath__ = __str__
    __vfspath__ = __str__

    def as_posix(self):
        """The path as a string with forward slashes."""
        return str(self).replace(self.parser.sep, '/')

    def __bytes__(self):
        return os.fsencode(self)

    def __repr__(self):
        return f"{type(self).__name__}({self.as_posix()!r})"

    def __reduce__(self):
        return type(self), tuple(self._raw_paths)

    # ---- comparison

    @property
    def _str_normcase(self):
        try:
            return self._str_normcase_cached
        except AttributeError:
            text = str(self)
            if self.parser is not posixpath:
                text = text.lower()
            self._str_normcase_cached = text
            return text

    @property
    def _parts_normcase(self):
        try:
            return self._parts_normcase_cached
        except AttributeError:
            self._parts_normcase_cached = self._str_normcase.split(self.parser.sep)
            return self._parts_normcase_cached

    def __eq__(self, other):
        if not isinstance(other, PurePath):
            return NotImplemented
        return self.parser is other.parser and self._str_normcase == other._str_normcase

    def __hash__(self):
        try:
            return self._hash
        except AttributeError:
            self._hash = hash(self._str_normcase)
            return self._hash

    def _ordering_key(self, other):
        if not isinstance(other, PurePath) or self.parser is not other.parser:
            return None
        return self._parts_normcase, other._parts_normcase

    def __lt__(self, other):
        keys = self._ordering_key(other)
        return NotImplemented if keys is None else keys[0] < keys[1]

    def __le__(self, other):
        keys = self._ordering_key(other)
        return NotImplemented if keys is None else keys[0] <= keys[1]

    def __gt__(self, other):
        keys = self._ordering_key(other)
        return NotImplemented if keys is None else keys[0] > keys[1]

    def __ge__(self, other):
        keys = self._ordering_key(other)
        return NotImplemented if keys is None else keys[0] >= keys[1]

    # ---- joining and components (docstrings come from pathlib.types)

    def joinpath(self, *pathsegments):
        return self.with_segments(self, *pathsegments)

    def __truediv__(self, key):
        try:
            return self.with_segments(self, key)
        except TypeError:
            return NotImplemented

    def __rtruediv__(self, key):
        try:
            return self.with_segments(key, self)
        except TypeError:
            return NotImplemented

    @property
    def anchor(self):
        return self.drive + self.root

    @property
    def parts(self):
        anchor = self.anchor
        return ((anchor,) if anchor else ()) + tuple(self._tail)

    @property
    def parent(self):
        tail = self._tail
        if not tail:
            return self
        return self._from_parsed_parts(self.drive, self.root, tail[:-1])

    @property
    def parents(self):
        return _PathParents(self)

    @property
    def name(self):
        tail = self._tail
        return tail[-1] if tail else ''

    @property
    def suffix(self):
        suffixes = self.suffixes
        return suffixes[-1] if suffixes else ''

    @property
    def suffixes(self):
        pieces = self.name.lstrip('.').split('.')
        return ['.' + piece for piece in pieces[1:]]

    @property
    def stem(self):
        name = self.name
        cut = len(self.suffix)
        return name[:len(name) - cut] if cut else name

    def with_name(self, name):
        parser = self.parser
        if (not name or name == '.' or parser.sep in name
                or (parser.altsep and parser.altsep in name)):
            raise ValueError(f"Invalid name {name!r}")
        tail = list(self._tail)
        if not tail:
            raise ValueError(f"{self!r} has an empty name")
        tail[-1] = name
        return self._from_parsed_parts(self.drive, self.root, tail)

    def with_stem(self, stem):
        suffix = self.suffix
        if suffix and not stem:
            raise ValueError(f"{self!r} has a non-empty suffix")
        return self.with_name(stem + suffix)

    def with_suffix(self, suffix):
        stem = self.stem
        if not stem:
            raise ValueError(f"{self!r} has an empty name")
        if suffix and not suffix.startswith('.'):
            raise ValueError(f"Invalid suffix {suffix!r}")
        return self.with_name(stem + suffix)

    def _coerce(self, other):
        return other if hasattr(other, 'with_segments') else self.with_segments(other)

    def relative_to(self, other, *, walk_up=False):
        other = self._coerce(other)
        ups = []
        for base in (other, *other.parents):
            if base == self or base in self.parents:
                break
            if not walk_up:
                raise ValueError(f"{str(self)!r} is not in the subpath of {str(other)!r}")
            if base.name == '..':
                raise ValueError(f"'..' segment in {str(other)!r} cannot be walked")
            ups.append('..')
        else:
            raise ValueError(f"{str(self)!r} and {str(other)!r} have different anchors")
        return self._from_parsed_parts('', '', ups + self._tail[len(base._tail):])

    def is_relative_to(self, other):
        other = self._coerce(other)
        return other == self or other in self.parents

    def is_absolute(self):
        """True if the path has a root (and, on Windows, a drive)."""
        if self.parser is posixpath:
            return any(raw.startswith('/') for raw in self._raw_paths)
        return self.parser.isabs(self)

    def is_reserved(self):
        """True for paths Windows reserves (deprecated; see
        os.path.isreserved())."""
        import warnings
        warnings._deprecated(
            "pathlib.PurePath.is_reserved",
            "pathlib.PurePath.is_reserved() is deprecated and scheduled for "
            "removal in Python 3.15. Use os.path.isreserved() to detect "
            "reserved paths on Windows.", remove=(3, 15))
        return self.parser is ntpath and self.parser.isreserved(self)

    def as_uri(self):
        """The path as a 'file:' URI (deprecated; use Path.as_uri())."""
        import warnings
        warnings._deprecated(
            "pathlib.PurePath.as_uri",
            "pathlib.PurePath.as_uri() is deprecated and scheduled for "
            "removal in Python 3.19. Use pathlib.Path.as_uri().",
            remove=(3, 19))
        return _file_uri(self)

    # ---- matching

    def full_match(self, pattern, *, case_sensitive=None):
        pattern = self._coerce(pattern)
        if case_sensitive is None:
            case_sensitive = self.parser is posixpath
        # an empty path is spelled '.', but must not match '*'
        subject = str(self) if self.parts else ''
        spec = str(pattern) if pattern.parts else ''
        seps = self.parser.sep + (self.parser.altsep or '')
        return _glob.compile_pattern(spec, seps, case_sensitive)(subject) is not None

    def match(self, path_pattern, *, case_sensitive=None):
        """Match the glob-style pattern from the right (relative patterns)
        or completely (anchored patterns); '**' acts like '*'."""
        pattern = self._coerce(path_pattern)
        if case_sensitive is None:
            case_sensitive = self.parser is posixpath
        mine = self._tail[::-1]
        theirs = pattern._tail[::-1]
        if not theirs and not pattern.anchor:
            raise ValueError("empty pattern")
        if len(mine) < len(theirs):
            return False
        if pattern.anchor and len(mine) != len(theirs):
            return False
        seps = self.parser.sep + (self.parser.altsep or '')
        for part, spec in zip(mine, theirs):
            if _glob._compile_segment(spec, seps, case_sensitive)(part) is None:
                return False
        if pattern.anchor:
            if self.root != pattern.root:
                return False
            if (pattern.drive or self.drive) and _glob.compile_pattern(
                    pattern.drive, seps, case_sensitive)(self.drive) is None:
                return False
        return True


os.PathLike.register(PurePath)
_JoinablePath.register(PurePath)


_URI_SAFE = frozenset(b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ"
                      b"0123456789_.-~/")


def _percent_encode(data):
    return ''.join(chr(b) if b in _URI_SAFE else '%%%02X' % b for b in data)


def _file_uri(path):
    if not path.is_absolute():
        raise ValueError("relative paths can't be expressed as file URIs")
    if path.parser is posixpath:
        return 'file://' + _percent_encode(os.fsencode(str(path)))
    drive = path.drive
    posix_form = path.as_posix()
    if len(drive) == 2 and drive[1] == ':':
        return 'file:///' + drive + _percent_encode(posix_form[2:].encode('utf-8'))
    if drive:
        return 'file:' + _percent_encode(posix_form.encode('utf-8'))
    return 'file://' + _percent_encode(posix_form.encode('utf-8'))


def _percent_decode(text):
    if '%' not in text:
        return text
    out = bytearray()
    i = 0
    while i < len(text):
        piece = text[i:i + 3]
        if (len(piece) == 3 and piece[0] == '%'
                and all(c in '0123456789abcdefABCDEF' for c in piece[1:])):
            out.append(int(piece[1:], 16))
            i += 3
        else:
            out += text[i].encode('utf-8', 'surrogateescape')
            i += 1
    return out.decode('utf-8', 'surrogateescape')


class PurePosixPath(PurePath):
    """Pure path with POSIX semantics (usable on any system)."""
    parser = posixpath
    __slots__ = ()


class PureWindowsPath(PurePath):
    """Pure path with Windows semantics (usable on any system)."""
    parser = ntpath
    __slots__ = ()


def _scan_local(directory):
    with os.scandir(directory or '.') as entries:
        found = list(entries)
    for entry in found:
        yield entry.name, (lambda follow, entry=entry:
                           entry.is_dir(follow_symlinks=follow))


def _hostname_is_local(host):
    if host in ('', 'localhost'):
        return True
    try:
        import socket
        return host == socket.gethostname()
    except Exception:
        return False


class Path(PurePath):
    """A filesystem path with I/O methods; Path(...) returns the flavour of
    the running system (PosixPath or WindowsPath)."""
    __slots__ = ('_info',)

    def __new__(cls, *args, **kwargs):
        if cls is Path:
            cls = WindowsPath if os.name == 'nt' else PosixPath
        return object.__new__(cls)

    @property
    def info(self):
        try:
            return self._info
        except AttributeError:
            self._info = PathInfo(self)
            return self._info

    # ---- status

    def stat(self, *, follow_symlinks=True):
        """os.stat() of the path."""
        return os.stat(self, follow_symlinks=follow_symlinks)

    def lstat(self):
        """os.lstat() of the path (symbolic links are not followed)."""
        return os.lstat(self)

    def exists(self, *, follow_symlinks=True):
        """True if the path exists (a dangling link counts only with
        follow_symlinks=False)."""
        return os.path.exists(self) if follow_symlinks else os.path.lexists(self)

    def _has_mode(self, test, follow_symlinks=True):
        try:
            return test(self.stat(follow_symlinks=follow_symlinks).st_mode)
        except (OSError, ValueError):
            return False

    def is_dir(self, *, follow_symlinks=True):
        """True if the path is a directory."""
        if follow_symlinks:
            return os.path.isdir(self)
        return self._has_mode(S_ISDIR, False)

    def is_file(self, *, follow_symlinks=True):
        """True if the path is a regular file."""
        if follow_symlinks:
            return os.path.isfile(self)
        return self._has_mode(S_ISREG, False)

    def is_mount(self):
        """True if the path is a mount point."""
        return os.path.ismount(self)

    def is_symlink(self):
        """True if the path is a symbolic link."""
        return os.path.islink(self)

    def is_junction(self):
        """True if the path is a Windows junction."""
        return os.path.isjunction(self)

    def is_block_device(self):
        """True if the path is a block device."""
        return self._has_mode(S_ISBLK)

    def is_char_device(self):
        """True if the path is a character device."""
        return self._has_mode(S_ISCHR)

    def is_fifo(self):
        """True if the path is a named pipe."""
        return self._has_mode(S_ISFIFO)

    def is_socket(self):
        """True if the path is a socket."""
        return self._has_mode(S_ISSOCK)

    def samefile(self, other_path):
        """True if both paths refer to the same file."""
        mine = self.stat()
        try:
            theirs = other_path.stat()
        except AttributeError:
            theirs = self.with_segments(other_path).stat()
        return mine.st_ino == theirs.st_ino and mine.st_dev == theirs.st_dev

    # ---- file contents

    def open(self, mode='r', buffering=-1, encoding=None,
             errors=None, newline=None):
        """Open the file, like the built-in open()."""
        if "b" not in mode:
            encoding = io.text_encoding(encoding)
        return io.open(self, mode, buffering, encoding, errors, newline)

    def read_bytes(self):
        with self.open(mode='rb', buffering=0) as stream:
            return stream.read()

    def read_text(self, encoding=None, errors=None, newline=None):
        encoding = io.text_encoding(encoding)
        with self.open(mode='r', encoding=encoding, errors=errors,
                       newline=newline) as stream:
            return stream.read()

    def write_bytes(self, data):
        view = memoryview(data)
        with self.open(mode='wb') as stream:
            return stream.write(view)

    def write_text(self, data, encoding=None, errors=None, newline=None):
        encoding = io.text_encoding(encoding)
        if not isinstance(data, str):
            raise TypeError(f'data must be str, not {type(data).__name__}')
        with self.open(mode='w', encoding=encoding, errors=errors,
                       newline=newline) as stream:
            return stream.write(data)

    def __open_reader__(self):
        return io.open(self, 'rb')

    def __open_writer__(self, mode):
        return io.open(self, f'{mode}b')

    # ---- directories

    def _child(self, entry, text):
        child = self.with_segments(text)
        child._str = text
        child._info = DirEntryInfo(entry)
        return child

    def iterdir(self):
        here = str(self)
        with os.scandir(here) as scanner:
            entries = list(scanner)
        if here == '.':
            return (self._child(e, e.name) for e in entries)
        return (self._child(e, e.path) for e in entries)

    def glob(self, pattern, *, case_sensitive=None, recurse_symlinks=False):
        sys.audit("pathlib.Path.glob", self, pattern)
        pattern = self._coerce(pattern)
        if pattern.anchor:
            raise NotImplementedError("Non-relative patterns are unsupported")
        segments = list(pattern._tail)
        if not segments:
            raise ValueError(f"Unacceptable pattern: {str(pattern)!r}")
        sep = self.parser.sep
        if pattern._raw_path[-1:] in (sep, self.parser.altsep):
            segments.append('')    # a trailing separator selects directories
        if case_sensitive is None:
            case_sensitive = self.parser is posixpath
        here = str(self)
        base = '' if here == '.' else (here if here.endswith(sep) else here + sep)
        globber = _glob.Globber(sep, case_sensitive, _scan_local,
                                os.path.lexists, recurse_symlinks)
        anchor_len = len(self.anchor)

        def tidy(text):
            if len(text) > anchor_len and text.endswith(sep):
                text = text[:-1]
            return self.with_segments(text)

        return map(tidy, globber.select(base, segments))

    def rglob(self, pattern, *, case_sensitive=None, recurse_symlinks=False):
        """Like glob() with '**/' prepended to the pattern."""
        sys.audit("pathlib.Path.rglob", self, pattern)
        return self.glob(self.parser.join('**', pattern),
                         case_sensitive=case_sensitive,
                         recurse_symlinks=recurse_symlinks)

    def walk(self, top_down=True, on_error=None, follow_symlinks=False):
        sys.audit("pathlib.Path.walk", self, on_error, follow_symlinks)
        here = str(self)
        follow = follow_symlinks or os._walk_symlinks_as_files
        for dirpath, dirnames, filenames in os.walk(here, top_down, on_error, follow):
            if here == '.':
                dirpath = dirpath[2:]
            yield self._from_parsed_string(dirpath), dirnames, filenames

    def mkdir(self, mode=0o777, parents=False, exist_ok=False, *, parent_mode=None):
        """Create the directory; with parents=True also missing ancestors
        (created with parent_mode if given)."""
        try:
            os.mkdir(self, mode)
        except FileNotFoundError:
            if not parents or self.parent == self:
                raise
            ancestor_mode = 0o777 if parent_mode is None else parent_mode
            self.parent.mkdir(ancestor_mode, parents=True, exist_ok=True,
                              parent_mode=parent_mode)
            self.mkdir(mode, parents=False, exist_ok=exist_ok)
        except OSError:
            # EEXIST may be masked by other errors (EACCES, EROFS, ...)
            if not exist_ok or not self.is_dir():
                raise

    def rmdir(self):
        """Remove the (empty) directory."""
        os.rmdir(self)

    # ---- names and locations

    def absolute(self):
        """An absolute version of the path (no normalization, symlinks
        are kept)."""
        if self.is_absolute():
            return self
        if self.root:
            return self._from_parsed_parts(os.path.splitroot(os.getcwd())[0],
                                           self.root, self._tail)
        cwd = os.path.abspath(self.drive) if self.drive else os.getcwd()
        if not self._tail:
            return self._from_parsed_string(cwd)
        drive, root, rest = os.path.splitroot(cwd)
        prefix = rest.split(self.parser.sep) if rest else []
        return self._from_parsed_parts(drive, root, prefix + self._tail)

    @classmethod
    def cwd(cls):
        """The current working directory."""
        here = os.getcwd()
        path = cls(here)
        path._str = here
        return path

    def resolve(self, strict=False):
        """An absolute path with symbolic links and '..' resolved."""
        return self.with_segments(os.path.realpath(self, strict=strict))

    def expanduser(self):
        """Replace a leading '~' or '~user' component with the home
        directory."""
        tail = self._tail
        if self.drive or self.root or not tail or tail[0][:1] != '~':
            return self
        home = os.path.expanduser(tail[0])
        if home[:1] == '~':
            raise RuntimeError("Could not determine home directory.")
        drv, root, home_tail = self._parse_path(home)
        return self._from_parsed_parts(drv, root, home_tail + tail[1:])

    @classmethod
    def home(cls):
        """The current user's home directory."""
        home = os.path.expanduser("~")
        if home == "~":
            raise RuntimeError("Could not determine home directory.")
        return cls(home)

    def as_uri(self):
        """The path as a 'file:' URI."""
        return _file_uri(self)

    @classmethod
    def from_uri(cls, uri):
        """Create a path from a 'file:' URI."""
        if not uri.startswith('file:'):
            raise ValueError(f"URI does not start with 'file:': {uri!r}")
        rest = uri[5:]
        if rest.startswith('///'):
            rest = rest[2:]
        elif rest.startswith('//localhost/'):
            rest = rest[11:]
        elif rest.startswith('//') and cls.parser is posixpath:
            host, slash, after = rest[2:].partition('/')
            if _hostname_is_local(host):
                rest = '/' + after
        if rest.startswith('///') or (rest[:1] == '/' and rest[2:3] in (':', '|')):
            rest = rest[1:]
        if rest[1:2] == '|':
            rest = rest[:1] + ':' + rest[2:]
        path = cls(_percent_decode(rest))
        if not path.is_absolute() or (cls.parser is posixpath
                                      and str(path).startswith('//')
                                      and not uri.startswith('file:////')):
            raise ValueError(f"URI is not absolute: {uri!r}")
        return path

    # ---- ownership and permissions

    def owner(self, *, follow_symlinks=True):
        """The user name owning the file."""
        if pwd is None:
            raise UnsupportedOperation(
                f"{type(self).__name__}.owner() is unsupported on this system")
        uid = self.stat(follow_symlinks=follow_symlinks).st_uid
        return pwd.getpwuid(uid).pw_name

    def group(self, *, follow_symlinks=True):
        """The group name owning the file."""
        if grp is None:
            raise UnsupportedOperation(
                f"{type(self).__name__}.group() is unsupported on this system")
        gid = self.stat(follow_symlinks=follow_symlinks).st_gid
        return grp.getgrgid(gid).gr_name

    def chmod(self, mode, *, follow_symlinks=True):
        """Change the permission bits, like os.chmod()."""
        os.chmod(self, mode, follow_symlinks=follow_symlinks)

    def lchmod(self, mode):
        """chmod() without following a symbolic link."""
        self.chmod(mode, follow_symlinks=False)

    # ---- links

    def readlink(self):
        if not hasattr(os, "readlink"):
            raise UnsupportedOperation(
                f"{type(self).__name__}.readlink() is unsupported on this system")
        return self.with_segments(os.readlink(self))

    def symlink_to(self, target, target_is_directory=False):
        if not hasattr(os, "symlink"):
            raise UnsupportedOperation(
                f"{type(self).__name__}.symlink_to() is unsupported on this system")
        os.symlink(target, self, target_is_directory)

    def hardlink_to(self, target):
        """Make this path a hard link to `target` (argument order is the
        reverse of os.link())."""
        if not hasattr(os, "link"):
            raise UnsupportedOperation(
                f"{type(self).__name__}.hardlink_to() is unsupported on this system")
        os.link(target, self)

    # ---- creating, removing, renaming

    def touch(self, mode=0o666, exist_ok=True):
        """Create the file if needed, else update its modification time."""
        if exist_ok:
            try:
                os.utime(self, None)
                return
            except OSError:
                pass
        flags = os.O_CREAT | os.O_WRONLY | (0 if exist_ok else os.O_EXCL)
        os.close(os.open(self, flags, mode))

    def unlink(self, missing_ok=False):
        """Remove the file or link."""
        try:
            os.unlink(self)
        except FileNotFoundError:
            if not missing_ok:
                raise

    def _delete(self):
        # remove a file, link or whole directory tree
        if self.is_symlink() or self.is_junction() or not self.is_dir():
            self.unlink()
        else:
            import shutil
            shutil.rmtree(self)

    def _renamed(self, target):
        return self._coerce(target)

    def rename(self, target):
        """Rename the path to `target` (relative to the current directory
        when relative); returns the new path."""
        os.rename(self, target)
        return self._renamed(target)

    def replace(self, target):
        """Like rename(), but overwrite an existing target."""
        os.replace(self, target)
        return self._renamed(target)

    # ---- copying and moving

    def copy(self, target, **kwargs):
        target = self._coerce(target)
        ensure_distinct_paths(self, target)
        target._copy_from(self, **kwargs)
        return target.joinpath()

    def copy_into(self, target_dir, **kwargs):
        if not self.name:
            raise ValueError(f"{self!r} has an empty name")
        if hasattr(target_dir, 'with_segments'):
            return self.copy(target_dir / self.name, **kwargs)
        return self.copy(self.with_segments(target_dir, self.name), **kwargs)

    def _copy_from(self, source, follow_symlinks=True, preserve_metadata=False):
        info = source.info
        if not follow_symlinks and info.is_symlink():
            os.symlink(vfspath(source.readlink()), self, info.is_dir())
            if preserve_metadata:
                copy_info(info, self, follow_symlinks=False)
        elif info.is_dir():
            children = source.iterdir()
            os.mkdir(self)
            for child in children:
                self.joinpath(child.name)._copy_from(child, follow_symlinks,
                                                     preserve_metadata)
            if preserve_metadata:
                copy_info(info, self)
        else:
            ensure_different_files(source, self)
            with vfsopen(source, 'rb') as reader, open(self, 'wb') as writer:
                copyfileobj(reader, writer)
            if preserve_metadata:
                copy_info(info, self)

    def move(self, target):
        """Move this file or directory tree to `target`, renaming when
        possible and copying across file systems otherwise."""
        try:
            local_target = self.with_segments(target)
        except TypeError:
            local_target = None
        if local_target is not None:
            ensure_different_files(self, local_target)
            try:
                os.replace(self, local_target)
                return local_target.joinpath()
            except OSError as err:
                if err.errno != EXDEV:
                    raise
            target = local_target
        moved = self.copy(target, follow_symlinks=False, preserve_metadata=True)
        self._delete()
        return moved

    def move_into(self, target_dir):
        """Move this file or directory tree into the directory `target_dir`."""
        if not self.name:
            raise ValueError(f"{self!r} has an empty name")
        if hasattr(target_dir, 'with_segments'):
            return self.move(target_dir / self.name)
        return self.move(self.with_segments(target_dir, self.name))


_ReadablePath.register(Path)
_WritablePath.register(Path)


def _refuse_instantiation(cls, *args, **kwargs):
    raise UnsupportedOperation(f"cannot instantiate {cls.__name__!r} on your system")


class PosixPath(Path, PurePosixPath):
    """Concrete path for POSIX systems."""
    __slots__ = ()
    if os.name == 'nt':
        __new__ = staticmethod(_refuse_instantiation)


class WindowsPath(Path, PureWindowsPath):
    """Concrete path for Windows systems."""
    __slots__ = ()
    if os.name != 'nt':
        __new__ = staticmethod(_refuse_instantiation)


def _share_docstrings():
    # Methods implementing the pathlib.types ABCs carry the ABCs' docstrings
    # (tests compare them).
    for abc in (_JoinablePath, _ReadablePath, _WritablePath):
        for name, member in vars(abc).items():
            if name.startswith('_') or name == 'parser':
                continue
            doc = getattr(member, '__doc__', None)
            for cls in (PurePath, Path):
                ours = cls.__dict__.get(name)
                if ours is None:
                    continue
                if isinstance(ours, property):
                    setattr(cls, name, property(ours.fget, ours.fset, ours.fdel, doc))
                elif isinstance(ours, (classmethod, staticmethod)):
                    ours.__func__.__doc__ = doc
                else:
                    try:
                        ours.__doc__ = doc
                    except (AttributeError, TypeError):
                        pass


_share_docstrings()
del _share_docstrings
