"""Core of the import system: module specs, per-module import locks, the
built-in and frozen importers, and the find-and-load algorithm behind
``__import__``.

The interpreter loads this file at start-up as ``_frozen_importlib`` and calls
``_install(sys, _imp)``.  Because nothing else is importable at that point, the
module may only rely on built-in modules, which ``_setup`` injects as globals.
Application code should use the ``importlib`` package instead.
"""

# Built-in modules injected by _setup().  They are None while this module is
# being bootstrapped, and the code below copes with that.
_thread = None
_warnings = None
_weakref = None

# importlib._bootstrap_external, once the path-based machinery is installed.
_bootstrap_external = None


# ---------------------------------------------------------------- small helpers

def _object_name(obj):
    """Qualified name of *obj*, or of its type when it has none."""
    try:
        return obj.__qualname__
    except AttributeError:
        return type(obj).__qualname__


def _wrap(new, old):
    """Make the wrapper *new* look like *old* (functools.wraps is unavailable)."""
    for attr in ('__module__', '__name__', '__qualname__', '__doc__'):
        if hasattr(old, attr):
            setattr(new, attr, getattr(old, attr))
    new.__dict__.update(old.__dict__)


def _new_module(name):
    return type(sys)(name)


def _warn(message, category, stacklevel=1):
    # The _warnings accelerator is used when present; during early start-up
    # the pure-Python warnings module may not be importable yet.
    if _warnings is not None:
        _warnings.warn(message, category, stacklevel + 1)
        return
    try:
        import warnings
    except ImportError:
        return
    warnings.warn(message, category, stacklevel + 1)


def _call_with_frames_removed(f, *args, **kwds):
    """Call f(*args, **kwds).

    Tracebacks of failed imports leave out the import machinery's own frames up
    to (and including) calls made through this function, so user code sees the
    frames of the module being imported first.
    """
    return f(*args, **kwds)


def _verbose_message(message, *args, verbosity=1):
    """Print a str.format()-style message to stderr under ``python -v``."""
    if sys.flags.verbose < verbosity:
        return
    if not message.startswith(('#', 'import ')):
        message = '# ' + message
    print(message.format(*args), file=sys.stderr)


# ---------------------------------------------------------------- module locks
#
# Importing a module holds that module's lock, so a second thread importing the
# same module waits until the first one is done.  Two threads importing modules
# that import each other would wait forever, so before waiting a thread checks
# whether the owner of the lock is (transitively) waiting for a lock the
# current thread holds; if so it gives up with _DeadlockError and the import
# proceeds with the partially initialised module, as a single thread would.

# name -> weak reference to the module's lock (a plain reference without
# _weakref); entries disappear when nobody uses the lock any more.
_module_locks = {}

# thread id -> list of the module locks that thread is currently waiting for.
_blocking_on = {}


class _DeadlockError(RuntimeError):
    pass


def _waits_for(start_tid, target_tid):
    """Does thread *start_tid* (transitively) wait for a lock owned by
    *target_tid*?  Walks the wait-for graph recorded in _blocking_on."""
    pending = [start_tid]
    visited = set()
    while pending:
        tid = pending.pop()
        if tid == target_tid:
            return True
        if tid is None or tid in visited:
            continue
        visited.add(tid)
        for lock in tuple(_blocking_on.get(tid, ())):
            pending.append(lock.owner)
    return False


