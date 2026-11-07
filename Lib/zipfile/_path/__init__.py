"""pathlib-style access to the members of a ZIP archive (zipfile.Path)."""

import contextlib
import io
import itertools
import posixpath
import re
import stat
import sys

import zipfile

from .glob import Translator

__all__ = ['Path']


def _parents(path):
    """Yield the ancestors of a '/'-separated path, nearest first.

    >>> list(_parents('a/b/c'))
    ['a/b', 'a']
    """
    while True:
        path = posixpath.dirname(path.rstrip('/'))
        if not path.strip('/'):  # nothing left but the (possibly doubled) root
            return
        yield path


def _ancestry(path):
    """Yield path and its ancestors (without trailing slashes)."""
    path = path.rstrip(posixpath.sep)
    while path.rstrip(posixpath.sep):
        yield path
        path, _ = posixpath.split(path)


def _difference(minuend, subtrahend):
    """Items of minuend not in subtrahend, preserving order."""
    excluded = set(subtrahend)
    return (x for x in minuend if x not in excluded)


class InitializedState:
    """Remember constructor arguments so subclasses pickle cleanly."""

    def __init__(self, *args, **kwargs):
        self.__args = args
        self.__kwargs = kwargs
        super().__init__(*args, **kwargs)

    def __getstate__(self):
        return self.__args, self.__kwargs

    def __setstate__(self, state):
        args, kwargs = state
        super().__init__(*args, **kwargs)


class CompleteDirs(InitializedState, zipfile.ZipFile):
    """ZipFile that also reports directories implied by member names
    (e.g. 'a/' for a member 'a/b.txt') even when they have no entry."""

    @staticmethod
    def _implied_dirs(names):
        seen = set()
        for name in names:
            for parent in _parents(name):
                d = parent + '/'
                if d not in seen:
                    seen.add(d)
                    yield d
        # 'seen' only grows by unique entries; filter out explicit ones below.

    def namelist(self):
        names = super().namelist()
        explicit = set(names)
        return names + [d for d in self._implied_dirs(names) if d not in explicit]

    def _name_set(self):
        return set(self.namelist())

    def resolve_dir(self, name):
        """'name' or 'name/' depending on which one is a directory here."""
        names = self._name_set()
        dirname = name + '/'
        return dirname if name not in names and dirname in names else name

    def getinfo(self, name):
        try:
            return super().getinfo(name)
        except KeyError:
            if not name.endswith('/') or name not in self._name_set():
                raise
            return zipfile.ZipInfo(filename=name)

    @classmethod
    def make(cls, source):
        """Wrap *source* (ZipFile or anything ZipFile accepts) so that it
        reports implied directories."""
        if isinstance(source, CompleteDirs):
            return source
        if not isinstance(source, zipfile.ZipFile):
            return cls(source)
        if 'r' not in source.mode:
            cls = CompleteDirs
        source.__class__ = cls
        return source

    @classmethod
    def inject(cls, zf):
        """Add explicit entries for implied directories to a writable
        archive *zf* and return it."""
        for name in cls._implied_dirs(zf.namelist()):
            if name not in zf.NameToInfo:
                zf.writestr(name, b"")
        return zf


class FastLookup(CompleteDirs):
    """CompleteDirs caching the name list (archive opened read-only)."""

    def namelist(self):
        with contextlib.suppress(AttributeError):
            return self.__names
        self.__names = super().namelist()
        return self.__names

    def _name_set(self):
        with contextlib.suppress(AttributeError):
            return self.__lookup
        self.__lookup = set(self.namelist())
        return self.__lookup


def _extract_text_encoding(encoding=None, *args, **kwargs):
    # stacklevel 3 points at the caller of Path.open()/read_text()
    return io.text_encoding(encoding, 3), args, kwargs


