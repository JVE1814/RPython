"""Importers, loaders and related constants of the import system."""

from ._bootstrap import ModuleSpec
from ._bootstrap import BuiltinImporter
from ._bootstrap import FrozenImporter
from ._bootstrap_external import (SOURCE_SUFFIXES, BYTECODE_SUFFIXES,
                                  EXTENSION_SUFFIXES)
from ._bootstrap_external import PathFinder
from ._bootstrap_external import FileFinder
from ._bootstrap_external import SourceFileLoader
from ._bootstrap_external import SourcelessFileLoader
from ._bootstrap_external import ExtensionFileLoader
from ._bootstrap_external import NamespaceLoader


def all_suffixes():
    """Every file suffix the default finders recognise as a module."""
    return SOURCE_SUFFIXES + BYTECODE_SUFFIXES + EXTENSION_SUFFIXES


__all__ = ['BYTECODE_SUFFIXES', 'BuiltinImporter',
           'EXTENSION_SUFFIXES', 'ExtensionFileLoader', 'FileFinder',
           'FrozenImporter', 'ModuleSpec', 'NamespaceLoader', 'PathFinder',
           'SOURCE_SUFFIXES', 'SourceFileLoader', 'SourcelessFileLoader',
           'all_suffixes']
