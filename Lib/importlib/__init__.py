"""The public face of the import system: import_module(), reload(),
invalidate_caches() and the submodules machinery, util, abc, resources and
metadata."""
__all__ = ['__import__', 'import_module', 'invalidate_caches', 'reload']

import _imp
import sys

# The import machinery normally runs from the pre-loaded _frozen_importlib
# modules; fall back to executing the source files when they are absent
# (e.g. when tests load a fresh copy of importlib with the frozen modules
# blocked).
try:
    import _frozen_importlib as _bootstrap
except ImportError:
    from . import _bootstrap
    _bootstrap._setup(sys, _imp)
else:
    _bootstrap.__name__ = 'importlib._bootstrap'
    _bootstrap.__package__ = 'importlib'
    try:
        _bootstrap.__file__ = __file__.replace('__init__.py', '_bootstrap.py')
    except NameError:
        pass
    sys.modules['importlib._bootstrap'] = _bootstrap

try:
    import _frozen_importlib_external as _bootstrap_external
except ImportError:
    from . import _bootstrap_external
    _bootstrap_external._set_bootstrap_module(_bootstrap)
    _bootstrap._bootstrap_external = _bootstrap_external
else:
    _bootstrap_external.__name__ = 'importlib._bootstrap_external'
    _bootstrap_external.__package__ = 'importlib'
    try:
        _bootstrap_external.__file__ = __file__.replace('__init__.py', '_bootstrap_external.py')
    except NameError:
        pass
    sys.modules['importlib._bootstrap_external'] = _bootstrap_external

_pack_uint32 = _bootstrap_external._pack_uint32
_unpack_uint32 = _bootstrap_external._unpack_uint32

from ._bootstrap import __import__


def invalidate_caches():
    """Call invalidate_caches() on every finder on sys.meta_path that has
    such a method."""
    for finder in sys.meta_path:
        if hasattr(finder, 'invalidate_caches'):
            finder.invalidate_caches()


def import_module(name, package=None):
    """Import and return a module.

    A relative *name* (leading dots) requires *package*, the package name
    to resolve it against.
    """
    level = 0
    if name.startswith('.'):
        if not package:
            raise TypeError("the 'package' argument is required to perform a "
                            f"relative import for {name!r}")
        for character in name:
            if character != '.':
                break
            level += 1
    return _bootstrap._gcd_import(name[level:], package, level)


_RELOADING = {}


def reload(module):
    """Re-execute a previously imported module in its existing module
    object and return it."""
    try:
        name = module.__spec__.name
    except AttributeError:
        try:
            name = module.__name__
        except AttributeError:
            raise TypeError("reload() argument must be a module") from None

    if sys.modules.get(name) is not module:
        raise ImportError(f"module {name} not in sys.modules", name=name)
    if name in _RELOADING:
        return _RELOADING[name]
    _RELOADING[name] = module
    try:
        parent_name = name.rpartition('.')[0]
        if parent_name:
            try:
                parent = sys.modules[parent_name]
            except KeyError:
                msg = f"parent {parent_name!r} not in sys.modules"
                raise ImportError(msg, name=parent_name) from None
            else:
                pkgpath = parent.__path__
        else:
            pkgpath = None
        target = module
        spec = module.__spec__ = _bootstrap._find_spec(name, pkgpath, target)
        if spec is None:
            raise ModuleNotFoundError(f"spec not found for the module {name!r}", name=name)
        _bootstrap._exec(spec, module)
        # the module may have replaced itself in sys.modules
        return sys.modules[name]
    finally:
        try:
            del _RELOADING[name]
        except KeyError:
            pass
