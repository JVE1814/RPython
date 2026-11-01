"""Protocols and abstract base classes for pathlib-style path objects.

PathInfo and _PathParser are typing protocols.  The private ABCs
_JoinablePath (lexical operations), _ReadablePath and _WritablePath (I/O)
let other path types (archive members, remote storage, ...) reuse
pathlib's high level methods by implementing a few primitives.
"""

import io
from abc import ABC, abstractmethod
from typing import Optional, Protocol, runtime_checkable

from pathlib._glob import Globber, compile_pattern
from pathlib._os import vfsopen, vfspath, ensure_distinct_paths


def _anchor_and_names(path, split):
    """Split `path` repeatedly: returns (anchor, names) where names lists
    the components after the anchor from last to first."""
    names = []
    head, tail = split(path)
    while head != path:
        names.append(tail)
        path = head
        head, tail = split(path)
    return path, names


@runtime_checkable
class _PathParser(Protocol):
    """The subset of os.path that _JoinablePath needs (see its `parser`)."""

    sep: str
    altsep: Optional[str]
    def split(self, path: str) -> tuple[str, str]: ...
    def splitext(self, path: str) -> tuple[str, str]: ...
    def normcase(self, path: str) -> str: ...


@runtime_checkable
class PathInfo(Protocol):
    """File type queries for a path; implementations may cache answers."""
    def exists(self, *, follow_symlinks: bool = True) -> bool: ...
    def is_dir(self, *, follow_symlinks: bool = True) -> bool: ...
    def is_file(self, *, follow_symlinks: bool = True) -> bool: ...
    def is_symlink(self) -> bool: ...