class _ModuleLock:
    """A re-entrant lock for one module that detects import deadlocks.

    Waiting threads park on a private "ticket" lock each; a release that frees
    the lock wakes the longest waiting thread, which then competes for it again.
    """

    def __init__(self, name):
        self.name = name
        self._state_lock = _thread.RLock()  # guards the fields below
        self.owner = None      # thread id of the holder
        self.depth = 0         # recursion depth of the holder
        self._tickets = []     # one acquired lock per parked thread

    @property
    def waiters(self):
        return self._tickets

    def has_deadlock(self):
        # waiting would deadlock if the holder waits for something we hold
        return _waits_for(self.owner, _thread.get_ident())

    def acquire(self):
        """Acquire the lock, waiting while another thread holds it.

        Raises _DeadlockError instead of waiting when the wait would never end.
        """
        me = _thread.get_ident()
        waiting = _blocking_on.setdefault(me, [])
        waiting.append(self)
        try:
            while True:
                with self._state_lock:
                    if self.depth == 0 or self.owner == me:
                        self.owner = me
                        self.depth += 1
                        return True
                    if self.has_deadlock():
                        raise _DeadlockError(f'deadlock detected by {self!r}')
                    ticket = _thread.allocate_lock()
                    ticket.acquire()
                    self._tickets.append(ticket)
                # parked until a release hands the ticket back
                try:
                    ticket.acquire()
                except BaseException:
                    self._abandon_ticket(ticket)
                    raise
        finally:
            try:
                waiting.remove(self)
            except ValueError:
                pass
            if not waiting and _blocking_on.get(me) is waiting:
                try:
                    del _blocking_on[me]
                except KeyError:
                    pass

    def _abandon_ticket(self, ticket):
        # A parked thread gives up (e.g. KeyboardInterrupt).  If its wake-up
        # was already sent, pass it on so that no other waiter is stranded.
        with self._state_lock:
            if ticket in self._tickets:
                self._tickets.remove(ticket)
            elif self.depth == 0 and self._tickets:
                self._tickets.pop(0).release()

    def release(self):
        me = _thread.get_ident()
        with self._state_lock:
            if self.owner != me or self.depth == 0:
                raise RuntimeError('cannot release un-acquired lock')
            self.depth -= 1
            if self.depth == 0:
                self.owner = None
                if self._tickets:
                    self._tickets.pop(0).release()

    def locked(self):
        return self.depth > 0

    def __repr__(self):
        return f'_ModuleLock({self.name!r}) at {id(self)}'


class _DummyModuleLock:
    """Stand-in for _ModuleLock when there are no threads."""

    def __init__(self, name):
        self.name = name
        self.depth = 0
        self.waiters = ()

    def acquire(self):
        self.depth += 1
        return True

    def release(self):
        if self.depth == 0:
            raise RuntimeError('cannot release un-acquired lock')
        self.depth -= 1

    def locked(self):
        return self.depth > 0

    def __repr__(self):
        return f'_DummyModuleLock({self.name!r}) at {id(self)}'


def _get_module_lock(name):
    """The lock of module *name*, created on first use.

    Must not be called with the global import lock held.
    """
    _imp.acquire_lock()
    try:
        entry = _module_locks.get(name)
        lock = entry() if (entry is not None and _weakref is not None) else entry
        if lock is None:
            lock = _ModuleLock(name) if _thread is not None else _DummyModuleLock(name)
            if _weakref is None:
                _module_locks[name] = lock
            else:
                def forget(ref, name=name):
                    _imp.acquire_lock()
                    try:
                        # a newer lock may have replaced the dead one
                        if _module_locks.get(name) is ref:
                            del _module_locks[name]
                    finally:
                        _imp.release_lock()
                _module_locks[name] = _weakref.ref(lock, forget)
    finally:
        _imp.release_lock()
    return lock


def _drop_unused_lock(name, lock):
    # Without weak references nothing removes the registry entry: drop it when
    # the lock is idle.
    if _weakref is not None:
        return
    _imp.acquire_lock()
    try:
        if _module_locks.get(name) is lock and not lock.locked() and not lock.waiters:
            del _module_locks[name]
    finally:
        _imp.release_lock()


class _ModuleLockManager:
    """``with _ModuleLockManager(name):`` holds the module lock of *name*."""

    def __init__(self, name):
        self._name = name
        self._lock = None

    def __enter__(self):
        self._lock = _get_module_lock(self._name)
        self._lock.acquire()

    def __exit__(self, *exc_info):
        self._lock.release()
        _drop_unused_lock(self._name, self._lock)


def _lock_unlock_module(name):
    """Wait until module *name* is no longer being imported by another thread
    (returns at once if waiting would deadlock)."""
    lock = _get_module_lock(name)
    try:
        lock.acquire()
    except _DeadlockError:
        return
    lock.release()
    _drop_unused_lock(name, lock)


# ---------------------------------------------------------------- loader helpers

def _requires_builtin(fxn):
    """Decorator for loader methods: *fullname* must name a built-in module."""
    def _requires_builtin_wrapper(self, fullname):
        if fullname not in sys.builtin_module_names:
            raise ImportError(f'{fullname!r} is not a built-in module', name=fullname)
        return fxn(self, fullname)
    _wrap(_requires_builtin_wrapper, fxn)
    return _requires_builtin_wrapper


