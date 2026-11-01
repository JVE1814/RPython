"""Compatibility location of the standard resource readers; they live in
importlib.resources.readers."""

from .resources.readers import (
    FileReader,
    MultiplexedPath,
    NamespaceReader,
    ZipReader,
)

__all__ = ['FileReader', 'ZipReader', 'MultiplexedPath', 'NamespaceReader']
