"""Core of importlib.resources: locating a package's resources and
materialising them on the file system."""

import contextlib
import functools
import importlib
import importlib.abc  # code using resources commonly expects importlib.abc loaded
import os
import pathlib
import sys
import tempfile
import types
import warnings
from typing import Optional, Union, cast

from .abc import ResourceReader, Traversable

Package = Union[types.ModuleType, str]
Anchor = Package

_PACKAGE_NAME = __spec__.parent if __spec__ is not None else __name__.rpartition('.')[0]


def package_to_anchor(func):
    """Accept the deprecated ``package=`` keyword in place of ``anchor``."""
    missing = object()

    @functools.wraps(func)
    def wrapper(anchor=missing, package=missing):
        if package is not missing:
            if anchor is not missing:
                return func(anchor, package)
            warnings.warn(
                "First parameter to files is renamed to 'anchor'",
                DeprecationWarning,
                stacklevel=2,
            )
            return func(package)
        if anchor is missing:
            return func()
        return func(anchor)

    return wrapper


@package_to_anchor
def files(anchor: Optional[Anchor] = None) -> Traversable:
    """Return a Traversable for the resources of *anchor* (a module or a
    module name).  Without an argument, the calling module is used."""
    return from_package(resolve(anchor))


def get_resource_reader(package: types.ModuleType) -> Optional[ResourceReader]:
    """Return the loader-provided resource reader of *package*, or None."""
    spec = package.__spec__
    loader = getattr(spec, 'loader', None)
    reader = getattr(loader, 'get_resource_reader', None)
    if reader is None:
        return None
    return reader(spec.name)


def _caller_module_name():
    """Name of the first module on the stack outside this package."""
    frame = sys._getframe(1)
    while frame is not None:
        name = frame.f_globals.get('__name__', '')
        if name != _PACKAGE_NAME and not name.startswith(_PACKAGE_NAME + '.'):
            return name
        frame = frame.f_back
    raise TypeError("unable to determine the calling module")


def resolve(cand: Optional[Anchor]) -> types.ModuleType:
    if cand is None:
        return importlib.import_module(_caller_module_name())
    if isinstance(cand, types.ModuleType):
        return cand
    return importlib.import_module(cand)


def get_package(package: Anchor) -> types.ModuleType:
    """Legacy helper: resolve *package* and require it to be a package."""
    module = resolve(package)
    if wrap_spec(module).submodule_search_locations is None:
        raise TypeError(f'{package!r} is not a package')
    return module


def from_package(package: types.ModuleType):
    """Return a Traversable for the resources of the module *package*."""
    from ._adapters import wrap_spec

    if package.__spec__ is None:
        raise TypeError(
            f"Cannot access resources for {package.__name__!r} as it does not "
            "appear to correspond to an importable module (its __spec__ is None)."
        )
    spec = wrap_spec(package)
    reader = spec.loader.get_resource_reader(spec.name)
    return reader.files()


def wrap_spec(package):
    from ._adapters import wrap_spec as _wrap

    return _wrap(package)


@contextlib.contextmanager
def _tempfile(reader, suffix='', *, _os_remove=os.remove):
    """Write ``reader()`` to a fresh temporary file and yield its Path; the
    file is removed on exit (it may already have been removed)."""
    fd, raw_path = tempfile.mkstemp(suffix=suffix)
    try:
        try:
            os.write(fd, reader())
        finally:
            os.close(fd)
        del reader
        yield pathlib.Path(raw_path)
    finally:
        try:
            _os_remove(raw_path)
        except FileNotFoundError:
            pass


def _temp_file(path):
    return _tempfile(path.read_bytes, suffix=path.name)


def _is_present_dir(path: Traversable) -> bool:
    """True if *path* is an existing directory (never raises)."""
    try:
        return path.is_dir()
    except Exception:
        return False


@functools.singledispatch
def as_file(path):
    """Context manager yielding a real file system path for the Traversable
    *path*.  Resources that are not on disk are copied to a temporary
    location for the duration of the ``with`` block."""
    if _is_present_dir(path):
        return _temp_dir(path)
    return _temp_file(path)


@as_file.register(pathlib.Path)
@contextlib.contextmanager
def _(path):
    """A pathlib.Path is already on the file system."""
    yield path


@contextlib.contextmanager
def _temp_path(dir: tempfile.TemporaryDirectory):
    with dir as name:
        yield pathlib.Path(name)


@contextlib.contextmanager
def _temp_dir(path):
    """Copy the directory tree *path* into a temporary directory."""
    assert path.is_dir()
    with _temp_path(tempfile.TemporaryDirectory()) as temp_dir:
        yield _write_contents(temp_dir, path)


def _write_contents(target, source):
    child = target.joinpath(source.name)
    if source.is_dir():
        child.mkdir()
        for item in source.iterdir():
            _write_contents(child, item)
    else:
        child.write_bytes(source.read_bytes())
    return child