def _requires_frozen(fxn):
    """Decorator for loader methods: *fullname* must name a frozen module."""
    def _requires_frozen_wrapper(self, fullname):
        if not _imp.is_frozen(fullname):
            raise ImportError(f'{fullname!r} is not a frozen module', name=fullname)
        return fxn(self, fullname)
    _wrap(_requires_frozen_wrapper, fxn)
    return _requires_frozen_wrapper


def _load_module_shim(self, fullname):
    """Generic implementation of the deprecated Loader.load_module()."""
    _warn("the load_module() method is deprecated and slated for removal in "
          "Python 3.12; use exec_module() instead", DeprecationWarning)
    spec = spec_from_loader(fullname, self)
    existing = sys.modules.get(fullname)
    if existing is None and fullname not in sys.modules:
        return _load(spec)
    _exec(spec, existing)
    return sys.modules[fullname]


# ---------------------------------------------------------------- module reprs

def _module_repr(module):
    """repr() of a module object."""
    spec = getattr(module, '__spec__', None)
    if spec:
        return _module_repr_from_spec(spec)
    name = getattr(module, '__name__', '?')
    try:
        filename = module.__file__
    except AttributeError:
        loader = getattr(module, '__loader__', None)
        if loader is None:
            return f'<module {name!r}>'
        return f'<module {name!r} ({loader!r})>'
    return f'<module {name!r} from {filename!r}>'


def _module_repr_from_spec(spec):
    name = '?' if spec.name is None else spec.name
    if spec.origin is not None:
        if spec.has_location:
            return f'<module {name!r} from {spec.origin!r}>'
        return f'<module {spec.name!r} ({spec.origin})>'
    loader = spec.loader
    if loader is None:
        return f'<module {name!r}>'
    if _bootstrap_external is not None and isinstance(loader, _bootstrap_external.NamespaceLoader):
        return f'<module {name!r} (namespace) from {list(loader._path)}>'
    return f'<module {name!r} ({loader!r})>'


# ---------------------------------------------------------------- ModuleSpec

class ModuleSpec:
    """Everything the import system knows about a module before loading it.

    ``name``: the fully qualified module name.  ``loader``: the loader that
    creates and executes the module (None for namespace packages).
    ``origin``: where the module comes from (a file name for file-based
    modules, "built-in", "frozen", ...).  ``loader_state``: loader-specific
    data.  ``submodule_search_locations``: the package's ``__path__`` (None for
    plain modules).  ``cached``: the bytecode cache file, if any.
    ``parent``: the package the module belongs to.  ``has_location``: whether
    ``origin`` names a loadable location (it then becomes ``__file__``).
    """

    def __init__(self, name, loader, *, origin=None, loader_state=None,
                 is_package=None):
        self.name = name
        self.loader = loader
        self.origin = origin
        self.loader_state = loader_state
        self.submodule_search_locations = [] if is_package else None
        self._uninitialized_submodules = []
        self._set_fileattr = False
        self._cached = None

    def __repr__(self):
        fields = [f'name={self.name!r}', f'loader={self.loader!r}']
        if self.origin is not None:
            fields.append(f'origin={self.origin!r}')
        if self.submodule_search_locations is not None:
            fields.append(f'submodule_search_locations={self.submodule_search_locations}')
        return f'{type(self).__name__}({", ".join(fields)})'

    def __eq__(self, other):
        try:
            return (self.name == other.name
                    and self.loader == other.loader
                    and self.origin == other.origin
                    and self.submodule_search_locations == other.submodule_search_locations
                    and self.cached == other.cached
                    and self.has_location == other.has_location)
        except AttributeError:
            return NotImplemented

    @property
    def cached(self):
        # computed lazily from a file origin
        if self._cached is None and self._set_fileattr and self.origin is not None:
            if _bootstrap_external is None:
                raise NotImplementedError
            self._cached = _bootstrap_external._get_cached(self.origin)
        return self._cached

    @cached.setter
    def cached(self, value):
        self._cached = value

    @property
    def parent(self):
        if self.submodule_search_locations is not None:
            return self.name
        return self.name.rpartition('.')[0]

    @property
    def has_location(self):
        return self._set_fileattr

    @has_location.setter
    def has_location(self, value):
        self._set_fileattr = bool(value)


