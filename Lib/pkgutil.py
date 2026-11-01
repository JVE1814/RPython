"""Utilities for the import system: module discovery and resources.

iter_modules(), walk_packages()  list modules on a path / recursively
get_importer(), iter_importers()  path entry finders
get_data()                        read a resource of a package
extend_path()                     helper for pkgutil-style namespace packages
resolve_name()                    turn 'pkg.mod:attr' strings into objects
"""

import importlib
import importlib.machinery
import importlib.util
import os
import os.path
import re
import sys
from collections import namedtuple

__all__ = [
    'get_importer', 'iter_importers', 'walk_packages', 'iter_modules',
    'get_data', 'extend_path', 'ModuleInfo', 'resolve_name',
]

ModuleInfo = namedtuple('ModuleInfo', 'module_finder name ispkg')
ModuleInfo.__doc__ = 'A namedtuple with minimal info about a module.'


# ---------------------------------------------------------------- finders

def get_importer(path_item):
    """The path entry finder for `path_item` (cached in
    sys.path_importer_cache), or None if no path hook accepts it."""
    path_item = os.fsdecode(path_item)
    cache = sys.path_importer_cache
    if path_item in cache:
        return cache[path_item]
    for hook in sys.path_hooks:
        try:
            finder = hook(path_item)
        except ImportError:
            continue
        cache.setdefault(path_item, finder)
        return finder
    return None


def iter_importers(fullname=""):
    """Yield the finders that would be used to import `fullname`: the
    finders of the parent package's __path__ for submodules, otherwise
    sys.meta_path followed by the finders of sys.path."""
    if fullname.startswith('.'):
        raise ImportError(f"Relative module name {fullname!r} not supported")
    parent = fullname.rpartition('.')[0]
    if parent:
        package = importlib.import_module(parent)
        entries = getattr(package, '__path__', None)
        if entries is None:
            return
    else:
        yield from sys.meta_path
        entries = sys.path
    for entry in entries:
        yield get_importer(entry)


def _directory_modules(directory, prefix):
    """(name, ispkg) for the modules and packages in a directory."""
    if not directory or not os.path.isdir(directory):
        return
    try:
        names = sorted(os.listdir(directory))
    except OSError:
        return
    import inspect
    reported = set()
    for entry in names:
        if entry == '__pycache__' or entry.startswith('.'):
            continue
        modname = inspect.getmodulename(entry)
        if modname == '__init__' or modname in reported:
            continue
        ispkg = False
        full = os.path.join(directory, entry)
        if not modname and os.path.isdir(full) and '.' not in entry:
            try:
                contents = os.listdir(full)
            except OSError:
                continue
            if not any(inspect.getmodulename(c) == '__init__' for c in contents):
                continue   # not a regular package
            modname = entry
            ispkg = True
        if modname and '.' not in modname:
            reported.add(modname)
            yield prefix + modname, ispkg


def _zip_modules(finder, prefix):
    try:
        from zipimport import _zip_directory_cache
    except ImportError:
        return
    entries = sorted(_zip_directory_cache.get(finder.archive, ()))
    inner = finder.prefix
    depth = len(inner)
    import inspect
    reported = set()
    for entry in entries:
        if not entry.startswith(inner):
            continue
        parts = entry[depth:].split(os.sep)
        if len(parts) == 2 and parts[1].startswith('__init__.py'):
            if parts[0] not in reported:
                reported.add(parts[0])
                yield prefix + parts[0], True
        if len(parts) != 1:
            continue
        modname = inspect.getmodulename(parts[0])
        if modname == '__init__':
            continue
        if modname and '.' not in modname and modname not in reported:
            reported.add(modname)
            yield prefix + modname, False


def iter_importer_modules(importer, prefix=''):
    """(name, ispkg) pairs for the modules a finder can import."""
    if hasattr(importer, 'iter_modules'):
        return importer.iter_modules(prefix)
    if isinstance(importer, importlib.machinery.FileFinder):
        return _directory_modules(importer.path, prefix)
    if type(importer).__name__ == 'zipimporter' and hasattr(importer, 'archive'):
        return _zip_modules(importer, prefix)
    return iter(())


def iter_modules(path=None, prefix=''):
    """Yield ModuleInfo for the top-level modules on `path` (a list of
    directories; default sys.path), names prefixed with `prefix`."""
    if path is None:
        finders = iter_importers()
    elif isinstance(path, str):
        raise ValueError("path must be None or list of paths to "
                         "look for modules in")
    else:
        finders = map(get_importer, path)
    seen = set()
    for finder in finders:
        for name, ispkg in iter_importer_modules(finder, prefix):
            if name not in seen:
                seen.add(name)
                yield ModuleInfo(finder, name, ispkg)


