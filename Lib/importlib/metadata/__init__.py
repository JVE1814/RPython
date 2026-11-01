"""Access the metadata of installed distribution packages.

``version(name)``, ``metadata(name)``, ``entry_points(**selection)``,
``files(name)``, ``requires(name)`` and ``packages_distributions()`` query
the distributions found by the finders on ``sys.meta_path`` (by default:
``*.dist-info`` and ``*.egg-info`` metadata on ``sys.path``, including
inside zip archives).
"""

from __future__ import annotations

import abc
import collections
import functools
import itertools
import json
import operator
import os
import pathlib
import posixpath
import re
import sys
import types
import zipfile
from importlib import import_module
from importlib.abc import MetaPathFinder
from typing import Any, Iterable, List, Mapping, Match, Optional

from . import _meta
from ._collections import FreezableDefaultDict, Pair
from ._functools import method_cache
from ._itertools import always_iterable, unique_everseen
from ._meta import PackageMetadata, SimplePath

__all__ = [
    'Distribution',
    'DistributionFinder',
    'PackageMetadata',
    'PackageNotFoundError',
    'MetadataNotFound',
    'SimplePath',
    'distribution',
    'distributions',
    'entry_points',
    'files',
    'metadata',
    'packages_distributions',
    'requires',
    'version',
]


class PackageNotFoundError(ModuleNotFoundError):
    """The requested distribution package is not installed (or has no
    metadata)."""

    def __str__(self) -> str:
        return f"No package metadata was found for {self.name}"

    @property
    def name(self) -> str:  # type: ignore[override]
        (name,) = self.args
        return name


class MetadataNotFound(FileNotFoundError):
    """A distribution was found but its metadata file is missing."""


class Sectioned:
    """Parser for INI-like text with optional ``[section]`` headers, as
    used by entry_points.txt and requires.txt.

    ``read()`` yields Pair(section, line) for each non-blank line;
    ``section_pairs()`` additionally parses the lines as 'name = value'
    and skips comments and lines outside a section.

    >>> text = '[sec]\\n# note\\na = 1\\n'
    >>> [tuple(p) for p in Sectioned.read(text)]
    [('sec', '# note'), ('sec', 'a = 1')]
    >>> [(p.name, tuple(p.value)) for p in Sectioned.section_pairs(text)]
    [('sec', ('a', '1'))]
    """

    @classmethod
    def section_pairs(cls, text):
        for item in cls.read(text, filter_=cls.valid):
            if item.name is None:
                continue
            yield item._replace(value=Pair.parse(item.value))

    @staticmethod
    def read(text, filter_=None):
        section = None
        for raw in text.splitlines():
            line = raw.strip()
            if filter_ is None:
                if not line:
                    continue
            elif not filter_(line):
                continue
            if line.startswith('[') and line.endswith(']'):
                section = line.strip('[]')
                continue
            yield Pair(section, line)

    @staticmethod
    def valid(line: str):
        return bool(line) and not line.startswith('#')