def spec_from_loader(name, loader, *, origin=None, is_package=None):
    """Build a ModuleSpec from a loader (and what it can tell about *name*)."""
    if origin is None:
        origin = getattr(loader, '_ORIGIN', None)
    if not origin and hasattr(loader, 'get_filename'):
        # file-based loader: the location comes from get_filename()
        if _bootstrap_external is None:
            raise NotImplementedError
        from_file = _bootstrap_external.spec_from_file_location
        if is_package is None:
            return from_file(name, loader=loader)
        return from_file(name, loader=loader,
                         submodule_search_locations=[] if is_package else None)
    if is_package is None:
        if hasattr(loader, 'is_package'):
            try:
                is_package = loader.is_package(name)
            except ImportError:
                is_package = None   # unknown
        else:
            is_package = False
    return ModuleSpec(name, loader, origin=origin, is_package=is_package)


def _spec_from_module(module, loader=None, origin=None):
    """A spec for an already existing module (its own __spec__ if it has one)."""
    spec = getattr(module, '__spec__', None)
    if spec is not None:
        return spec
    if loader is None:
        loader = getattr(module, '__loader__', None)
    location = getattr(module, '__file__', None)
    if origin is None:
        if loader is not None:
            origin = getattr(loader, '_ORIGIN', None)
        if not origin and location is not None:
            origin = location
    try:
        search_locations = list(module.__path__)
    except AttributeError:
        search_locations = None
    spec = ModuleSpec(module.__name__, loader, origin=origin)
    spec._set_fileattr = location is not None and origin == location
    spec.cached = getattr(module, '__cached__', None)
    spec.submodule_search_locations = search_locations
    return spec


def _set_module_attr(module, attr, value):
    # Some module-like objects refuse attributes; that is not an error here.
    try:
        setattr(module, attr, value)
    except AttributeError:
        pass


def _init_module_attrs(spec, module, *, override=False):
    """Set the import-related attributes of *module* from *spec*.

    Unless *override* is true, attributes that already have a non-None value
    are kept.  ``__spec__`` is always set.
    """
    def wanted(attr):
        return override or getattr(module, attr, None) is None

    if wanted('__name__'):
        _set_module_attr(module, '__name__', spec.name)
    if wanted('__loader__'):
        loader = spec.loader
        if loader is None and spec.submodule_search_locations is not None:
            # a namespace package: give it a NamespaceLoader without
            # running its __init__ (no path finder is involved here)
            if _bootstrap_external is None:
                raise NotImplementedError
            ns_loader_type = _bootstrap_external.NamespaceLoader
            loader = ns_loader_type.__new__(ns_loader_type)
            loader._path = spec.submodule_search_locations
            spec.loader = loader
            module.__file__ = None
        _set_module_attr(module, '__loader__', loader)
    if wanted('__package__'):
        _set_module_attr(module, '__package__', spec.parent)
    _set_module_attr(module, '__spec__', spec)
    if wanted('__path__') and spec.submodule_search_locations is not None:
        _set_module_attr(module, '__path__', spec.submodule_search_locations)
    if spec.has_location:
        if wanted('__file__'):
            _set_module_attr(module, '__file__', spec.origin)
    return module


def module_from_spec(spec):
    """Create a module from *spec* (the loader's create_module() or a plain
    module object) and initialise its import attributes."""
    module = None
    loader = spec.loader
    if hasattr(loader, 'create_module'):
        module = loader.create_module(spec)
    elif hasattr(loader, 'exec_module'):
        raise ImportError('loaders that define exec_module() must also define create_module()')
    if module is None:
        module = _new_module(spec.name)
    _init_module_attrs(spec, module)
    return module


def _move_to_end_of_sys_modules(name):
    # A module may replace itself in sys.modules while executing; what is
    # there afterwards is the result of the import, re-inserted last.
    module = sys.modules.pop(name)
    sys.modules[name] = module
    return module


def _exec(spec, module):
    """Execute *spec* in the existing *module* (importlib.reload)."""
    name = spec.name
    with _ModuleLockManager(name):
        if sys.modules.get(name) is not module:
            raise ImportError(f'module {name!r} not in sys.modules', name=name)
        try:
            if spec.loader is None and spec.submodule_search_locations is None:
                raise ImportError('missing loader', name=spec.name)
            _init_module_attrs(spec, module, override=True)
            if spec.loader is not None:
                if hasattr(spec.loader, 'exec_module'):
                    spec.loader.exec_module(module)
                else:
                    _warn(f"{_object_name(spec.loader)}.exec_module() not found; "
                          "falling back to load_module()", ImportWarning)
                    spec.loader.load_module(name)
        finally:
            module = _move_to_end_of_sys_modules(spec.name)
    return module


