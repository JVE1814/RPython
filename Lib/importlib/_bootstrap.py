"""The import machinery: specs, module locks, built-in/frozen importers and
the find-and-load algorithm.

Loaded at interpreter start-up as ``_frozen_importlib`` and wired up with
``_install(sys, _imp)``; it may only depend on built-in modules.  Use the
``importlib`` package rather than this module.
"""

def _object_name(obj):
    try:
        return obj.__qualname__
    except AttributeError:
        return type(obj).__qualname__



_thread = None
_warnings = None
_weakref = None

_bootstrap_external = None


def _wrap(new, old):
    for replace in ['__module__', '__name__', '__qualname__', '__doc__']:
        if hasattr(old, replace):
            setattr(new, replace, getattr(old, replace))
    new.__dict__.update(old.__dict__)


def _new_module(name):
    return type(sys)(name)


def _warn(message, category, stacklevel=1):
    if _warnings is not None:
        _warnings.warn(message, category, stacklevel + 1)
    else:
        try:
            import warnings
        except ImportError:
            return
        warnings.warn(message, category, stacklevel + 1)



class _List(list):
    __slots__ = ("__weakref__",)


_module_locks = {}

_blocking_on = {}


class _BlockingOnManager:
    def __init__(self, thread_id, lock):
        self.thread_id = thread_id
        self.lock = lock

    def __enter__(self):
        self.blocked_on = _blocking_on.setdefault(self.thread_id, _List())
        self.blocked_on.append(self.lock)

    def __exit__(self, *args, **kwargs):
        try:
            self.blocked_on.remove(self.lock)
        except ValueError:
            pass
        if not self.blocked_on:
            if _blocking_on.get(self.thread_id) is self.blocked_on:
                try:
                    del _blocking_on[self.thread_id]
                except KeyError:
                    pass


class _DeadlockError(RuntimeError):
    pass


def _has_deadlocked(target_id, *, seen_ids, candidate_ids, blocking_on):
    if target_id in candidate_ids:
        return True

    for tid in candidate_ids:
        if not (candidate_blocking_on := blocking_on.get(tid)):
            continue
        elif tid in seen_ids:
            return False
        seen_ids.add(tid)

        edges = [lock.owner for lock in list(candidate_blocking_on)]
        if _has_deadlocked(target_id, seen_ids=seen_ids, candidate_ids=edges,
                blocking_on=blocking_on):
            return True

    return False


class _ModuleLock:
    def __init__(self, name):
        self.lock = _thread.RLock()
        self.wakeup = _thread.allocate_lock()

        self.name = name

        self.owner = None

        self.count = []

        self.waiters = []

    def has_deadlock(self):
        return _has_deadlocked(
            target_id=_thread.get_ident(),
            seen_ids=set(),
            candidate_ids=[self.owner],
            blocking_on=_blocking_on,
        )

    def acquire(self):
        tid = _thread.get_ident()
        with _BlockingOnManager(tid, self):
            while True:
                with self.lock:
                    if self.count == [] or self.owner == tid:
                        self.owner = tid
                        self.count.append(True)
                        return True


                    if self.has_deadlock():
                        raise _DeadlockError(f'deadlock detected by {self!r}')

                    if self.wakeup.acquire(False):
                        self.waiters.append(None)

                self.wakeup.acquire()

                self.wakeup.release()

    def release(self):
        tid = _thread.get_ident()
        with self.lock:
            if self.owner != tid:
                raise RuntimeError('cannot release un-acquired lock')
            assert len(self.count) > 0
            self.count.pop()
            if not len(self.count):
                self.owner = None
                if len(self.waiters) > 0:
                    self.waiters.pop()
                    self.wakeup.release()

    def locked(self):
        return bool(self.count)

    def __repr__(self):
        return f'_ModuleLock({self.name!r}) at {id(self)}'


class _DummyModuleLock:
    def __init__(self, name):
        self.name = name
        self.count = 0

    def acquire(self):
        self.count += 1
        return True

    def release(self):
        if self.count == 0:
            raise RuntimeError('cannot release un-acquired lock')
        self.count -= 1

    def locked(self):
        return bool(self.count)

    def __repr__(self):
        return f'_DummyModuleLock({self.name!r}) at {id(self)}'


