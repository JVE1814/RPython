"""Resource readers used by the standard loaders (file system, zip
archives and namespace packages)."""

import collections
import os
import pathlib
import warnings
import zipfile

from . import abc

__all__ = ['FileReader', 'ZipReader', 'MultiplexedPath', 'NamespaceReader']


def remove_duplicates(items):
    """Return *items* without repeats, keeping first-seen order."""
    return iter(collections.OrderedDict.fromkeys(items))


class FileReader(abc.TraversableResources):
    """Resources of a module loaded from a file: its containing directory."""

    def __init__(self, loader):
        self.path = pathlib.Path(loader.path).parent

    def resource_path(self, resource):
        """Return the file system path of *resource* (existing or not)."""
        return str(self.path.joinpath(resource))

    def files(self):
        return self.path


class ZipReader(abc.TraversableResources):
    """Resources of a module imported from a zip archive by zipimport."""

    def __init__(self, loader, module):
        prefix = loader.prefix.replace('\\', '/')
        if loader.is_package(module):
            prefix += module.rpartition('.')[2] + '/'
        self.prefix = prefix
        self.archive = loader.archive

    def open_resource(self, resource):
        try:
            return super().open_resource(resource)
        except KeyError as exc:
            raise FileNotFoundError(exc.args[0])

    def is_resource(self, path):
        target = self.files().joinpath(path)
        return target.is_file() and target.exists()

    def files(self):
        return zipfile.Path(self.archive, self.prefix)


class MultiplexedPath(abc.Traversable):
    """A read-only directory that merges several directories.

    Used for namespace packages, whose portions may live in different
    locations.  Children with the same name in more than one location are
    merged again if they are all directories; otherwise the first wins.
    """

    def __init__(self, *paths):
        self._paths = [_ensure_traversable(p) for p in remove_duplicates(paths)]
        if not self._paths:
            raise FileNotFoundError('MultiplexedPath must contain at least one path')
        for p in self._paths:
            if not p.is_dir():
                raise NotADirectoryError('MultiplexedPath only supports directories')

    def iterdir(self):
        by_name = {}
        for location in self._paths:
            for child in location.iterdir():
                by_name.setdefault(child.name, []).append(child)
        for name in sorted(by_name):
            yield self._merge(by_name[name])

    @staticmethod
    def _merge(children):
        if len(children) == 1:
            return children[0]
        if all(c.is_dir() for c in children):
            return MultiplexedPath(*children)
        # a mix of files and directories: the first file wins
        return next(c for c in children if not c.is_dir())

    def read_bytes(self):
        raise FileNotFoundError(f'{self} is not a file')

    def read_text(self, *args, **kwargs):
        raise FileNotFoundError(f'{self} is not a file')

    def is_dir(self):
        return True

    def is_file(self):
        return False

    def joinpath(self, *descendants):
        try:
            return super().joinpath(*descendants)
        except abc.TraversalError:
            # Nothing matched: point at a (non-existent) child of the
            # first location so that callers get a path to report.
            return self._paths[0].joinpath(*descendants)

    @classmethod
    def _follow(cls, children):
        return cls._merge(list(children))

    def open(self, *args, **kwargs):
        raise FileNotFoundError(f'{self} is not a file')

    @property
    def name(self):
        return self._paths[0].name

    def __repr__(self):
        inner = ', '.join(f"'{p}'" for p in self._paths)
        return f'MultiplexedPath({inner})'


def _ensure_traversable(path):
    """Convert a str to pathlib.Path (deprecated); pass Traversables on."""
    if not isinstance(path, str):
        return path
    warnings.warn(
        "Constructing MultiplexedPath with strings is deprecated. "
        "Use Traversable objects instead.",
        DeprecationWarning,
        stacklevel=3,
    )
    return pathlib.Path(path)


class NamespaceReader(abc.TraversableResources):
    """Resources of a namespace package: all of its portions merged."""

    def __init__(self, namespace_path):
        if 'NamespacePath' not in str(namespace_path):
            raise ValueError('Invalid path')
        locations = [self._resolve(entry) for entry in namespace_path]
        self.path = MultiplexedPath(*[loc for loc in locations if loc])

    @classmethod
    def _resolve(cls, path_str):
        """Turn one ``__path__`` entry into a Traversable (a directory on
        disk or a directory inside a zip archive); return None for entries
        that are neither (e.g. path-hook sentinels)."""
        if not isinstance(path_str, (str, os.PathLike)):
            return None
        candidates = list(cls._candidate_paths(os.fspath(path_str)))
        for candidate in candidates:
            if candidate.is_dir():
                return candidate
        return None

    @staticmethod
    def _candidate_paths(path_str):
        plain = pathlib.Path(path_str)
        yield plain
        # Walk up looking for a zip archive containing the rest of the path.
        for parent in plain.parents:
            if parent.is_file():
                inner = plain.relative_to(parent).as_posix()
                try:
                    yield zipfile.Path(str(parent), inner.rstrip('/') + '/')
                except Exception:
                    pass
                return
            if parent.is_dir():
                return

    def resource_path(self, resource):
        """Return the file system path of *resource* in the first portion."""
        return str(self.path.joinpath(resource))

    def files(self):
        return self.path