def _load_backward_compatible(spec):
    """Load through a loader that only has the old load_module() API."""
    name = spec.name
    try:
        spec.loader.load_module(name)
    except:
        if name in sys.modules:
            _move_to_end_of_sys_modules(name)
        raise
    module = _move_to_end_of_sys_modules(name)
    if getattr(module, '__loader__', None) is None:
        _set_module_attr(module, '__loader__', spec.loader)
    if getattr(module, '__package__', None) is None:
        package = module.__name__ if hasattr(module, '__path__') else name.rpartition('.')[0]
        _set_module_attr(module, '__package__', package)
    if getattr(module, '__spec__', None) is None:
        _set_module_attr(module, '__spec__', spec)
    return module


def _load_unlocked(spec):
    """Create and execute the module described by *spec*; the caller holds
    the module lock."""
    loader = spec.loader
    if loader is not None and not hasattr(loader, 'exec_module'):
        _warn(f"{_object_name(loader)}.exec_module() not found; "
              "falling back to load_module()", ImportWarning)
        return _load_backward_compatible(spec)

    module = module_from_spec(spec)
    # While _initializing is set, other threads importing the module wait for
    # the lock instead of using the half-executed module.
    spec._initializing = True
    try:
        sys.modules[spec.name] = module
        try:
            if loader is not None:
                loader.exec_module(module)
            elif spec.submodule_search_locations is None:
                raise ImportError('missing loader', name=spec.name)
        except:
            sys.modules.pop(spec.name, None)
            raise
        module = _move_to_end_of_sys_modules(spec.name)
        _verbose_message('import {!r} # {!r}', spec.name, loader)
    finally:
        spec._initializing = False
    return module


def _load(spec):
    """Load *spec* under its module lock (the module is created afresh)."""
    with _ModuleLockManager(spec.name):
        return _load_unlocked(spec)


# ---------------------------------------------------------------- built-in modules

class BuiltinImporter:
    """Meta path finder and loader for the modules compiled into the
    interpreter (sys.builtin_module_names)."""

    _ORIGIN = 'built-in'

    @classmethod
    def find_spec(cls, fullname, path=None, target=None):
        if not _imp.is_builtin(fullname):
            return None
        return spec_from_loader(fullname, cls, origin=cls._ORIGIN)

    @staticmethod
    def create_module(spec):
        if spec.name not in sys.builtin_module_names:
            raise ImportError(f'{spec.name!r} is not a built-in module', name=spec.name)
        return _call_with_frames_removed(_imp.create_builtin, spec)

    @staticmethod
    def exec_module(module):
        _call_with_frames_removed(_imp.exec_builtin, module)

    @classmethod
    @_requires_builtin
    def get_code(cls, fullname):
        return None     # built-in modules have no code object

    @classmethod
    @_requires_builtin
    def get_source(cls, fullname):
        return None

    @classmethod
    @_requires_builtin
    def is_package(cls, fullname):
        return False

    load_module = classmethod(_load_module_shim)


# ---------------------------------------------------------------- frozen modules