class _ModuleLockManager:

    def __init__(self, name):
        self._name = name
        self._lock = None

    def __enter__(self):
        self._lock = _get_module_lock(self._name)
        self._lock.acquire()

    def __exit__(self, *args, **kwargs):
        self._lock.release()
        _release_module_lock_ref(self._name, self._lock)


def _release_module_lock_ref(name, lock):
    if _weakref is not None:
        return
    _imp.acquire_lock()
    try:
        if (_module_locks.get(name) is lock and not lock.locked()
                and not lock.waiters):
            del _module_locks[name]
    finally:
        _imp.release_lock()



def _get_module_lock(name):
    _imp.acquire_lock()
    try:
        try:
            if _weakref is not None:
                lock = _module_locks[name]()
            else:
                lock = _module_locks[name]
        except KeyError:
            lock = None

        if lock is None:
            if _thread is None:
                lock = _DummyModuleLock(name)
            else:
                lock = _ModuleLock(name)

            if _weakref is not None:
                def cb(ref, name=name):
                    _imp.acquire_lock()
                    try:
                        if _module_locks.get(name) is ref:
                            del _module_locks[name]
                    finally:
                        _imp.release_lock()

                _module_locks[name] = _weakref.ref(lock, cb)
            else:
                _module_locks[name] = lock
    finally:
        _imp.release_lock()

    return lock


def _lock_unlock_module(name):
    lock = _get_module_lock(name)
    try:
        lock.acquire()
    except _DeadlockError:
        pass
    else:
        lock.release()
        _release_module_lock_ref(name, lock)


def _call_with_frames_removed(f, *args, **kwds):
    return f(*args, **kwds)


def _verbose_message(message, *args, verbosity=1):
    if sys.flags.verbose >= verbosity:
        if not message.startswith(('#', 'import ')):
            message = '# ' + message
        print(message.format(*args), file=sys.stderr)


def _requires_builtin(fxn):
    def _requires_builtin_wrapper(self, fullname):
        if fullname not in sys.builtin_module_names:
            raise ImportError(f'{fullname!r} is not a built-in module',
                              name=fullname)
        return fxn(self, fullname)
    _wrap(_requires_builtin_wrapper, fxn)
    return _requires_builtin_wrapper


def _requires_frozen(fxn):
    def _requires_frozen_wrapper(self, fullname):
        if not _imp.is_frozen(fullname):
            raise ImportError(f'{fullname!r} is not a frozen module',
                              name=fullname)
        return fxn(self, fullname)
    _wrap(_requires_frozen_wrapper, fxn)
    return _requires_frozen_wrapper


def _load_module_shim(self, fullname):
    msg = ("the load_module() method is deprecated and slated for removal in "
          "Python 3.12; use exec_module() instead")
    _warn(msg, DeprecationWarning)
    spec = spec_from_loader(fullname, self)
    if fullname in sys.modules:
        module = sys.modules[fullname]
        _exec(spec, module)
        return sys.modules[fullname]
    else:
        return _load(spec)


def _module_repr(module):
    loader = getattr(module, '__loader__', None)
    if spec := getattr(module, "__spec__", None):
        return _module_repr_from_spec(spec)
    try:
        name = module.__name__
    except AttributeError:
        name = '?'
    try:
        filename = module.__file__
    except AttributeError:
        if loader is None:
            return f'<module {name!r}>'
        else:
            return f'<module {name!r} ({loader!r})>'
    else:
        return f'<module {name!r} from {filename!r}>'


class ModuleSpec:
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
        args = [f'name={self.name!r}', f'loader={self.loader!r}']
        if self.origin is not None:
            args.append(f'origin={self.origin!r}')
        if self.submodule_search_locations is not None:
            args.append(f'submodule_search_locations={self.submodule_search_locations}')
        return f'{self.__class__.__name__}({", ".join(args)})'

    def __eq__(self, other):
        smsl = self.submodule_search_locations
        try:
            return (self.name == other.name and
                    self.loader == other.loader and
                    self.origin == other.origin and
                    smsl == other.submodule_search_locations and
                    self.cached == other.cached and
                    self.has_location == other.has_location)
        except AttributeError:
            return NotImplemented

    @property
    def cached(self):
        if self._cached is None and self.origin is not None and self._set_fileattr:
            if _bootstrap_external is None:
                raise NotImplementedError
            self._cached = _bootstrap_external._get_cached(self.origin)
        return self._cached

    @cached.setter
    def cached(self, cached):
        self._cached = cached

    @property
    def parent(self):
        if self.submodule_search_locations is None:
            return self.name.rpartition('.')[0]
        else:
            return self.name

    @property
    def has_location(self):
        return self._set_fileattr

    @has_location.setter
    def has_location(self, value):
        self._set_fileattr = bool(value)


