"""Abstract base classes related to import."""
from . import _bootstrap_external
from . import machinery
try:
    import _frozen_importlib
except ImportError as exc:
    if exc.name != '_frozen_importlib':
        raise
    _frozen_importlib = None
try:
    import _frozen_importlib_external
except ImportError:
    _frozen_importlib_external = _bootstrap_external
from ._abc import Loader
import abc


__all__ = [
    'Loader', 'MetaPathFinder', 'PathEntryFinder',
    'ResourceLoader', 'InspectLoader', 'ExecutionLoader',
    'FileLoader', 'SourceLoader',
]


def __getattr__(name):
    """Lazily provide the resource ABCs that moved to importlib.resources."""
    if name in ('ResourceReader', 'Traversable', 'TraversableResources'):
        import warnings
        warnings._deprecated(f"importlib.abc.{name}",
                             remove=(3, 14))
        from .resources import abc as _resources_abc
        return getattr(_resources_abc, name)
    raise AttributeError(f'module {__name__!r} has no attribute {name!r}')


def _register(abstract_cls, *classes):
    for cls in classes:
        abstract_cls.register(cls)
        if _frozen_importlib is not None:
            try:
                frozen_cls = getattr(_frozen_importlib, cls.__name__)
            except AttributeError:
                frozen_cls = getattr(_frozen_importlib_external, cls.__name__)
            abstract_cls.register(frozen_cls)


class MetaPathFinder(metaclass=abc.ABCMeta):

    """Abstract base class for import finders on sys.meta_path."""

    # find_spec() is intentionally not abstract (backwards compatibility).

    def invalidate_caches(self):
        """Invalidate any internal cache (used by
        importlib.invalidate_caches())."""

_register(MetaPathFinder, machinery.BuiltinImporter, machinery.FrozenImporter,
          machinery.PathFinder)


class PathEntryFinder(metaclass=abc.ABCMeta):

    """Abstract base class for path entry finders used by PathFinder."""

    def invalidate_caches(self):
        """Invalidate any internal cache (used by
        PathFinder.invalidate_caches())."""

_register(PathEntryFinder, machinery.FileFinder)


class ResourceLoader(Loader):

    """Abstract base class for loaders which can return data from their
    back-end storage (deprecated)."""

    def __init__(self):
        import warnings
        warnings.warn('importlib.abc.ResourceLoader is deprecated in '
                      'favour of supporting resource loading through '
                      'importlib.resources.abc.TraversableResources.',
                      DeprecationWarning, stacklevel=2)
        super().__init__()

    @abc.abstractmethod
    def get_data(self, path):
        """Return the bytes stored at *path*; raise OSError if there are
        none."""
        raise OSError


class InspectLoader(Loader):

    """Abstract base class for loaders which support inspection of the
    modules they load."""

    def is_package(self, fullname):
        """Whether the module is a package; ImportError if it cannot be
        found."""
        raise ImportError

    def get_code(self, fullname):
        """Return the code object of the module (None if there is none),
        compiled from get_source() by default."""
        source = self.get_source(fullname)
        if source is None:
            return None
        return self.source_to_code(source)

    @abc.abstractmethod
    def get_source(self, fullname):
        """Return the source of the module (None if unavailable)."""
        raise ImportError

    @staticmethod
    def source_to_code(data, path='<string>', fullname=None):
        """Compile source data (str or bytes) into a code object."""
        return compile(data, path, 'exec', dont_inherit=True)

    exec_module = _bootstrap_external._LoaderBasics.exec_module
    load_module = _bootstrap_external._LoaderBasics.load_module

_register(InspectLoader, machinery.BuiltinImporter, machinery.FrozenImporter,
          machinery.NamespaceLoader)


class ExecutionLoader(InspectLoader):

    """Abstract base class for loaders that wish to support the execution
    of modules as scripts."""

    @abc.abstractmethod
    def get_filename(self, fullname):
        """Return the path of the module; ImportError if it cannot be
        found."""
        raise ImportError

    def get_code(self, fullname):
        """Compile the source with get_filename() as the file name."""
        source = self.get_source(fullname)
        if source is None:
            return None
        try:
            path = self.get_filename(fullname)
        except ImportError:
            return self.source_to_code(source)
        else:
            return self.source_to_code(source, path)

_register(
    ExecutionLoader,
    machinery.ExtensionFileLoader,
)


class FileLoader(_bootstrap_external.FileLoader, ResourceLoader, ExecutionLoader):

    """Abstract base class partially implementing the ResourceLoader and
    ExecutionLoader ABCs."""

_register(FileLoader, machinery.SourceFileLoader,
            machinery.SourcelessFileLoader)


class SourceLoader(_bootstrap_external.SourceLoader, ResourceLoader, ExecutionLoader):

    """Abstract base class for loading source code (and optionally any
    corresponding bytecode).

    Subclasses provide get_data() and get_filename(); path_stats() and
    set_data() enable bytecode caching.
    """

    def path_mtime(self, path):
        """Return the (int) modification time of *path* (deprecated)."""
        import warnings
        warnings.warn('SourceLoader.path_mtime is deprecated in favour of '
                      'SourceLoader.path_stats().',
                      DeprecationWarning, stacklevel=2)
        if self.path_stats.__func__ is SourceLoader.path_stats:
            raise OSError
        return int(self.path_stats(path)['mtime'])

    def path_stats(self, path):
        """Return {'mtime': ..., 'size': ...} for *path*; raise OSError if
        the path cannot be handled."""
        if self.path_mtime.__func__ is SourceLoader.path_mtime:
            raise OSError
        return {'mtime': self.path_mtime(path)}

    def set_data(self, path, data):
        """Write bytes to *path*; failures are silently ignored (used for
        bytecode caching)."""

_register(SourceLoader, machinery.SourceFileLoader)