class EntryPoint:
    """An entry point: a named reference to an object in a module.

    ``value`` has the form ``module[:attr.path] [extras]``.  Instances are
    immutable and hashable; ``dist`` names the providing Distribution when
    the entry point was discovered from one.
    """

    pattern = re.compile(
        r'(?P<module>[\w.]+)\s*'
        r'(:\s*(?P<attr>[\w.]+)\s*)?'
        r'((?P<extras>\[.*\])\s*)?$'
    )

    name: str
    value: str
    group: str
    dist: Optional[Distribution] = None

    def __init__(self, name: str, value: str, group: str) -> None:
        vars(self).update(name=name, value=value, group=group)

    def _match(self) -> Match:
        match = self.pattern.match(self.value)
        if match is None:
            raise ValueError(
                f"Entry point value {self.value!r} is not of the form "
                "'module[:attr] [extras]'"
            )
        return match

    def load(self) -> Any:
        """Import the module and return the referenced object."""
        match = self._match()
        obj = import_module(match.group('module'))
        for name in (match.group('attr') or '').split('.'):
            if name:
                obj = getattr(obj, name)
        return obj

    @property
    def module(self) -> str:
        return self._match().group('module')

    @property
    def attr(self) -> str:
        return self._match().group('attr')

    @property
    def extras(self) -> List[str]:
        return re.findall(r'\w+', self._match().group('extras') or '')

    def _for(self, dist):
        vars(self).update(dist=dist)
        return self

    def matches(self, **params):
        """True if every given attribute equals the given value.

        >>> ep = EntryPoint(group='g', name='n', value='m:a [x]')
        >>> ep.matches(group='g', extras=['x'])
        True
        >>> ep.matches(name='other')
        False
        """
        return all(getattr(self, key) == wanted for key, wanted in params.items())

    def _key(self):
        return self.name, self.value, self.group

    def __lt__(self, other):
        return self._key() < other._key()

    def __eq__(self, other):
        if not isinstance(other, EntryPoint):
            return NotImplemented
        return self._key() == other._key()

    def __setattr__(self, name, value):
        raise AttributeError("EntryPoint objects are immutable.")

    def __delattr__(self, name):
        raise AttributeError("EntryPoint objects are immutable.")

    def __repr__(self):
        return (
            f'EntryPoint(name={self.name!r}, value={self.value!r}, '
            f'group={self.group!r})'
        )

    def __hash__(self) -> int:
        return hash(self._key())

    def __reduce__(self):
        return (self.__class__, self._key())


class EntryPoints(tuple):
    """An immutable collection of EntryPoint objects, indexable by name."""

    __slots__ = ()

    def __getitem__(self, name: str) -> EntryPoint:  # type: ignore[override]
        """Return the entry point called *name*; raise KeyError if absent."""
        for ep in self:
            if ep.name == name:
                return ep
        raise KeyError(name)

    def __repr__(self):
        return f'{self.__class__.__name__}({tuple(self)!r})'

    def select(self, **params) -> EntryPoints:
        """Return the entry points matching all of *params*
        (see EntryPoint.matches)."""
        return EntryPoints(ep for ep in self if ep.matches(**params))

    @property
    def names(self) -> set[str]:
        return {ep.name for ep in self}

    @property
    def groups(self) -> set[str]:
        return {ep.group for ep in self}

    @classmethod
    def _from_text_for(cls, text, dist):
        return cls(ep._for(dist) for ep in cls._from_text(text))

    @staticmethod
    def _from_text(text):
        for item in Sectioned.section_pairs(text or ''):
            yield EntryPoint(name=item.value.name, value=item.value.value, group=item.name)


class PackagePath(pathlib.PurePosixPath):
    """A path of a file listed in a distribution's RECORD (relative to the
    distribution's root), with optional ``hash`` and ``size``."""

    hash: Optional[FileHash]
    size: int
    dist: Distribution

    def read_text(self, encoding: str = 'utf-8') -> str:
        return self.locate().read_text(encoding=encoding)

    def read_binary(self) -> bytes:
        return self.locate().read_bytes()

    def locate(self) -> SimplePath:
        """Return a concrete path to this file."""
        return self.dist.locate_file(self)


class FileHash:
    def __init__(self, spec: str) -> None:
        self.mode, _, self.value = spec.partition('=')

    def __repr__(self) -> str:
        return f'<FileHash mode: {self.mode} value: {self.value}>'


def _split_record_line(line):
    """Split one CSV line of a RECORD file (handles quoted fields)."""
    fields = []
    current = []
    quoted = False
    pos = 0
    length = len(line)
    while pos < length:
        char = line[pos]
        if quoted:
            if char == '"':
                if pos + 1 < length and line[pos + 1] == '"':
                    current.append('"')
                    pos += 1
                else:
                    quoted = False
            else:
                current.append(char)
        elif char == '"':
            quoted = True
        elif char == ',':
            fields.append(''.join(current))
            current = []
        else:
            current.append(char)
        pos += 1
    fields.append(''.join(current))
    return fields