def spec_from_loader(name, loader, *, origin=None, is_package=None):
    if origin is None:
        origin = getattr(loader, '_ORIGIN', None)

    if not origin and hasattr(loader, 'get_filename'):
        if _bootstrap_external is None:
            raise NotImplementedError
        spec_from_file_location = _bootstrap_external.spec_from_file_location

        if is_package is None:
            return spec_from_file_location(name, loader=loader)
        search = [] if is_package else None
        return spec_from_file_location(name, loader=loader,
                                       submodule_search_locations=search)

    if is_package is None:
        if hasattr(loader, 'is_package'):
            try:
                is_package = loader.is_package(name)
            except ImportError:
                is_package = None  # aka, undefined
        else:
            is_package = False

    return ModuleSpec(name, loader, origin=origin, is_package=is_package)


def _spec_from_module(module, loader=None, origin=None):
    try:
        spec = module.__spec__
    except AttributeError:
        pass
    else:
        if spec is not None:
            return spec

    name = module.__name__
    if loader is None:
        try:
            loader = module.__loader__
        except AttributeError:
            pass
    try:
        location = module.__file__
    except AttributeError:
        location = None
    if origin is None:
        if loader is not None:
            origin = getattr(loader, '_ORIGIN', None)
        if not origin and location is not None:
            origin = location
    try:
        cached = module.__cached__
    except AttributeError:
        cached = None
    try:
        submodule_search_locations = list(module.__path__)
    except AttributeError:
        submodule_search_locations = None

    spec = ModuleSpec(name, loader, origin=origin)
    spec._set_fileattr = False if location is None else (origin == location)
    spec.cached = cached
    spec.submodule_search_locations = submodule_search_locations
    return spec


def _init_module_attrs(spec, module, *, override=False):
    if (override or getattr(module, '__name__', None) is None):
        try:
            module.__name__ = spec.name
        except AttributeError:
            pass
    if override or getattr(module, '__loader__', None) is None:
        loader = spec.loader
        if loader is None:
            if spec.submodule_search_locations is not None:
                if _bootstrap_external is None:
                    raise NotImplementedError
                NamespaceLoader = _bootstrap_external.NamespaceLoader

                loader = NamespaceLoader.__new__(NamespaceLoader)
                loader._path = spec.submodule_search_locations
                spec.loader = loader
                module.__file__ = None
        try:
            module.__loader__ = loader
        except AttributeError:
            pass
    if override or getattr(module, '__package__', None) is None:
        try:
            module.__package__ = spec.parent
        except AttributeError:
            pass
    try:
        module.__spec__ = spec
    except AttributeError:
        pass
    if override or getattr(module, '__path__', None) is None:
        if spec.submodule_search_locations is not None:
            try:
                module.__path__ = spec.submodule_search_locations
            except AttributeError:
                pass
    if spec.has_location:
        if override or getattr(module, '__file__', None) is None:
            try:
                module.__file__ = spec.origin
            except AttributeError:
                pass

        if override or getattr(module, '__cached__', None) is None:
            if spec.cached is not None:
                try:
                    module.__cached__ = spec.cached
                except AttributeError:
                    pass
    return module


def module_from_spec(spec):
    module = None
    if hasattr(spec.loader, 'create_module'):
        module = spec.loader.create_module(spec)
    elif hasattr(spec.loader, 'exec_module'):
        raise ImportError('loaders that define exec_module() '
                          'must also define create_module()')
    if module is None:
        module = _new_module(spec.name)
    _init_module_attrs(spec, module)
    return module


