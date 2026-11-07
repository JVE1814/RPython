"""Importers, loaders and related constants of the import system."""

from ._bootstrap import ModuleSpec
from ._bootstrap import BuiltinImporter
from ._bootstrap import FrozenImporter
from ._bootstrap_external import (SOURCE_SUFFIXES, BYTECODE_SUFFIXES,
                                  EXTENSION_SUFFIXES)
from ._bootstrap_external import WindowsRegistryFinder
from ._bootstrap_external import PathFinder
from ._bootstrap_external import FileFinder
from ._bootstrap_external import SourceFileLoader
from ._bootstrap_external import SourcelessFileLoader
from ._bootstrap_external import ExtensionFileLoader
from ._bootstrap_external import AppleFrameworkLoader
from ._bootstrap_external import NamespaceLoader


def all_suffixes():
    """Every file suffix the default finders recognise as a module."""
    return SOURCE_SUFFIXES + BYTECODE_SUFFIXES + EXTENSION_SUFFIXES


__all__ = ['AppleFrameworkLoader', 'BYTECODE_SUFFIXES', 'BuiltinImporter',
           'DEBUG_BYTECODE_SUFFIXES', 'EXTENSION_SUFFIXES',
           'ExtensionFileLoader', 'FileFinder', 'FrozenImporter', 'ModuleSpec',
           'NamespaceLoader', 'OPTIMIZED_BYTECODE_SUFFIXES', 'PathFinder',
           'SOURCE_SUFFIXES', 'SourceFileLoader', 'SourcelessFileLoader',
           'WindowsRegistryFinder', 'all_suffixes']


# Bytecode suffixes from before optimisation levels were part of the cache
# file name; both are plain '.pyc' now.
_DEPRECATED_SUFFIX_NAMES = ('DEBUG_BYTECODE_SUFFIXES', 'OPTIMIZED_BYTECODE_SUFFIXES')


def __getattr__(name):
    if name in _DEPRECATED_SUFFIX_NAMES:
        import warnings
        warnings.warn(f'importlib.machinery.{name} is deprecated; '
                      'use importlib.machinery.BYTECODE_SUFFIXES instead',
                      DeprecationWarning, stacklevel=2)
        return ['.pyc']
    raise AttributeError(f'module {__name__!r} has no attribute {name!r}')