class FrozenImporter:
    """Meta path finder and loader for frozen modules (code embedded in the
    interpreter).  The spec's loader_state records the original module name
    (``origname``) and the stdlib file the module was frozen from
    (``filename``, None when unknown)."""

    _ORIGIN = 'frozen'

    @classmethod
    def _resolve_filename(cls, fullname, alias=None, ispkg=False):
        """(filename, package directory) of the stdlib source of frozen module
        *fullname* when it is imported as *alias*; (None, None) if unknown."""
        stdlib = getattr(sys, '_stdlib_dir', None)
        if not fullname or not stdlib:
            return None, None
        sep = '\\' if sys.platform == 'win32' else '/'
        if fullname != alias:
            if fullname.startswith('<'):
                # "<pkg": the package itself, frozen from pkg/__init__.py
                fullname = fullname[1:]
                if not ispkg:
                    fullname += '.__init__'
            else:
                # an alias of a plain module
                ispkg = False
        relative = fullname.replace('.', sep)
        if not ispkg:
            return f'{stdlib}{sep}{relative}.py', None
        pkgdir = f'{stdlib}{sep}{relative}'
        return f'{pkgdir}{sep}__init__.py', pkgdir

    @classmethod
    def _fix_up_module(cls, module):
        """Give a module that was executed from frozen code outside of
        find_spec()/exec_module() (start-up, PyImport_ImportFrozenModule) the
        loader_state, __file__ and __path__ a regular frozen import produces."""
        spec = module.__spec__
        if spec.loader_state is not None:
            return
        namespace = vars(module)
        origname = namespace.pop('__origname__', None) or spec.name
        ispkg = hasattr(module, '__path__')
        filename, pkgdir = cls._resolve_filename(origname, spec.name, ispkg)
        spec.loader_state = type(sys.implementation)(filename=filename, origname=origname)
        if ispkg and pkgdir and spec.submodule_search_locations is not None:
            spec.submodule_search_locations.insert(0, pkgdir)
        if filename and namespace.get('__file__') is None:
            _set_module_attr(module, '__file__', filename)
        if ispkg and spec.submodule_search_locations is not None:
            path = module.__path__
            if path is not spec.submodule_search_locations:
                for entry in spec.submodule_search_locations:
                    if entry not in path:
                        path.append(entry)

    @classmethod
    def find_spec(cls, fullname, path=None, target=None):
        found = _call_with_frames_removed(_imp.find_frozen, fullname)
        if found is None:
            return None
        _data, ispkg, origname = found
        spec = spec_from_loader(fullname, cls, origin=cls._ORIGIN, is_package=ispkg)
        filename, pkgdir = cls._resolve_filename(origname, fullname, ispkg)
        spec.loader_state = type(sys.implementation)(filename=filename, origname=origname)
        if pkgdir:
            spec.submodule_search_locations.insert(0, pkgdir)
        return spec

    @staticmethod
    def create_module(spec):
        module = _new_module(spec.name)
        filename = getattr(spec.loader_state, 'filename', None)
        if filename:
            module.__file__ = filename
        return module

    @staticmethod
    def exec_module(module):
        name = module.__spec__.name
        code = _call_with_frames_removed(_imp.get_frozen_object, name)
        exec(code, module.__dict__)

    @classmethod
    def load_module(cls, fullname):
        module = _load_module_shim(cls, fullname)
        _data, ispkg, origname = _imp.find_frozen(fullname)
        module.__origname__ = origname
        vars(module).pop('__file__', None)
        if ispkg:
            module.__path__ = []
        cls._fix_up_module(module)
        return module

    @classmethod
    @_requires_frozen
    def get_code(cls, fullname):
        return _imp.get_frozen_object(fullname)

    @classmethod
    @_requires_frozen
    def get_source(cls, fullname):
        return None     # the source is not kept

    @classmethod
    @_requires_frozen
    def is_package(cls, fullname):
        return _imp.is_frozen_package(fullname)


# ---------------------------------------------------------------- finding and loading

class _ImportLockContext:
    """Holds the global import lock (finders run under it)."""

    def __enter__(self):
        _imp.acquire_lock()

    def __exit__(self, *exc_info):
        _imp.release_lock()


def _resolve_name(name, package, level):
    """Absolute name of the relative import of *name* at *level* from *package*."""
    pieces = package.rsplit('.', level - 1)
    if len(pieces) < level:
        raise ImportError('attempted relative import beyond top-level package')
    base = pieces[0]
    return f'{base}.{name}' if name else base


def _find_spec(name, path, target=None):
    """Ask the finders on sys.meta_path for a spec of *name*."""
    finders = sys.meta_path
    if finders is None:
        raise ImportError("sys.meta_path is None, Python is likely shutting down")
    if not finders:
        _warn('sys.meta_path is empty', ImportWarning)
    # A finder may import the module itself as a side effect; the module's own
    # spec then wins (unless this is a reload).
    reloading = name in sys.modules
    for finder in finders:
        with _ImportLockContext():
            find = getattr(finder, 'find_spec', None)
            if find is None:
                continue
            spec = find(name, path, target)
        if spec is None:
            continue
        if not reloading and name in sys.modules:
            existing = getattr(sys.modules[name], '__spec__', None)
            if existing is not None:
                return existing
        return spec
    return None