def _module_repr_from_spec(spec):
    name = '?' if spec.name is None else spec.name
    if spec.origin is None:
        loader = spec.loader
        if loader is None:
            return f'<module {name!r}>'
        elif (
            _bootstrap_external is not None
            and isinstance(loader, _bootstrap_external.NamespaceLoader)
        ):
            return f'<module {name!r} (namespace) from {list(loader._path)}>'
        else:
            return f'<module {name!r} ({loader!r})>'
    else:
        if spec.has_location:
            return f'<module {name!r} from {spec.origin!r}>'
        else:
            return f'<module {spec.name!r} ({spec.origin})>'


def _exec(spec, module):
    name = spec.name
    with _ModuleLockManager(name):
        if sys.modules.get(name) is not module:
            msg = f'module {name!r} not in sys.modules'
            raise ImportError(msg, name=name)
        try:
            if spec.loader is None:
                if spec.submodule_search_locations is None:
                    raise ImportError('missing loader', name=spec.name)
                _init_module_attrs(spec, module, override=True)
            else:
                _init_module_attrs(spec, module, override=True)
                if not hasattr(spec.loader, 'exec_module'):
                    msg = (f"{_object_name(spec.loader)}.exec_module() not found; "
                           "falling back to load_module()")
                    _warn(msg, ImportWarning)
                    spec.loader.load_module(name)
                else:
                    spec.loader.exec_module(module)
        finally:
            module = sys.modules.pop(spec.name)
            sys.modules[spec.name] = module
    return module


def _load_backward_compatible(spec):
    try:
        spec.loader.load_module(spec.name)
    except:
        if spec.name in sys.modules:
            module = sys.modules.pop(spec.name)
            sys.modules[spec.name] = module
        raise
    module = sys.modules.pop(spec.name)
    sys.modules[spec.name] = module
    if getattr(module, '__loader__', None) is None:
        try:
            module.__loader__ = spec.loader
        except AttributeError:
            pass
    if getattr(module, '__package__', None) is None:
        try:
            module.__package__ = module.__name__
            if not hasattr(module, '__path__'):
                module.__package__ = spec.name.rpartition('.')[0]
        except AttributeError:
            pass
    if getattr(module, '__spec__', None) is None:
        try:
            module.__spec__ = spec
        except AttributeError:
            pass
    return module


def _load_unlocked(spec):
    if spec.loader is not None:
        if not hasattr(spec.loader, 'exec_module'):
            msg = (f"{_object_name(spec.loader)}.exec_module() not found; "
                    "falling back to load_module()")
            _warn(msg, ImportWarning)
            return _load_backward_compatible(spec)

    module = module_from_spec(spec)

    spec._initializing = True
    try:
        sys.modules[spec.name] = module
        try:
            if spec.loader is None:
                if spec.submodule_search_locations is None:
                    raise ImportError('missing loader', name=spec.name)
            else:
                spec.loader.exec_module(module)
        except:
            try:
                del sys.modules[spec.name]
            except KeyError:
                pass
            raise
        module = sys.modules.pop(spec.name)
        sys.modules[spec.name] = module
        _verbose_message('import {!r} # {!r}', spec.name, spec.loader)
    finally:
        spec._initializing = False

    return module

def _load(spec):
    with _ModuleLockManager(spec.name):
        return _load_unlocked(spec)



class BuiltinImporter:

    _ORIGIN = "built-in"

    @classmethod
    def find_spec(cls, fullname, path=None, target=None):
        if _imp.is_builtin(fullname):
            return spec_from_loader(fullname, cls, origin=cls._ORIGIN)
        else:
            return None

    @staticmethod
    def create_module(spec):
        if spec.name not in sys.builtin_module_names:
            raise ImportError(f'{spec.name!r} is not a built-in module',
                              name=spec.name)
        return _call_with_frames_removed(_imp.create_builtin, spec)

    @staticmethod
    def exec_module(module):
        _call_with_frames_removed(_imp.exec_builtin, module)

    @classmethod
    @_requires_builtin
    def get_code(cls, fullname):
        return None

    @classmethod
    @_requires_builtin
    def get_source(cls, fullname):
        return None

    @classmethod
    @_requires_builtin
    def is_package(cls, fullname):
        return False

    load_module = classmethod(_load_module_shim)


