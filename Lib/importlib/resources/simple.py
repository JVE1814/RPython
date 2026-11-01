"""Building blocks for readers that only know how to list and open
resources: implement ``SimpleReader`` and mix in ``TraversableReader``."""

import abc
import io
import itertools
from typing import BinaryIO, List

from .abc import Traversable, TraversableResources


class SimpleReader(abc.ABC):
    """The minimal interface a resource provider must offer."""

    @property
    @abc.abstractmethod
    def package(self) -> str:
        """Dotted name of the package these resources belong to."""

    @abc.abstractmethod
    def children(self) -> List['SimpleReader']:
        """Readers for the sub-packages/directories of this one."""

    @abc.abstractmethod
    def resources(self) -> List[str]:
        """Names of the resources directly contained here."""

    @abc.abstractmethod
    def open_binary(self, resource: str) -> BinaryIO:
        """Open the resource *resource* for binary reading."""

    @property
    def name(self):
        return self.package.split('.')[-1]


class ResourceContainer(Traversable):
    """A Traversable directory backed by a SimpleReader."""

    def __init__(self, reader: SimpleReader):
        self.reader = reader

    def is_dir(self):
        return True

    def is_file(self):
        return False

    def iterdir(self):
        handles = (ResourceHandle(self, name) for name in self.reader.resources())
        containers = (ResourceContainer(child) for child in self.reader.children())
        return itertools.chain(handles, containers)

    def open(self, *args, **kwargs):
        raise IsADirectoryError()

    @property
    def name(self):
        return self.reader.name


class ResourceHandle(Traversable):
    """A Traversable file inside a ResourceContainer."""

    def __init__(self, parent: ResourceContainer, name: str):
        self.parent = parent
        self._name = name

    @property
    def name(self):
        return self._name

    def iterdir(self):
        return iter(())

    def is_file(self):
        return True

    def is_dir(self):
        return False

    def open(self, mode='r', *args, **kwargs):
        stream = self.parent.reader.open_binary(self.name)
        if 'b' not in mode:
            stream = io.TextIOWrapper(stream, *args, **kwargs)
        return stream

    def joinpath(self, name):
        raise RuntimeError("Cannot traverse into a resource")


class TraversableReader(TraversableResources, SimpleReader):
    """Mix-in deriving ``files()`` from the SimpleReader methods."""

    def files(self):
        return ResourceContainer(self)