def walk_packages(path=None, prefix='', onerror=None):
    """Like iter_modules(), but also recursing into packages (which are
    imported for that).  `onerror(name)` is called for packages that fail
    to import; without it ImportErrors are ignored and other exceptions
    propagate."""
    visited = set()

    def walk(entries, prefix):
        for info in iter_modules(entries, prefix):
            yield info
            if not info.ispkg:
                continue
            try:
                __import__(info.name)
            except ImportError:
                if onerror is not None:
                    onerror(info.name)
                continue
            except Exception:
                if onerror is None:
                    raise
                onerror(info.name)
                continue
            sub_path = getattr(sys.modules[info.name], '__path__', None) or []
            fresh = [p for p in sub_path if p not in visited]
            visited.update(fresh)
            yield from walk(fresh, info.name + '.')

    return walk(path, prefix)


# ---------------------------------------------------------------- resources

def get_data(package, resource):
    """The bytes of `resource` ('/'-separated, relative to the package
    directory) read through the package's loader; None if the loader
    cannot provide data."""
    spec = importlib.util.find_spec(package)
    if spec is None:
        return None
    loader = spec.loader
    if loader is None or not hasattr(loader, 'get_data'):
        return None
    module = sys.modules.get(package)
    if module is None:
        module = importlib._bootstrap._load(spec)
    location = getattr(module, '__file__', None)
    if location is None:
        return None
    pieces = [os.path.dirname(location)] + resource.split('/')
    return loader.get_data(os.path.join(*pieces))


def extend_path(path, name):
    """Extend a package's __path__ with the other directories on the
    search path that contain a subdirectory for the same package (and with
    the entries of '<name>.pkg' files).  Use as

        __path__ = extend_path(__path__, __name__)
    """
    if not isinstance(path, list):
        return path
    result = list(path)
    parent, _, last = name.rpartition('.')
    if parent:
        try:
            search = sys.modules[parent].__path__
        except (KeyError, AttributeError):
            return result
    else:
        search = sys.path
    pkg_file_name = name + '.pkg'
    for entry in search:
        if not isinstance(entry, str):
            continue
        finder = get_importer(entry)
        if finder is not None and hasattr(finder, 'find_spec'):
            spec = finder.find_spec(last)
            locations = (spec.submodule_search_locations or []) if spec else []
            for location in locations:
                if location not in result:
                    result.append(location)
        pkg_file = os.path.join(entry, pkg_file_name)
        if os.path.isfile(pkg_file):
            try:
                stream = open(pkg_file)
            except OSError as err:
                sys.stderr.write("Can't open %s: %s\n" % (pkg_file, err))
                continue
            with stream:
                for line in stream:
                    line = line.rstrip('\n')
                    if line and not line.startswith('#'):
                        result.append(line)
    return result


# ---------------------------------------------------------------- names

_DOTTED = r'(?!\d)\w+(?:\.(?!\d)\w+)*'
_NAME_PATTERN = re.compile(f'^(?P<pkg>{_DOTTED})(?P<cln>:(?P<obj>{_DOTTED})?)?$', re.UNICODE)


def resolve_name(name, strict=False):
    """Resolve 'package.module:object.attribute' (or, unless strict,
    'package.module.object.attribute') to the object it names.

    Raises ValueError for malformed names, ImportError if the module
    cannot be imported and AttributeError for missing attributes."""
    match = _NAME_PATTERN.match(name)
    if not match:
        raise ValueError(f'invalid format: {name!r}')
    module_name = match.group('pkg')
    if strict and not match.group('obj'):
        raise ValueError(f'invalid format: {name!r}')
    if match.group('cln'):
        target = importlib.import_module(module_name)
        attributes = match.group('obj')
        attributes = attributes.split('.') if attributes else []
    else:
        # import the longest importable prefix, then use attributes
        parts = module_name.split('.')
        imported = parts.pop(0)
        target = importlib.import_module(imported)
        while parts:
            candidate = f'{imported}.{parts[0]}'
            try:
                target = importlib.import_module(candidate)
            except ImportError:
                break
            imported = candidate
            parts.pop(0)
        attributes = parts
    for attribute in attributes:
        target = getattr(target, attribute)
    return target
