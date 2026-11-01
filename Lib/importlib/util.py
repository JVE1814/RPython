"""Utility code for constructing importers and working with modules."""
from ._abc import Loader
from ._bootstrap import module_from_spec
from ._bootstrap import _resolve_name
from ._bootstrap import spec_from_loader
from ._bootstrap import _find_spec
from ._bootstrap_external import MAGIC_NUMBER
from ._bootstrap_external import cache_from_source
from ._bootstrap_external import decode_source
from ._bootstrap_external import source_from_cache
from ._bootstrap_external import spec_from_file_location

import _imp
import sys
import types


def source_hash(source_bytes):
    "Return the hash of *source_bytes* as used in hash-based pyc files."
    return _imp.source_hash(_imp.pyc_magic_number_token, source_bytes)


def resolve_name(name, package):
    """Resolve a relative module name to an absolute one."""
    if not name.startswith('.'):
        return name
    elif not package:
        raise ImportError(f'no package specified for {repr(name)} '
                          '(required for relative module names)')
    level = 0
    for character in name:
        if character != '.':
            break
        level += 1
    return _resolve_name(name[level:], package, level)


def _find_spec_from_path(name, path=None):
    """Return the spec for the specified module.

    The module itself is looked at first (sys.modules); otherwise the
    finders are searched along *path* (sys.path when None).
    """
    if name not in sys.modules:
        return _find_spec(name, path)
    else:
        module = sys.modules[name]
        if module is None:
            return None
        try:
            spec = module.__spec__
        except AttributeError:
            raise ValueError(f'{name}.__spec__ is not set') from None
        else:
            if spec is None:
                raise ValueError(f'{name}.__spec__ is None')
            return spec


def find_spec(name, package=None):
    """Return the spec for the specified module.

    If the module is in sys.modules its __spec__ is returned; otherwise the
    finders are searched (importing parent packages as needed).  A relative
    *name* is resolved against *package*.
    """
    fullname = resolve_name(name, package) if name.startswith('.') else name
    if fullname not in sys.modules:
        parent_name = fullname.rpartition('.')[0]
        if parent_name:
            parent = __import__(parent_name, fromlist=['__path__'])
            try:
                parent_path = parent.__path__
            except AttributeError as e:
                raise ModuleNotFoundError(
                    f"__path__ attribute not found on {parent_name!r} "
                    f"while trying to find {fullname!r}", name=fullname) from e
        else:
            parent_path = None
        return _find_spec(fullname, parent_path)
    else:
        module = sys.modules[fullname]
        if module is None:
            return None
        try:
            spec = module.__spec__
        except AttributeError:
            raise ValueError(f'{name}.__spec__ is not set') from None
        else:
            if spec is None:
                raise ValueError(f'{name}.__spec__ is None')
            return spec


class _incompatible_extension_module_restrictions:
    """Context manager to disable (or enable) the check that extension
    modules support multiple interpreters / free-threading.

    Usage: ``with _incompatible_extension_module_restrictions(disable_check=True): ...``
    """

    def __init__(self, *, disable_check):
        self.disable_check = bool(disable_check)

    def __enter__(self):
        self.old = _imp._override_multi_interp_extensions_check(self.override)
        return self

    def __exit__(self, *args):
        old = self.old
        del self.old
        _imp._override_multi_interp_extensions_check(old)

    @property
    def override(self):
        return -1 if self.disable_check else 1


class _LazyModule(types.ModuleType):

    """A module whose loading is deferred until an attribute is accessed."""

    def __getattribute__(self, attr):
        """Load the module on first attribute access, then return the
        attribute."""
        __spec__ = object.__getattribute__(self, '__spec__')
        loader_state = __spec__.loader_state
        with loader_state['lock']:
            # Only the first thread to arrive loads; others wait on the lock.
            if object.__getattribute__(self, '__class__') is _LazyModule:
                __class__ = loader_state['__class__']

                # Reentrant access from the loading thread itself.
                if loader_state['is_loading']:
                    return __class__.__getattribute__(self, attr)
                loader_state['is_loading'] = True

                __dict__ = __class__.__getattribute__(self, '__dict__')

                original_name = __spec__.name
                attrs_then = loader_state['__dict__']
                attrs_now = __dict__
                attrs_updated = {}
                for key, value in attrs_now.items():
                    # Remember attributes changed since the module object
                    # was created, to re-apply them after execution.
                    if key not in attrs_then:
                        attrs_updated[key] = value
                    elif id(attrs_now[key]) != id(attrs_then[key]):
                        attrs_updated[key] = value
                __spec__.loader.exec_module(self)
                # The module may have been replaced in sys.modules.
                if original_name in sys.modules:
                    if id(self) != id(sys.modules[original_name]):
                        raise ValueError(f"module object for {original_name!r} "
                                          "substituted in sys.modules during a lazy "
                                          "load")
                __dict__.update(attrs_updated)
                # Finally stop being lazy.
                self.__class__ = __class__

        return getattr(self, attr)

    def __delattr__(self, attr):
        """Trigger the load and then perform the deletion."""
        self.__getattribute__(attr)
        delattr(self, attr)


class LazyLoader(Loader):

    """A loader that creates a module which defers loading until first
    attribute access."""

    @staticmethod
    def __check_eager_loader(loader):
        if not hasattr(loader, 'exec_module'):
            raise TypeError('loader must define exec_module()')

    @classmethod
    def factory(cls, loader):
        """Construct a callable which returns the eager loader made lazy."""
        cls.__check_eager_loader(loader)
        return lambda *args, **kwargs: cls(loader(*args, **kwargs))

    def __init__(self, loader):
        self.__check_eager_loader(loader)
        self.loader = loader

    def create_module(self, spec):
        return self.loader.create_module(spec)

    def exec_module(self, module):
        """Make the module load lazily."""
        # Threading is only needed for lazy loading, and importlib.util can
        # be pulled in at interpreter startup.
        import threading
        module.__spec__.loader = self.loader
        module.__loader__ = self.loader
        loader_state = {}
        loader_state['__dict__'] = module.__dict__.copy()
        loader_state['__class__'] = module.__class__
        loader_state['lock'] = threading.RLock()
        loader_state['is_loading'] = False
        module.__spec__.loader_state = loader_state
        module.__class__ = _LazyModule


__all__ = ['LazyLoader', 'Loader', 'MAGIC_NUMBER',
           'cache_from_source', 'decode_source', 'find_spec',
           'module_from_spec', 'resolve_name', 'source_from_cache',
           'source_hash', 'spec_from_file_location', 'spec_from_loader']
