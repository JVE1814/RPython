"""Resource readers returned by the standard loaders' get_resource_reader()."""

import pathlib

from importlib.resources.abc import TraversableResources

__all__ = ['FileReader', 'NamespaceReader']


class FileReader(TraversableResources):
    """Resources of a package loaded from the file system."""

    def __init__(self, loader):
        self.path = pathlib.Path(loader.path).parent

    def resource_path(self, resource):
        return str(self.path.joinpath(resource))

    def files(self):
        return self.path


class NamespaceReader(TraversableResources):
    """Resources of a namespace package (spread over several directories)."""

    def __init__(self, namespace_path):
        if 'NamespacePath' not in str(type(namespace_path)) and not isinstance(
                namespace_path, (list, tuple)):
            raise ValueError('Invalid path')
        from importlib.resources import _MultiplexedPath
        self.path = _MultiplexedPath(*list(namespace_path))

    def resource_path(self, resource):
        return str(self.path.joinpath(resource))

    def files(self):
        return self.path