def _module_name_of(path):
    """The importable module name of a file path, or None
    (like inspect.getmodulename)."""
    from importlib.machinery import all_suffixes

    filename = posixpath.basename(str(path))
    for suffix in sorted(all_suffixes(), key=len, reverse=True):
        if filename.endswith(suffix) and len(filename) > len(suffix):
            return filename[: -len(suffix)]
    return None


class Distribution(metaclass=abc.ABCMeta):
    """An installed distribution package and its metadata.

    Subclasses implement ``read_text`` and ``locate_file``.
    """

    @abc.abstractmethod
    def read_text(self, filename) -> Optional[str]:
        """Return the text of the metadata file *filename* (e.g.
        'METADATA', 'entry_points.txt'), or None if it does not exist."""

    @abc.abstractmethod
    def locate_file(self, path: str | os.PathLike[str]) -> SimplePath:
        """Return a path to *path*, a file relative to the distribution's
        root (the directory containing its metadata directory)."""

    @classmethod
    def from_name(cls, name: str) -> Distribution:
        """Return the Distribution called *name* (name normalisation per
        PEP 503 applies).  Raise PackageNotFoundError if not found."""
        if not name:
            raise ValueError("A distribution name is required.")
        fallback = None
        for dist in cls.discover(name=name):
            if dist._metadata_text() is not None:
                return dist
            if fallback is None:
                fallback = dist
        if fallback is not None:
            return fallback
        raise PackageNotFoundError(name)

    @classmethod
    def discover(
        cls, *, context: Optional[DistributionFinder.Context] = None, **kwargs
    ) -> Iterable[Distribution]:
        """Yield the distributions found by all finders for *context*
        (or for a Context built from **kwargs, e.g. name=, path=)."""
        if context and kwargs:
            raise ValueError("cannot accept context and kwargs")
        context = context or DistributionFinder.Context(**kwargs)
        return itertools.chain.from_iterable(
            finder(context) for finder in cls._discover_resolvers()
        )

    @staticmethod
    def at(path: str | os.PathLike[str]) -> Distribution:
        """Return the Distribution whose metadata directory is *path*."""
        return PathDistribution(pathlib.Path(path))

    @staticmethod
    def _discover_resolvers():
        found = []
        for finder in sys.meta_path:
            method = getattr(finder, 'find_distributions', None)
            if method is not None:
                found.append(method)
        return found

    def _metadata_text(self):
        return (
            self.read_text('METADATA')
            or self.read_text('PKG-INFO')
            # an egg-info *file* is itself the metadata
            or self.read_text('')
        )

    @property
    def metadata(self) -> _meta.PackageMetadata:
        """The parsed core metadata (METADATA or PKG-INFO).

        Raise MetadataNotFound if the distribution has no metadata file.
        """
        from . import _adapters

        text = self._metadata_text()
        if text is None:
            raise MetadataNotFound()
        return _adapters.Message.from_text(text)

    @property
    def name(self) -> str:
        """The 'Name' field of the metadata."""
        return self.metadata['Name']

    @property
    def _normalized_name(self):
        return Prepared.normalize(self.name)

    @property
    def version(self) -> str:
        """The 'Version' field of the metadata."""
        return self.metadata['Version']

    @property
    def entry_points(self) -> EntryPoints:
        """The entry points declared in entry_points.txt."""
        return EntryPoints._from_text_for(self.read_text('entry_points.txt'), self)

    @property
    def files(self) -> Optional[List[PackagePath]]:
        """The files of this distribution (from RECORD, or for legacy eggs
        installed-files.txt / SOURCES.txt), restricted to existing ones.
        None if no file list is available."""
        lines = (
            self._read_files_distinfo()
            or self._read_files_egginfo_installed()
            or self._read_files_egginfo_sources()
        )
        if lines is None:
            return None
        result = []
        for line in lines:
            if not line.strip():
                continue
            fields = _split_record_line(line)
            fields += [''] * (3 - len(fields))
            path = PackagePath(fields[0])
            path.hash = FileHash(fields[1]) if fields[1] else None
            path.size = int(fields[2]) if fields[2] else None
            path.dist = self
            if path.locate().exists():
                result.append(path)
        return result

    def _read_files_distinfo(self):
        text = self.read_text('RECORD')
        return text and text.splitlines()

    def _read_files_egginfo_installed(self):
        """installed-files.txt (written by pip for legacy installs) lists
        paths relative to the egg-info directory; rebase them on the
        distribution root."""
        text = self.read_text('installed-files.txt')
        info_dir = getattr(self, '_path', None)
        if not text or info_dir is None:
            return None
        root = pathlib.Path(self.locate_file('')).resolve()
        lines = []
        for name in text.splitlines():
            if not name.strip():
                continue
            target = pathlib.Path(str(info_dir), name).resolve()
            rel = target.relative_to(root, walk_up=True)
            lines.append(_quote(rel.as_posix()))
        return lines

    def _read_files_egginfo_sources(self):
        """SOURCES.txt (from the sdist) as a last-resort file list."""
        text = self.read_text('SOURCES.txt')
        return text and [_quote(line) for line in text.splitlines()]

    @property
    def requires(self) -> Optional[List[str]]:
        """The requirements of this distribution (PEP 508 strings)."""
        reqs = self._read_dist_info_reqs() or self._read_egg_info_reqs()
        return reqs and list(reqs)

    def _read_dist_info_reqs(self):
        return self.metadata.get_all('Requires-Dist')

    def _read_egg_info_reqs(self):
        source = self.read_text('requires.txt')
        if source is None:
            return None
        return self._deps_from_requires_text(source)

    @classmethod
    def _deps_from_requires_text(cls, source):
        return cls._convert_egg_info_reqs_to_simple_reqs(Sectioned.read(source))

    @staticmethod
    def _convert_egg_info_reqs_to_simple_reqs(sections):
        """Turn requires.txt entries (whose section names encode
        'extra:marker') into PEP 508 strings with environment markers."""
        for item in sections:
            extra, _, marker = (item.name or '').partition(':')
            conditions = []
            if marker:
                conditions.append(f'({marker})' if extra else marker)
            if extra:
                conditions.append(f'extra == "{extra}"')
            requirement = item.value
            if conditions:
                # PEP 508: a URL requirement needs whitespace before ';'
                if '@' in requirement:
                    requirement += ' '
                requirement += '; ' + ' and '.join(conditions)
            yield requirement

    def _load_json(self, filename):
        text = self.read_text(filename)
        if text is None:
            return None
        return json.loads(text, object_hook=lambda data: types.SimpleNamespace(**data))

    @property
    def origin(self):
        """The contents of direct_url.json (PEP 610) as nested namespaces,
        or None."""
        return self._load_json('direct_url.json')


