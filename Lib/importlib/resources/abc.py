"""Abstract interfaces for package resources.

``ResourceReader`` is the original (legacy) loader-side protocol;
``Traversable`` is the path-like object returned by
``importlib.resources.files()``; ``TraversableResources`` adapts the former
to the latter.
"""

import abc
import os
import pathlib
from typing import Any, BinaryIO, Iterable, Iterator, NoReturn, Optional, Text, Union
from typing import Protocol, runtime_checkable

StrPath = Union[str, os.PathLike[str]]

__all__ = ["ResourceReader", "Traversable", "TraversableResources"]


class ResourceReader(metaclass=abc.ABCMeta):
    """Abstract base class for loaders that can provide resource access."""

    @abc.abstractmethod
    def open_resource(self, resource: Text) -> BinaryIO:
        """Return a binary file-like object opened for reading *resource*.

        Raise FileNotFoundError if the resource does not exist.
        """
        raise FileNotFoundError

    @abc.abstractmethod
    def resource_path(self, resource: Text) -> Text:
        """Return the file system path of *resource*.

        Raise FileNotFoundError if the resource has no concrete path.
        """
        raise FileNotFoundError

    @abc.abstractmethod
    def is_resource(self, path: Text) -> bool:
        """Return True if *path* names a resource (a file, not a directory)."""
        raise FileNotFoundError

    @abc.abstractmethod
    def contents(self) -> Iterable[str]:
        """Return an iterable of the names of entries in the package."""
        raise FileNotFoundError


class TraversalError(Exception):
    """Raised when a Traversable cannot locate a descendant."""


@runtime_checkable
class Traversable(Protocol):
    """A path-like object for navigating package resources.

    Implementations provide ``iterdir``, ``is_dir``, ``is_file``, ``open``
    and ``name``; the remaining operations are derived from those.
    """

    @abc.abstractmethod
    def iterdir(self) -> Iterator["Traversable"]:
        """Yield the Traversable children of this directory."""

    def read_bytes(self) -> bytes:
        """Return the whole content of this resource as bytes."""
        with self.open('rb') as stream:
            return stream.read()

    def read_text(
        self, encoding: Optional[str] = None, errors: Optional[str] = None
    ) -> str:
        """Return the whole content of this resource as text."""
        with self.open(encoding=encoding, errors=errors) as stream:
            return stream.read()

    @abc.abstractmethod
    def is_dir(self) -> bool:
        """Return True if this object is a directory."""

    @abc.abstractmethod
    def is_file(self) -> bool:
        """Return True if this object is a file."""

    def joinpath(self, *descendants: StrPath) -> "Traversable":
        """Return the Traversable reached by following *descendants*.

        Each descendant may itself contain '/'-separated segments.  Raise
        TraversalError when an intermediate name is not found.
        """
        if not descendants:
            return self
        segments = []
        for item in descendants:
            segments.extend(pathlib.PurePosixPath(item).parts)
        if not segments:
            return self
        wanted, remaining = segments[0], segments[1:]
        for child in self.iterdir():
            if child.name == wanted:
                return child.joinpath(*remaining)
        raise TraversalError("Target not found during traversal.", wanted, remaining)

    def __truediv__(self, child: StrPath) -> "Traversable":
        """Shorthand for ``joinpath(child)``."""
        return self.joinpath(child)

    @abc.abstractmethod
    def open(self, mode='r', *args, **kwargs):
        """Open the resource; *mode* is 'r' (text, the default) or 'rb'.

        Extra arguments are passed to io.TextIOWrapper in text mode.
        """

    @property
    @abc.abstractmethod
    def name(self) -> str:
        """The base name of this object without any parent references."""


class TraversableResources(ResourceReader):
    """A ResourceReader implemented on top of a Traversable (``files()``)."""

    @abc.abstractmethod
    def files(self) -> "Traversable":
        """Return a Traversable for the package's resources."""

    def open_resource(self, resource: StrPath) -> BinaryIO:
        return self.files().joinpath(resource).open('rb')

    def resource_path(self, resource: Any) -> NoReturn:
        raise FileNotFoundError(resource)

    def is_resource(self, path: StrPath) -> bool:
        return self.files().joinpath(path).is_file()

    def contents(self) -> Iterator[str]:
        return (entry.name for entry in self.files().iterdir())
