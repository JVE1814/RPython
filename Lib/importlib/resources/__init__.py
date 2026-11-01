"""Access to the data files ("resources") shipped inside packages.

files(anchor) returns a Traversable (usually a pathlib.Path) for a package
directory; as_file() turns any Traversable into a real file system path for
the duration of a with block.  The functional helpers (read_text(), ...)
take the package and the path components of the resource.
"""

import contextlib
import os
import pathlib
import sys
import tempfile
import types

from .abc import ResourceReader, Traversable, TraversableResources

__all__ = [
    'Anchor', 'Package', 'ResourceReader', 'as_file', 'contents', 'files',
    'is_resource', 'open_binary', 'open_text', 'path', 'read_binary',
    'read_text',
]

Package = types.ModuleType | str
Anchor = Package


def _resolve(anchor):
    if anchor is None:
        # the caller's module (two frames up: files() -> _resolve)
        name = sys._getframe(2).f_globals['__name__']
        import importlib
        return importlib.import_module(name)
    if isinstance(anchor, str):
        import importlib
        return importlib.import_module(anchor)
    return anchor


class _MultiplexedPath(Traversable):
    """The union of several directories (a namespace package)."""

    def __init__(self, *paths):
        self._paths = [pathlib.Path(p) for p in paths]
        if not self._paths:
            raise FileNotFoundError('MultiplexedPath must contain at least one path')

    def iterdir(self):
        seen = set()
        for p in self._paths:
            for child in p.iterdir():
                if child.name not in seen:
                    seen.add(child.name)
                    yield child

    def read_bytes(self):
        raise FileNotFoundError(f'{self} is not a file')

    def read_text(self, *args, **kwargs):
        raise FileNotFoundError(f'{self} is not a file')

    def is_dir(self):
        return True

    def is_file(self):
        return False

    def joinpath(self, *descendants):
        for p in self._paths:
            candidate = p.joinpath(*descendants)
            if candidate.exists():
                return candidate
        return self._paths[0].joinpath(*descendants)

    __truediv__ = joinpath

    def open(self, *args, **kwargs):
        raise FileNotFoundError(f'{self} is not a file')

    @property
    def name(self):
        return self._paths[0].name

    def __repr__(self):
        paths = ', '.join(f"'{p}'" for p in self._paths)
        return f'MultiplexedPath({paths})'


def _reader_files(module):
    spec = getattr(module, '__spec__', None)
    loader = getattr(spec, 'loader', None) or getattr(module, '__loader__', None)
    get_reader = getattr(loader, 'get_resource_reader', None)
    if get_reader is None:
        return None
    reader = get_reader(spec.name if spec is not None else module.__name__)
    if reader is None:
        return None
    if hasattr(reader, 'files'):
        return reader.files()
    return None


def files(anchor=None):
    """A Traversable for the directory of the package (or module) *anchor*."""
    module = _resolve(anchor)
    found = _reader_files(module)
    if found is not None:
        return found
    spec = getattr(module, '__spec__', None)
    locations = getattr(spec, 'submodule_search_locations', None)
    if spec is not None and spec.origin and spec.origin not in ('namespace', 'built-in', 'frozen'):
        return pathlib.Path(spec.origin).parent
    if locations:
        paths = list(locations)
        if len(paths) == 1:
            return pathlib.Path(paths[0])
        return _MultiplexedPath(*paths)
    origin = getattr(module, '__file__', None)
    if origin:
        return pathlib.Path(origin).parent
    raise TypeError(f'{module!r} has no file system location')


@contextlib.contextmanager
def as_file(path):
    """A real file system path for the Traversable *path* (a temporary copy
    when it does not live on the file system)."""
    if isinstance(path, pathlib.Path):
        yield path
        return
    if path.is_dir():
        with tempfile.TemporaryDirectory() as tmp:
            target = pathlib.Path(tmp) / path.name
            _copy_tree(path, target)
            yield target
        return
    fd, raw = tempfile.mkstemp(suffix=path.name)
    try:
        with os.fdopen(fd, 'wb') as f:
            f.write(path.read_bytes())
        yield pathlib.Path(raw)
    finally:
        try:
            os.remove(raw)
        except FileNotFoundError:
            pass


def _copy_tree(src, dst):
    dst.mkdir()
    for child in src.iterdir():
        if child.is_dir():
            _copy_tree(child, dst / child.name)
        else:
            (dst / child.name).write_bytes(child.read_bytes())


# ---------------------------------------------------------------- functional API

def _target(anchor, path_names):
    return files(anchor).joinpath(*path_names)


def open_binary(anchor, *path_names):
    """Open the resource for binary reading."""
    return _target(anchor, path_names).open('rb')


def open_text(anchor, *path_names, encoding='utf-8', errors='strict'):
    """Open the resource for text reading."""
    return _target(anchor, path_names).open('r', encoding=encoding, errors=errors)


def read_binary(anchor, *path_names):
    """The resource's contents as bytes."""
    return _target(anchor, path_names).read_bytes()


def read_text(anchor, *path_names, encoding='utf-8', errors='strict'):
    """The resource's contents as str."""
    with open_text(anchor, *path_names, encoding=encoding, errors=errors) as f:
        return f.read()


def path(anchor, *path_names):
    """Context manager giving a file system path to the resource."""
    return as_file(_target(anchor, path_names))


def is_resource(anchor, *path_names):
    """True if the resource exists and is a file."""
    return _target(anchor, path_names).is_file()


def contents(anchor, *path_names):
    """Names of the entries in a resource directory."""
    return [child.name for child in _target(anchor, path_names).iterdir()]