def _quote(name):
    return '"' + name.replace('"', '""') + '"'


class DistributionFinder(MetaPathFinder):
    """A MetaPathFinder that can also locate distributions."""

    class Context:
        """Search criteria for ``find_distributions``.

        ``name`` restricts the search to one distribution (None means
        all); ``path`` is the list of directories to search (defaults to
        sys.path).  Finders may honour other attributes.
        """

        name = None

        def __init__(self, **kwargs):
            vars(self).update(kwargs)

        @property
        def path(self) -> List[str]:
            return vars(self).get('path', sys.path)

    @abc.abstractmethod
    def find_distributions(self, context=Context()) -> Iterable[Distribution]:
        """Return an iterable of the Distributions matching *context*."""


class FastPath:
    """One sys.path entry (a directory or a zip archive) with a cached
    index of the metadata directories it contains.

    Instances are cached per root; the cache is reset in forked children.
    """

    @functools.lru_cache()  # type: ignore[misc]
    def __new__(cls, root):
        return super().__new__(cls)

    def __init__(self, root):
        self.root = root

    def joinpath(self, child):
        return pathlib.Path(self.root, child)

    def children(self):
        try:
            return os.listdir(self.root or '.')
        except Exception:
            pass
        try:
            return self.zip_children()
        except Exception:
            return []

    def zip_children(self):
        archive = zipfile.Path(self.root)
        names = archive.root.namelist()
        self.joinpath = archive.joinpath
        return list(dict.fromkeys(name.split('/', 1)[0] for name in names))

    def search(self, name):
        return self.lookup(self.mtime).search(name)

    @property
    def mtime(self):
        try:
            return os.stat(self.root).st_mtime
        except OSError:
            self.lookup.cache_clear()
            return None

    @method_cache
    def lookup(self, mtime):
        return Lookup(self)