class FrozenImporter:

    _ORIGIN = "frozen"

    @classmethod
    def _fix_up_module(cls, module):
        spec = module.__spec__
        state = spec.loader_state
        if state is None:

            origname = vars(module).pop('__origname__', None)
            assert origname, 'see PyImport_ImportFrozenModuleObject()'
            ispkg = hasattr(module, '__path__')
            assert _imp.is_frozen_package(module.__name__) == ispkg, ispkg
            filename, pkgdir = cls._resolve_filename(origname, spec.name, ispkg)
            spec.loader_state = type(sys.implementation)(
                filename=filename,
                origname=origname,
            )
            __path__ = spec.submodule_search_locations
            if ispkg:
                assert __path__ == [], __path__
                if pkgdir:
                    spec.submodule_search_locations.insert(0, pkgdir)
            else:
                assert __path__ is None, __path__

            assert not hasattr(module, '__file__'), module.__file__
            if filename:
                try:
                    module.__file__ = filename
                except AttributeError:
                    pass
            if ispkg:
                if module.__path__ != __path__:
                    assert module.__path__ == [], module.__path__
                    module.__path__.extend(__path__)
        else:
            __path__ = spec.submodule_search_locations
            ispkg = __path__ is not None
            assert sorted(vars(state)) == ['filename', 'origname'], state
            if state.origname:
                (__file__, pkgdir,
                 ) = cls._resolve_filename(state.origname, spec.name, ispkg)
                assert state.filename == __file__, (state.filename, __file__)
                if pkgdir:
                    assert __path__ == [pkgdir], (__path__, pkgdir)
                else:
                    assert __path__ == ([] if ispkg else None), __path__
            else:
                __file__ = None
                assert state.filename is None, state.filename
                assert __path__ == ([] if ispkg else None), __path__
            if __file__:
                assert hasattr(module, '__file__')
                assert module.__file__ == __file__, (module.__file__, __file__)
            else:
                assert not hasattr(module, '__file__'), module.__file__
            if ispkg:
                assert hasattr(module, '__path__')
                assert module.__path__ == __path__, (module.__path__, __path__)
            else:
                assert not hasattr(module, '__path__'), module.__path__
        assert not spec.has_location

    @classmethod
    def _resolve_filename(cls, fullname, alias=None, ispkg=False):
        if not fullname or not getattr(sys, '_stdlib_dir', None):
            return None, None
        try:
            sep = cls._SEP
        except AttributeError:
            sep = cls._SEP = '\\' if sys.platform == 'win32' else '/'

        if fullname != alias:
            if fullname.startswith('<'):
                fullname = fullname[1:]
                if not ispkg:
                    fullname = f'{fullname}.__init__'
            else:
                ispkg = False
        relfile = fullname.replace('.', sep)
        if ispkg:
            pkgdir = f'{sys._stdlib_dir}{sep}{relfile}'
            filename = f'{pkgdir}{sep}__init__.py'
        else:
            pkgdir = None
            filename = f'{sys._stdlib_dir}{sep}{relfile}.py'
        return filename, pkgdir

    @classmethod
    def find_spec(cls, fullname, path=None, target=None):
        info = _call_with_frames_removed(_imp.find_frozen, fullname)
        if info is None:
            return None
        _, ispkg, origname = info
        spec = spec_from_loader(fullname, cls,
                                origin=cls._ORIGIN,
                                is_package=ispkg)
        filename, pkgdir = cls._resolve_filename(origname, fullname, ispkg)
        spec.loader_state = type(sys.implementation)(
            filename=filename,
            origname=origname,
        )
        if pkgdir:
            spec.submodule_search_locations.insert(0, pkgdir)
        return spec

    @staticmethod
    def create_module(spec):
        module = _new_module(spec.name)
        try:
            filename = spec.loader_state.filename
        except AttributeError:
            pass
        else:
            if filename:
                module.__file__ = filename
        return module

    @staticmethod
    def exec_module(module):
        spec = module.__spec__
        name = spec.name
        code = _call_with_frames_removed(_imp.get_frozen_object, name)
        exec(code, module.__dict__)

    @classmethod
    def load_module(cls, fullname):
        module = _load_module_shim(cls, fullname)
        info = _imp.find_frozen(fullname)
        assert info is not None
        _, ispkg, origname = info
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
        return None

    @classmethod
    @_requires_frozen
    def is_package(cls, fullname):
        return _imp.is_frozen_package(fullname)