class _JoinablePath(ABC):
    """Lexical path operations built on three primitives: the `parser`
    attribute, with_segments() and __vfspath__()."""
    __slots__ = ()

    @property
    @abstractmethod
    def parser(self):
        """Module-like object (e.g. posixpath) doing low-level parsing."""
        raise NotImplementedError

    @abstractmethod
    def with_segments(self, *pathsegments):
        """Create a path of the same kind from the given segments.

        Every derived path is created through this method, so subclasses
        can override it to pass on extra state."""
        raise NotImplementedError

    @abstractmethod
    def __vfspath__(self):
        """The path as a string."""
        raise NotImplementedError

    def _split(self):
        return _anchor_and_names(vfspath(self), self.parser.split)

    @property
    def anchor(self):
        """Drive and root together ('' for relative paths)."""
        return self._split()[0]

    @property
    def name(self):
        """The last component ('' if there is none)."""
        return self.parser.split(vfspath(self))[1]

    @property
    def suffix(self):
        """The last dot-suffix of the name, e.g. '.gz' (or '')."""
        suffixes = self.suffixes
        return suffixes[-1] if suffixes else ''

    @property
    def suffixes(self):
        """All dot-suffixes of the name, e.g. ['.tar', '.gz']."""
        pieces = self.name.lstrip('.').split('.')
        return ['.' + piece for piece in pieces[1:]]

    @property
    def stem(self):
        """The name without its last suffix."""
        name = self.name
        cut = len(self.suffix)
        return name[:len(name) - cut] if cut else name

    def with_name(self, name):
        """A path that differs from this one in the last component."""
        if self.parser.split(name)[0]:
            raise ValueError(f"Invalid name {name!r}")
        text = vfspath(self)
        old = self.parser.split(text)[1]
        return self.with_segments(text[:len(text) - len(old)] + name)

    def with_stem(self, stem):
        """A path with a different stem and the same suffix."""
        suffix = self.suffix
        if suffix and not stem:
            raise ValueError(f"{self!r} has a non-empty suffix")
        return self.with_name(stem + suffix)

    def with_suffix(self, suffix):
        """A path with the last suffix replaced, added ('' removes it)."""
        stem = self.stem
        if not stem:
            raise ValueError(f"{self!r} has an empty name")
        if suffix and not suffix.startswith('.'):
            raise ValueError(f"Invalid suffix {suffix!r}")
        return self.with_name(stem + suffix)

    @property
    def parts(self):
        """Tuple of the anchor (if any) followed by the components."""
        anchor, names = self._split()
        names.reverse()
        return ((anchor,) if anchor else ()) + tuple(names)

    def joinpath(self, *pathsegments):
        """Append the segments to this path; an anchored segment replaces
        what precedes it."""
        return self.with_segments(vfspath(self), *pathsegments)

    def __truediv__(self, key):
        try:
            return self.with_segments(vfspath(self), key)
        except TypeError:
            return NotImplemented

    def __rtruediv__(self, key):
        try:
            return self.with_segments(key, vfspath(self))
        except TypeError:
            return NotImplemented

    @property
    def parent(self):
        """The path without its last component (itself if there is none)."""
        text = vfspath(self)
        head = self.parser.split(text)[0]
        return self if head == text else self.with_segments(head)

    @property
    def parents(self):
        """The ancestors of the path, nearest first."""
        found = []
        text = vfspath(self)
        head = self.parser.split(text)[0]
        while head != text:
            found.append(self.with_segments(head))
            text = head
            head = self.parser.split(text)[0]
        return tuple(found)

    def relative_to(self, other, *, walk_up=False):
        """This path expressed relative to `other` (ValueError if that is
        impossible).  With walk_up=True, '..' entries may be used."""
        ups = []
        for base in (other,) + tuple(other.parents):
            if self.is_relative_to(base):
                break
            if not walk_up:
                raise ValueError(f"{self!r} is not in the subpath of {other!r}")
            if base.name == '..':
                raise ValueError(f"'..' segment in {other!r} cannot be walked")
            ups.append('..')
        else:
            raise ValueError(f"{self!r} and {other!r} have different anchors")
        downs = []
        current = self
        while not (current == base):
            downs.append(current.name)
            current = current.parent
        downs.reverse()
        return self.with_segments(*ups, *downs)

    def is_relative_to(self, other):
        """True if `other` is this path or one of its ancestors."""
        return other == self or other in self.parents

    def full_match(self, pattern, *, case_sensitive=None):
        """True if the whole path matches the glob-style pattern ('**'
        matches any number of components)."""
        if not hasattr(pattern, 'with_segments'):
            pattern = self.with_segments(pattern)
        if case_sensitive is None:
            case_sensitive = self.parser.normcase('Aa') == 'Aa'
        seps = self.parser.sep + (self.parser.altsep or '')
        match = compile_pattern(vfspath(pattern), seps, case_sensitive)
        return match(vfspath(self)) is not None