def _sanity_check(name, package, level):
    """Validate the arguments of an import."""
    if not isinstance(name, str):
        raise TypeError(f'module name must be str, not {type(name)}')
    if level < 0:
        raise ValueError('level must be >= 0')
    if level > 0:
        if not isinstance(package, str):
            raise TypeError('__package__ not set to a string')
        if not package:
            raise ImportError('attempted relative import with no known parent package')
    if not name and level == 0:
        raise ValueError('Empty module name')


_ERR_MSG_PREFIX = 'No module named '

# marker for "not in sys.modules"
_NEEDS_LOADING = object()


def _find_and_load_unlocked(name, import_):
    """Find and load *name*; the caller holds its module lock."""
    package, _, child = name.rpartition('.')
    search_path = None
    parent_spec = None
    if package:
        if package not in sys.modules:
            _call_with_frames_removed(import_, package)
        # importing the parent may have imported this module too
        if name in sys.modules:
            return sys.modules[name]
        parent_module = sys.modules[package]
        try:
            search_path = parent_module.__path__
        except AttributeError:
            raise ModuleNotFoundError(f'{_ERR_MSG_PREFIX}{name!r}; {package!r} is not a package',
                                      name=name) from None
        parent_spec = getattr(parent_module, '__spec__', None)

    spec = _find_spec(name, search_path)
    if spec is None:
        raise ModuleNotFoundError(f'{_ERR_MSG_PREFIX}{name!r}', name=name)
    # The parent's spec lists submodules that are being imported (for the
    # "partially initialized module" error messages).
    pending = getattr(parent_spec, '_uninitialized_submodules', None)
    if pending is not None:
        pending.append(child)
    try:
        module = _load_unlocked(spec)
    finally:
        if pending is not None:
            pending.pop()

    if package:
        try:
            setattr(sys.modules[package], child, module)
        except AttributeError:
            _warn(f"Cannot set an attribute on {package!r} for child module {child!r}",
                  ImportWarning)
    return module


def _is_initializing(module):
    return getattr(getattr(module, '__spec__', None), '_initializing', False) is True


def _resolve_parents(name, import_):
    """Before *name* is loaded: wait (outermost first) for the packages above
    it that another thread is still initialising, and import the immediate
    parent if it is missing.  Missing packages further up are left alone (a
    hand-made parent in sys.modules needs no ancestors)."""
    parent = name.rpartition('.')[0]
    dot = name.find('.')
    while dot != -1:
        ancestor = name[:dot]
        module = sys.modules.get(ancestor, _NEEDS_LOADING)
        if module is _NEEDS_LOADING:
            if ancestor == parent:
                _call_with_frames_removed(import_, ancestor)
        elif module is not None and _is_initializing(module):
            _lock_unlock_module(ancestor)
        dot = name.find('.', dot + 1)


def _find_and_load(name, import_):
    """Return module *name*, importing it (and its parents) if needed."""
    module = sys.modules.get(name, _NEEDS_LOADING)
    if module is _NEEDS_LOADING or _is_initializing(module):
        if module is _NEEDS_LOADING:
            _resolve_parents(name, import_)
        lock = _get_module_lock(name)
        try:
            lock.acquire()
        except _DeadlockError:
            # Another thread is importing the module and waits for us: use the
            # partially initialised module if there is one, as a single
            # thread would.
            module = sys.modules.get(name, _NEEDS_LOADING)
            if module is _NEEDS_LOADING:
                raise
        else:
            try:
                module = sys.modules.get(name, _NEEDS_LOADING)
                if module is _NEEDS_LOADING:
                    return _find_and_load_unlocked(name, import_)
            finally:
                lock.release()
                _drop_unused_lock(name, lock)
    if module is None:
        raise ModuleNotFoundError(f'import of {name} halted; None in sys.modules', name=name)
    return module


def _gcd_import(name, package=None, level=0):
    """Import and return module *name* (relative to *package* when *level* > 0).

    The common core of import_module() and __import__().
    """
    _sanity_check(name, package, level)
    if level > 0:
        name = _resolve_name(name, package, level)
    return _find_and_load(name, _gcd_import)