if hasattr(os, 'register_at_fork'):
    os.register_at_fork(after_in_child=FastPath.__new__.cache_clear)


class Lookup:
    """Index of the ``*.dist-info``/``*.egg-info`` entries (and eggs'
    EGG-INFO) below one FastPath, keyed by normalised name."""

    def __init__(self, path: FastPath):
        root_name = os.path.basename(path.root).lower()
        in_egg = root_name.endswith('.egg')
        self.infos = FreezableDefaultDict(list)
        self.eggs = FreezableDefaultDict(list)

        for child in path.children():
            lowered = child.lower()
            if lowered.endswith(('.dist-info', '.egg-info')):
                stem = lowered.rpartition('.')[0]
                dist_name = stem.partition('-')[0]
                self.infos[Prepared.normalize(dist_name)].append(path.joinpath(child))
            elif in_egg and lowered == 'egg-info':
                dist_name = root_name.rpartition('.')[0].partition('-')[0]
                self.eggs[Prepared.legacy_normalize(dist_name)].append(
                    path.joinpath(child)
                )

        self.infos.freeze()
        self.eggs.freeze()

    def search(self, prepared: Prepared):
        if prepared:
            infos = self.infos[prepared.normalized]
            eggs = self.eggs[prepared.legacy_normalized]
        else:
            infos = itertools.chain.from_iterable(self.infos.values())
            eggs = itertools.chain.from_iterable(self.eggs.values())
        return itertools.chain(infos, eggs)


class Prepared:
    """A distribution name together with its normalised forms."""

    normalized = None
    legacy_normalized = None

    def __init__(self, name: Optional[str]):
        self.name = name
        if name is None:
            return
        self.normalized = self.normalize(name)
        self.legacy_normalized = self.legacy_normalize(name)

    @staticmethod
    def normalize(name):
        """PEP 503 normalisation, with '_' as the separator.

        >>> Prepared.normalize('Foo.Bar--baz')
        'foo_bar_baz'
        """
        return re.sub(r'[-_.]+', '_', name).lower()

    @staticmethod
    def legacy_normalize(name):
        """The looser normalisation used for egg names."""
        return name.lower().replace('-', '_')

    def __bool__(self):
        return bool(self.name)


class MetadataPathFinder(DistributionFinder):
    """Finds distributions from metadata directories on ``path``
    (default: sys.path).  Used by importlib.machinery.PathFinder."""

    @classmethod
    def find_distributions(
        cls, context=DistributionFinder.Context()
    ) -> Iterable[PathDistribution]:
        found = cls._search_paths(context.name, context.path)
        return map(PathDistribution, found)

    @classmethod
    def _search_paths(cls, name, paths):
        prepared = Prepared(name)
        return itertools.chain.from_iterable(
            FastPath(entry).search(prepared) for entry in paths
        )

    @classmethod
    def invalidate_caches(cls) -> None:
        FastPath.__new__.cache_clear()


