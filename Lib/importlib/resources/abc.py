"""Abstract interfaces of importlib.resources."""

import abc
import os
import pathlib
from typing import BinaryIO, Iterable, Iterator, NoReturn, Text, Optional
from typing import runtime_checkable, Protocol

__all__ = ["ResourceReader", "Traversable", "TraversableResources"]


class ResourceReader(metaclass=abc.ABCMeta):
    """Interface of loaders that can read package resources (legacy)."""

    @abc.abstractmethod
    def open_resource(self, resource: Text) -> BinaryIO:
        raise FileNotFoundError

    @abc.abstractmethod
    def resource_path(self, resource: Text) -> Text:
        raise FileNotFoundError

    @abc.abstractmethod
    def is_resource(self, path: Text) -> bool:
        raise FileNotFoundError

    @abc.abstractmethod
    def contents(self) -> Iterable[str]:
        raise FileNotFoundError


class TraversalError(Exception):
    pass


@runtime_checkable
class Traversable(Protocol):
    """A path-like object (a subset of pathlib.Path) in a resource tree."""

    @abc.abstractmethod
    def iterdir(self) -> Iterator["Traversable"]:
        """The entries of this directory."""

    def read_bytes(self) -> bytes:
        with self.open('rb') as strm:
            return strm.read()

    def read_text(self, encoding: Optional[str] = None) -> str:
        with self.open(encoding=encoding) as strm:
            return strm.read()

    @abc.abstractmethod
    def is_dir(self) -> bool:
        """True for a directory."""

    @abc.abstractmethod
    def is_file(self) -> bool:
        """True for a file."""

    def joinpath(self, *descendants) -> "Traversable":
        """The descendant named by the path segments."""
        if not descendants:
            return self
        names = []
        for d in descendants:
            names.extend(str(d).replace('\\', '/').split('/'))
        target = names[0]
        rest = names[1:]
        for child in self.iterdir():
            if child.name == target:
                return child.joinpath(*rest)
        raise TraversalError("Target not found during traversal.", target, names)

    def __truediv__(self, child) -> "Traversable":
        return self.joinpath(child)

    @abc.abstractmethod
    def open(self, mode='r', *args, **kwargs):
        """Open the file (text or binary as for pathlib.Path.open())."""

    @property
    @abc.abstractmethod
    def name(self) -> str:
        """The last path component."""


class TraversableResources(ResourceReader):
    """A ResourceReader based on a Traversable (files())."""

    @abc.abstractmethod
    def files(self) -> "Traversable":
        """The root of the resources."""

    def open_resource(self, resource):
        return self.files().joinpath(resource).open('rb')

    def resource_path(self, resource):
        raise FileNotFoundError(resource)

    def is_resource(self, path):
        return self.files().joinpath(path).is_file()

    def contents(self):
        return (item.name for item in self.files().iterdir())