class _ReadablePath(_JoinablePath):
    """Read access built on the primitives `info`, __open_reader__(),
    iterdir() and readlink()."""
    __slots__ = ()

    @property
    @abstractmethod
    def info(self):
        """A PathInfo object describing the file at this path."""
        raise NotImplementedError

    @abstractmethod
    def __open_reader__(self):
        """Open the file for reading in binary mode."""
        raise NotImplementedError

    def read_bytes(self):
        """The contents of the file as bytes."""
        with vfsopen(self, mode='rb') as stream:
            return stream.read()

    def read_text(self, encoding=None, errors=None, newline=None):
        """The contents of the file decoded as text."""
        encoding = io.text_encoding(encoding)
        with vfsopen(self, mode='r', encoding=encoding, errors=errors,
                     newline=newline) as stream:
            return stream.read()

    @abstractmethod
    def iterdir(self):
        """Iterate over the entries of the directory (in no particular
        order, without '.' and '..')."""
        raise NotImplementedError

    def glob(self, pattern, *, recurse_symlinks=True):
        """Iterate over the existing paths below this one that match the
        relative glob pattern."""
        text = vfspath(self.with_segments(pattern))
        anchor, names = _anchor_and_names(text, self.parser.split)
        if anchor:
            raise NotImplementedError("Non-relative patterns are unsupported")
        if not names:
            raise ValueError(f"Unacceptable pattern: {pattern!r}")
        if not recurse_symlinks:
            raise NotImplementedError("recurse_symlinks=False is unsupported")
        sep = self.parser.sep
        if self.parser.altsep:
            text = text.replace(self.parser.altsep, sep)
        make = self.with_segments

        def scan(directory):
            for child in make(directory).iterdir():
                info = child.info
                yield child.name, (lambda follow, info=info:
                                   info.is_dir(follow_symlinks=follow))

        def lexists(path_text):
            return make(path_text).info.exists(follow_symlinks=False)

        base = vfspath(self)
        if base and not base.endswith(sep):
            base += sep
        case_sensitive = self.parser.normcase('Aa') == 'Aa'
        globber = Globber(sep, case_sensitive, scan, lexists, recurse_symlinks)
        return map(make, globber.select(base, text.split(sep)))

    def walk(self, top_down=True, on_error=None, follow_symlinks=False):
        """Yield (dirpath, dirnames, filenames) for this directory tree, as
        os.walk() does."""
        todo = [self]
        while todo:
            item = todo.pop()
            if isinstance(item, tuple):
                yield item
                continue
            dirnames = []
            filenames = []
            try:
                children = list(item.iterdir())
            except OSError as error:
                if on_error is not None:
                    on_error(error)
                continue
            subdirs = []
            for child in children:
                if child.info.is_dir(follow_symlinks=follow_symlinks):
                    dirnames.append(child.name)
                    subdirs.append(child)
                else:
                    filenames.append(child.name)
            if top_down:
                yield item, dirnames, filenames
                todo.extend(item.joinpath(name) for name in reversed(dirnames))
            else:
                todo.append((item, dirnames, filenames))
                todo.extend(reversed(subdirs))

    @abstractmethod
    def readlink(self):
        """The path a symbolic link points to."""
        raise NotImplementedError

    def copy(self, target, **kwargs):
        """Copy this file or directory tree to `target`; returns the new
        path."""
        ensure_distinct_paths(self, target)
        target._copy_from(self, **kwargs)
        return target.joinpath()   # a fresh object without cached info

    def copy_into(self, target_dir, **kwargs):
        """Copy this file or directory tree into the existing directory
        `target_dir`."""
        name = self.name
        if not name:
            raise ValueError(f"{self!r} has an empty name")
        return self.copy(target_dir / name, **kwargs)


class _WritablePath(_JoinablePath):
    """Write access built on the primitives symlink_to(), mkdir() and
    __open_writer__()."""
    __slots__ = ()

    @abstractmethod
    def symlink_to(self, target, target_is_directory=False):
        """Make this path a symbolic link to `target` (note the argument
        order, the reverse of os.symlink())."""
        raise NotImplementedError

    @abstractmethod
    def mkdir(self):
        """Create a directory at this path."""
        raise NotImplementedError

    @abstractmethod
    def __open_writer__(self, mode):
        """Open the file for writing in binary mode ('w', 'a' or 'x')."""
        raise NotImplementedError

    def write_bytes(self, data):
        """Replace the contents of the file with the bytes-like `data`;
        returns the number of bytes written."""
        view = memoryview(data)   # check the type before truncating
        with vfsopen(self, mode='wb') as stream:
            return stream.write(view)

    def write_text(self, data, encoding=None, errors=None, newline=None):
        """Replace the contents of the file with the string `data`;
        returns the number of characters written."""
        encoding = io.text_encoding(encoding)
        if not isinstance(data, str):
            raise TypeError(f'data must be str, not {type(data).__name__}')
        with vfsopen(self, mode='w', encoding=encoding, errors=errors,
                     newline=newline) as stream:
            return stream.write(data)

    def _copy_from(self, source, follow_symlinks=True):
        work = [(source, self)]
        while work:
            src, dst = work.pop()
            if not follow_symlinks and src.info.is_symlink():
                dst.symlink_to(vfspath(src.readlink()), src.info.is_dir())
            elif src.info.is_dir():
                children = src.iterdir()
                dst.mkdir()
                work.extend((child, dst.joinpath(child.name)) for child in children)
            else:
                with vfsopen(src, 'rb') as reader, vfsopen(dst, 'wb') as writer:
                    while chunk := reader.read(1 << 20):
                        writer.write(chunk)