class _ImportLockContext:

    def __enter__(self):
        _imp.acquire_lock()

    def __exit__(self, exc_type, exc_value, exc_traceback):
        _imp.release_lock()


def _resolve_name(name, package, level):
    bits = package.rsplit('.', level - 1)
    if len(bits) < level:
        raise ImportError('attempted relative import beyond top-level package')
    base = bits[0]
    return f'{base}.{name}' if name else base


def _find_spec(name, path, target=None):
    meta_path = sys.meta_path
    if meta_path is None:
        raise ImportError("sys.meta_path is None, Python is likely "
                          "shutting down")

    if not meta_path:
        _warn('sys.meta_path is empty', ImportWarning)

    is_reload = name in sys.modules
    for finder in meta_path:
        with _ImportLockContext():
            try:
                find_spec = finder.find_spec
            except AttributeError:
                continue
            else:
                spec = find_spec(name, path, target)
        if spec is not None:
            if not is_reload and name in sys.modules:
                module = sys.modules[name]
                try:
                    __spec__ = module.__spec__
                except AttributeError:
                    return spec
                else:
                    if __spec__ is None:
                        return spec
                    else:
                        return __spec__
            else:
                return spec
    else:
        return None


def _sanity_check(name, package, level):
    if not isinstance(name, str):
        raise TypeError(f'module name must be str, not {type(name)}')
    if level < 0:
        raise ValueError('level must be >= 0')
    if level > 0:
        if not isinstance(package, str):
            raise TypeError('__package__ not set to a string')
        elif not package:
            raise ImportError('attempted relative import with no known parent '
                              'package')
    if not name and level == 0:
        raise ValueError('Empty module name')


_ERR_MSG_PREFIX = 'No module named '

def _find_and_load_unlocked(name, import_):
    path = None
    parent = name.rpartition('.')[0]
    parent_spec = None
    if parent:
        if parent not in sys.modules:
            _call_with_frames_removed(import_, parent)
        if name in sys.modules:
            return sys.modules[name]
        parent_module = sys.modules[parent]
        try:
            path = parent_module.__path__
        except AttributeError:
            msg = f'{_ERR_MSG_PREFIX}{name!r}; {parent!r} is not a package'
            raise ModuleNotFoundError(msg, name=name) from None
        parent_spec = getattr(parent_module, '__spec__', None)
        child = name.rpartition('.')[2]
    spec = _find_spec(name, path)
    if spec is None:
        raise ModuleNotFoundError(f'{_ERR_MSG_PREFIX}{name!r}', name=name)
    else:
        if parent_spec is not None and hasattr(parent_spec, '_uninitialized_submodules'):
            parent_spec._uninitialized_submodules.append(child)
        try:
            module = _load_unlocked(spec)
        finally:
            if parent_spec is not None and hasattr(parent_spec, '_uninitialized_submodules'):
                parent_spec._uninitialized_submodules.pop()
    if parent:
        parent_module = sys.modules[parent]
        try:
            setattr(parent_module, child, module)
        except AttributeError:
            msg = f"Cannot set an attribute on {parent!r} for child module {child!r}"
            _warn(msg, ImportWarning)
    return module


_NEEDS_LOADING = object()


def _is_initializing(module):
    spec = getattr(module, '__spec__', None)
    return getattr(spec, '_initializing', False) is True


def _resolve_parents(name, import_):
    parts = name.split('.')
    for i in range(1, len(parts)):
        ancestor = '.'.join(parts[:i])
        module = sys.modules.get(ancestor, _NEEDS_LOADING)
        if module is _NEEDS_LOADING:
            _call_with_frames_removed(import_, ancestor)
            continue
        if module is not None and _is_initializing(module):
            _lock_unlock_module(ancestor)