class Path:
    """A path inside a ZIP archive, with a subset of the pathlib API.

    Path(root, at='') where root is a ZipFile, a path to an archive or a
    file object, and *at* the member path ('' for the root, directories end
    with '/').
    """

    __repr = "{self.__class__.__name__}({self.root.filename!r}, {self.at!r})"

    def __init__(self, root, at=""):
        self.root = FastLookup.make(root)
        self.at = at

    def __eq__(self, other):
        if self.__class__ is not other.__class__:
            return NotImplemented
        return (self.root, self.at) == (other.root, other.at)

    def __hash__(self):
        return hash((self.root, self.at))

    def open(self, mode='r', *args, pwd=None, **kwargs):
        """Open the member; text modes accept the io.TextIOWrapper
        arguments, binary modes accept none."""
        if self.is_dir():
            raise IsADirectoryError(self)
        zip_mode = mode[0]
        if zip_mode == 'r' and not self.exists():
            raise FileNotFoundError(self)
        stream = self.root.open(self.at, zip_mode, pwd=pwd)
        if 'b' in mode:
            if args or kwargs:
                raise ValueError("encoding args invalid for binary operation")
            return stream
        encoding, args, kwargs = _extract_text_encoding(*args, **kwargs)
        return io.TextIOWrapper(stream, encoding, *args, **kwargs)

    def _base(self):
        import pathlib
        return pathlib.PurePosixPath(self.at or self.root.filename)

    @property
    def name(self):
        return self._base().name

    @property
    def suffix(self):
        return self._base().suffix

    @property
    def suffixes(self):
        return self._base().suffixes

    @property
    def stem(self):
        return self._base().stem

    @property
    def filename(self):
        import pathlib
        return pathlib.Path(self.root.filename).joinpath(self.at)

    def read_text(self, *args, **kwargs):
        encoding, args, kwargs = _extract_text_encoding(*args, **kwargs)
        with self.open('r', encoding, *args, **kwargs) as strm:
            return strm.read()

    def read_bytes(self):
        with self.open('rb') as strm:
            return strm.read()

    def _is_child(self, path):
        return posixpath.dirname(path.at.rstrip("/")) == self.at.rstrip("/")

    def _next(self, at):
        return self.__class__(self.root, at)

    def is_dir(self):
        return not self.at or self.at.endswith("/")

    def is_file(self):
        return self.exists() and not self.is_dir()

    def exists(self):
        return self.at in self.root._name_set()

    def iterdir(self):
        if not self.is_dir():
            raise ValueError("Can't listdir a file")
        subs = map(self._next, self.root.namelist())
        return filter(self._is_child, subs)

    def match(self, path_pattern):
        import pathlib
        return pathlib.PurePosixPath(self.at).match(path_pattern)

    def is_symlink(self):
        """True if the member's mode bits describe a symbolic link."""
        info = self.root.getinfo(self.at)
        mode = info.external_attr >> 16
        return stat.S_ISLNK(mode)

    def glob(self, pattern):
        if not pattern:
            raise ValueError(f"Unacceptable pattern: {pattern!r}")
        prefix = re.escape(self.at)
        tr = Translator(seps='/')
        matches = re.compile(prefix + tr.translate(pattern)).fullmatch
        return map(self._next, filter(matches, self.root.namelist()))

    def rglob(self, pattern):
        return self.glob(f'**/{pattern}')

    def relative_to(self, other, *extra):
        return posixpath.relpath(str(self), str(other.joinpath(*extra)))

    def __str__(self):
        # an archive without a file name (e.g. in memory) is shown as ":zipfile:"
        root = self.root.filename
        if root is None:
            return posixpath.join(":zipfile:", self.at) if self.at else ":zipfile:"
        return posixpath.join(root, self.at)

    def __repr__(self):
        return self.__repr.format(self=self)

    def joinpath(self, *other):
        next_at = posixpath.join(self.at, *other)
        return self._next(self.root.resolve_dir(next_at))

    __truediv__ = joinpath

    @property
    def parent(self):
        if not self.at:
            return self.filename.parent
        parent_at = posixpath.dirname(self.at.rstrip('/'))
        if parent_at:
            parent_at += '/'
        return self._next(parent_at)