def _handle_fromlist(module, fromlist, import_, *, recursive=False):
    """The ``from package import a, b`` part: import submodules named in
    *fromlist* that are not attributes of the package yet."""
    for item in fromlist:
        if not isinstance(item, str):
            where = module.__name__ + '.__all__' if recursive else "``from list''"
            raise TypeError(f"Item in {where} must be str, not {type(item).__name__}")
        if item == '*':
            if not recursive and hasattr(module, '__all__'):
                _handle_fromlist(module, module.__all__, import_, recursive=True)
            continue
        if hasattr(module, item):
            continue
        submodule = f'{module.__name__}.{item}'
        try:
            _call_with_frames_removed(import_, submodule)
        except ModuleNotFoundError as exc:
            # a missing submodule is fine (the attribute lookup reports it),
            # unless it was deliberately blocked with None in sys.modules
            if exc.name == submodule and sys.modules.get(submodule, _NEEDS_LOADING) is not None:
                continue
            raise
    return module


def _calc___package__(globals):
    """The package a relative import in a module with *globals* refers to."""
    package = globals.get('__package__')
    spec = globals.get('__spec__')
    if package is not None:
        if spec is not None and package != spec.parent:
            _warn(f"__package__ != __spec__.parent ({package!r} != {spec.parent!r})",
                  DeprecationWarning, stacklevel=3)
        return package
    if spec is not None:
        return spec.parent
    _warn("can't resolve package from __spec__ or __package__, "
          "falling back on __name__ and __path__", ImportWarning, stacklevel=3)
    package = globals['__name__']
    if '__path__' not in globals:
        package = package.rpartition('.')[0]
    return package


def __import__(name, globals=None, locals=None, fromlist=(), level=0):
    """Import a module (the implementation behind the import statement).

    Without *fromlist* the top-level package of the imported name is
    returned (``import a.b.c`` binds ``a``); with a *fromlist* the named module
    itself, after importing the submodules the fromlist names.  *level*
    selects a relative import, resolved against the package of *globals*.
    """
    if level == 0:
        module = _gcd_import(name)
    else:
        package = _calc___package__(globals if globals is not None else {})
        module = _gcd_import(name, package, level)
    if fromlist:
        if hasattr(module, '__path__'):
            return _handle_fromlist(module, fromlist, _gcd_import)
        return module
    if level == 0:
        return _gcd_import(name.partition('.')[0])
    if not name:
        return module
    # relative "import .a.b" without fromlist: the result is the module of
    # the first component, found through the resolved absolute name
    absolute = _resolve_name(name, package, level)
    first = absolute[:len(absolute) - (len(name) - len(name.partition('.')[0]))]
    try:
        return sys.modules[first]
    except KeyError:
        raise KeyError(f'{first!r} not in sys.modules as expected') from None


# ---------------------------------------------------------------- start-up

def _builtin_from_name(name):
    spec = BuiltinImporter.find_spec(name)
    if spec is None:
        raise ImportError('no built-in module named ' + name)
    return _load_unlocked(spec)


def _setup(sys_module, _imp_module):
    """Bind this module to *sys* and *_imp*, give the modules imported before
    importlib existed a spec, and inject the built-in modules this module
    needs."""
    global _imp, sys, _blocking_on
    _imp = _imp_module
    sys = sys_module

    module_type = type(sys)
    for name, module in list(sys.modules.items()):
        if not isinstance(module, module_type):
            continue
        if name in sys.builtin_module_names:
            loader = BuiltinImporter
        elif _imp.is_frozen(name):
            loader = FrozenImporter
        else:
            continue
        _init_module_attrs(_spec_from_module(module, loader), module)
        if loader is FrozenImporter:
            loader._fix_up_module(module)

    this_module = sys.modules[__name__]
    for builtin_name in ('_thread', '_warnings', '_weakref'):
        if builtin_name in sys.modules:
            builtin = sys.modules[builtin_name]
        elif builtin_name in sys.builtin_module_names:
            builtin = _builtin_from_name(builtin_name)
        else:
            continue
        setattr(this_module, builtin_name, builtin)

    _blocking_on = {}


def _install(sys_module, _imp_module):
    """Install the importers for built-in and frozen modules."""
    _setup(sys_module, _imp_module)
    sys.meta_path.append(BuiltinImporter)
    sys.meta_path.append(FrozenImporter)


def _install_external_importers():
    """Install the path-based importers (filesystem access)."""
    global _bootstrap_external
    import _frozen_importlib_external
    _bootstrap_external = _frozen_importlib_external
    _frozen_importlib_external._install(sys.modules[__name__])