def _find_and_load(name, import_):
    module = sys.modules.get(name, _NEEDS_LOADING)
    if (module is _NEEDS_LOADING or
        getattr(getattr(module, "__spec__", None), "_initializing", False)):
        if module is _NEEDS_LOADING:
            _resolve_parents(name, import_)
        lock = _get_module_lock(name)
        try:
            lock.acquire()
        except _DeadlockError:
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
                _release_module_lock_ref(name, lock)

    if module is None:
        message = f'import of {name} halted; None in sys.modules'
        raise ModuleNotFoundError(message, name=name)

    return module


def _gcd_import(name, package=None, level=0):
    _sanity_check(name, package, level)
    if level > 0:
        name = _resolve_name(name, package, level)
    return _find_and_load(name, _gcd_import)


def _handle_fromlist(module, fromlist, import_, *, recursive=False):
    for x in fromlist:
        if not isinstance(x, str):
            if recursive:
                where = module.__name__ + '.__all__'
            else:
                where = "``from list''"
            raise TypeError(f"Item in {where} must be str, "
                            f"not {type(x).__name__}")
        elif x == '*':
            if not recursive and hasattr(module, '__all__'):
                _handle_fromlist(module, module.__all__, import_,
                                 recursive=True)
        elif not hasattr(module, x):
            from_name = f'{module.__name__}.{x}'
            try:
                _call_with_frames_removed(import_, from_name)
            except ModuleNotFoundError as exc:
                if (exc.name == from_name and
                    sys.modules.get(from_name, _NEEDS_LOADING) is not None):
                    continue
                raise
    return module


def _calc___package__(globals):
    package = globals.get('__package__')
    spec = globals.get('__spec__')
    if package is not None:
        if spec is not None and package != spec.parent:
            _warn("__package__ != __spec__.parent "
                  f"({package!r} != {spec.parent!r})",
                  DeprecationWarning, stacklevel=3)
        return package
    elif spec is not None:
        return spec.parent
    else:
        _warn("can't resolve package from __spec__ or __package__, "
              "falling back on __name__ and __path__",
              ImportWarning, stacklevel=3)
        package = globals['__name__']
        if '__path__' not in globals:
            package = package.rpartition('.')[0]
    return package


def __import__(name, globals=None, locals=None, fromlist=(), level=0):
    if level == 0:
        module = _gcd_import(name)
    else:
        globals_ = globals if globals is not None else {}
        package = _calc___package__(globals_)
        module = _gcd_import(name, package, level)
    if not fromlist:
        if level == 0:
            return _gcd_import(name.partition('.')[0])
        elif not name:
            return module
        else:
            cut_off = len(name) - len(name.partition('.')[0])
            return sys.modules[module.__name__[:len(module.__name__)-cut_off]]
    elif hasattr(module, '__path__'):
        return _handle_fromlist(module, fromlist, _gcd_import)
    else:
        return module


def _builtin_from_name(name):
    spec = BuiltinImporter.find_spec(name)
    if spec is None:
        raise ImportError('no built-in module named ' + name)
    return _load_unlocked(spec)


def _setup(sys_module, _imp_module):
    global _imp, sys, _blocking_on
    _imp = _imp_module
    sys = sys_module

    module_type = type(sys)
    for name, module in list(sys.modules.items()):
        if isinstance(module, module_type):
            if name in sys.builtin_module_names:
                loader = BuiltinImporter
            elif _imp.is_frozen(name):
                loader = FrozenImporter
            else:
                continue
            spec = _spec_from_module(module, loader)
            _init_module_attrs(spec, module)
            if loader is FrozenImporter:
                loader._fix_up_module(module)

    self_module = sys.modules[__name__]
    for builtin_name in ('_thread', '_warnings', '_weakref'):
        if builtin_name not in sys.modules:
            if builtin_name not in sys.builtin_module_names:
                continue
            builtin_module = _builtin_from_name(builtin_name)
        else:
            builtin_module = sys.modules[builtin_name]
        setattr(self_module, builtin_name, builtin_module)

    _blocking_on = {}


def _install(sys_module, _imp_module):
    _setup(sys_module, _imp_module)

    sys.meta_path.append(BuiltinImporter)
    sys.meta_path.append(FrozenImporter)


def _install_external_importers():
    global _bootstrap_external
    import _frozen_importlib_external
    _bootstrap_external = _frozen_importlib_external
    _frozen_importlib_external._install(sys.modules[__name__])
