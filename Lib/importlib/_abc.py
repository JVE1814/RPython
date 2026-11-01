"""The Loader ABC (kept separate from importlib.abc so that importlib.util
can use it without importing the abc machinery for resources)."""
from . import _bootstrap
import abc


class Loader(metaclass=abc.ABCMeta):

    """Abstract base class for import loaders."""

    def create_module(self, spec):
        """Return a module to initialise, or None for the default module
        creation semantics."""
        # By default, defer to default semantics for the new module.
        return None

    def load_module(self, fullname):
        """Load and return the module (deprecated interface; exec_module()
        is used by the import system)."""
        if not hasattr(self, 'exec_module'):
            raise ImportError
        return _bootstrap._load_module_shim(self, fullname)
