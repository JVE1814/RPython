"""Helpers for working with the import system: resolving relative names,
finding specs, cache file names, and loading modules lazily."""

from ._abc import Loader
from ._bootstrap import module_from_spec
from ._bootstrap import _resolve_name
from ._bootstrap import spec_from_loader
from ._bootstrap import _find_spec
from ._bootstrap_external import MAGIC_NUMBER
from ._bootstrap_external import _RAW_MAGIC_NUMBER
from ._bootstrap_external import cache_from_source
from ._bootstrap_external import decode_source
from ._bootstrap_external import source_from_cache
from ._bootstrap_external import spec_from_file_location

import _imp
import _thread
import sys
import types

__all__ = [
    'LazyLoader', 'Loader', 'MAGIC_NUMBER', 'cache_from_source',
    'decode_source', 'find_spec', 'module_from_spec', 'resolve_name',
    'source_from_cache', 'source_hash', 'spec_from_file_location',
    'spec_from_loader',
]


def source_hash(source_bytes):
    """The hash of *source_bytes* that hash-based .pyc files record."""
    return _imp.source_hash(_RAW_MAGIC_NUMBER, source_bytes)


def resolve_name(name, package):
    """The absolute module name for *name*, which may be relative (leading
    dots) to the package named *package*."""
    if not name.startswith('.'):
        return name
    if not package:
        raise ImportError(f'cannot resolve the relative name {name!r} '
                          f'without a package')
    stripped = name.lstrip('.')
    return _resolve_name(stripped, package, len(name) - len(stripped))


def _spec_of_loaded(module, name):
    # the spec recorded on a module that is already in sys.modules
    if module is None:
        return None
    try:
        spec = module.__spec__
    except AttributeError:
        raise ValueError(f'{name}.__spec__ is not set') from None
    if spec is None:
        raise ValueError(f'{name}.__spec__ is None')
    return spec


def _find_spec_from_path(name, path=None):
    """The spec of the module *name*, searched for on *path* (sys.path when
    None) unless the module is imported already. Parents are not imported:
    *name* must be a top-level name or *path* the parent's __path__."""
    if name in sys.modules:
        return _spec_of_loaded(sys.modules[name], name)
    return _find_spec(name, path)


def find_spec(name, package=None):
    """The spec of the module *name* (relative names are resolved against
    *package*), or None if no finder knows it.

    A module that is imported already answers with its __spec__. Otherwise
    the parent package is imported so that its __path__ can be searched.
    """
    fullname = resolve_name(name, package) if name.startswith('.') else name
    if fullname in sys.modules:
        return _spec_of_loaded(sys.modules[fullname], name)
    parent_name = fullname.rpartition('.')[0]
    search_path = None
    if parent_name:
        parent = __import__(parent_name, fromlist=['__path__'])
        try:
            search_path = parent.__path__
        except AttributeError as exc:
            raise ModuleNotFoundError(
                f'__path__ attribute not found on {parent_name!r} '
                f'while trying to find {fullname!r}', name=fullname) from exc
    return _find_spec(fullname, search_path)


class _incompatible_extension_module_restrictions:
    """Context manager that turns the check for extension modules that do not
    support multiple interpreters off (disable_check=True) or on, restoring
    the previous setting on exit."""

    def __init__(self, *, disable_check):
        self.disable_check = bool(disable_check)

    @property
    def override(self):
        # -1: check disabled, 1: check enforced (0 would mean "default")
        return -1 if self.disable_check else 1

    def __enter__(self):
        self._previous = _imp._override_multi_interp_extensions_check(self.override)
        return self

    def __exit__(self, *exc_info):
        previous, self._previous = self._previous, None
        _imp._override_multi_interp_extensions_check(previous)


# ---------------------------------------------------------------- lazy loading
#
# LazyLoader.exec_module() does not execute the module: it records what the
# module looks like (its namespace and class) in spec.loader_state and turns
# the module into a _LazyModule. The first attribute read or deletion runs the
# real loader; afterwards the attributes the caller set in the meantime are
# put back on top of what the module's code produced, and the module gets its
# own class back (unless its code chose another class itself).

def _complete_lazy_load(module, spec, state):
    namespace = object.__getattribute__(module, '__dict__')
    recorded = state['namespace']
    # what was assigned while the module was still lazy wins over its code
    assigned = {key: value for key, value in namespace.items()
                if key not in recorded or recorded[key] is not value}
    name = spec.name
    spec.loader.exec_module(module)
    if name in sys.modules and sys.modules[name] is not module:
        raise ValueError(f'module object for {name!r} substituted in '
                         f'sys.modules during a lazy load')
    namespace.update(assigned)
    if type(module) is _LazyModule:
        object.__setattr__(module, '__class__', state['class'])


def _ensure_loaded(module):
    spec = object.__getattribute__(module, '__spec__')
    state = spec.loader_state
    with state['lock']:
        # another thread may have finished the load while this one waited,
        # and code run by the load may read attributes of the module again
        if type(module) is not _LazyModule or state['loading']:
            return False
        state['loading'] = True
        try:
            _complete_lazy_load(module, spec, state)
        finally:
            state['loading'] = False
    return True


class _LazyModule(types.ModuleType):

    """A module whose code runs when one of its attributes is first used."""

    def __getattribute__(self, attr):
        if not _ensure_loaded(self) and type(self) is _LazyModule:
            # still lazy: the load is running on this thread
            return object.__getattribute__(self, attr)
        return getattr(self, attr)

    def __delattr__(self, attr):
        _ensure_loaded(self)
        delattr(self, attr)


class LazyLoader(Loader):

    """Wraps a loader so that a module's code runs on first attribute
    access instead of at import time."""

    @staticmethod
    def _require_exec_module(loader):
        if not hasattr(loader, 'exec_module'):
            raise TypeError('loader must define exec_module()')

    @classmethod
    def factory(cls, loader):
        """A callable that builds the wrapped loader from its arguments and
        returns it wrapped in a LazyLoader."""
        cls._require_exec_module(loader)
        def make(*args, **kwargs):
            return cls(loader(*args, **kwargs))
        return make

    def __init__(self, loader):
        self._require_exec_module(loader)
        self.loader = loader

    def create_module(self, spec):
        return self.loader.create_module(spec)

    def exec_module(self, module):
        """Make *module* lazy: its code runs when it is first used."""
        module.__spec__.loader = self.loader
        module.__loader__ = self.loader
        module.__spec__.loader_state = {
            'namespace': module.__dict__.copy(),
            'class': type(module),
            'lock': _thread.RLock(),
            'loading': False,
        }
        module.__class__ = _LazyModule