class PathDistribution(Distribution):
    """A distribution whose metadata directory is a (pathlib or zipfile)
    path."""

    def __init__(self, path: SimplePath) -> None:
        self._path = path

    def read_text(self, filename: str | os.PathLike[str]) -> Optional[str]:
        try:
            return self._path.joinpath(filename).read_text(encoding='utf-8')
        except (
            FileNotFoundError,
            IsADirectoryError,
            KeyError,
            NotADirectoryError,
            PermissionError,
        ):
            return None

    read_text.__doc__ = Distribution.read_text.__doc__

    def locate_file(self, path: str | os.PathLike[str]) -> SimplePath:
        return self._path.parent / path

    @property
    def _normalized_name(self):
        stem = os.path.basename(str(self._path))
        from_stem = self._name_from_stem(stem)
        if from_stem is not None:
            return Prepared.normalize(from_stem)
        return super()._normalized_name

    @staticmethod
    def _name_from_stem(stem):
        """The distribution name encoded in a metadata directory name.

        >>> PathDistribution._name_from_stem('foo-3.0.egg-info')
        'foo'
        >>> PathDistribution._name_from_stem('CherryPy-3.0.dist-info')
        'CherryPy'
        >>> PathDistribution._name_from_stem('face.egg-info')
        'face'
        >>> PathDistribution._name_from_stem('foo.bar')
        """
        base, ext = os.path.splitext(stem)
        if ext not in ('.dist-info', '.egg-info'):
            return None
        return base.partition('-')[0]


def distribution(distribution_name: str) -> Distribution:
    """Return the Distribution for the named package."""
    return Distribution.from_name(distribution_name)


def distributions(**kwargs) -> Iterable[Distribution]:
    """Return all distributions (``name=``/``path=`` narrow the search)."""
    return Distribution.discover(**kwargs)


def metadata(distribution_name: str) -> _meta.PackageMetadata:
    """Return the metadata of the named package."""
    return Distribution.from_name(distribution_name).metadata


def version(distribution_name: str) -> str:
    """Return the version string of the named package."""
    return distribution(distribution_name).version


_unique = functools.partial(
    unique_everseen,
    key=operator.attrgetter('_normalized_name'),
)
"""Filter distributions, keeping the first one seen for each normalised
name."""


def entry_points(**params) -> EntryPoints:
    """Return the entry points of all installed distributions, optionally
    filtered by ``group=``, ``name=`` etc. (see EntryPoints.select).

    Only the first distribution of a given (normalised) name contributes.
    """
    found = itertools.chain.from_iterable(
        dist.entry_points for dist in _unique(distributions())
    )
    return EntryPoints(found).select(**params)


def files(distribution_name: str) -> Optional[List[PackagePath]]:
    """Return the files belonging to the named package."""
    return distribution(distribution_name).files


def requires(distribution_name: str) -> Optional[List[str]]:
    """Return the requirements declared by the named package."""
    return distribution(distribution_name).requires


def packages_distributions() -> Mapping[str, List[str]]:
    """Map top-level import names to the distributions providing them.

    >>> import collections.abc
    >>> pkgs = packages_distributions()
    >>> all(isinstance(dist, collections.abc.Sequence) for dist in pkgs.values())
    True
    """
    mapping = collections.defaultdict(list)
    for dist in distributions():
        for import_name in _top_level_declared(dist) or _top_level_inferred(dist):
            mapping[import_name].append(dist.metadata['Name'])
    return dict(mapping)


def _top_level_declared(dist):
    return (dist.read_text('top_level.txt') or '').split()


def _top_level_name(path: PackagePath) -> str:
    parts = path.parts
    if len(parts) > 1:
        return parts[0]
    return _module_name_of(path) or str(path)


def _top_level_inferred(dist):
    names = {_top_level_name(path) for path in always_iterable(dist.files)}
    return [name for name in names if '.' not in name]
