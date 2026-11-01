"""Weak references and weak containers (PEP 205)."""

from _weakref import (getweakrefcount, getweakrefs, ref, proxy,
                      CallableProxyType, ProxyType, ReferenceType,
                      _remove_dead_weakref)
from _weakrefset import WeakSet, _IterationGuard

import _collections_abc
import sys
import itertools

ProxyTypes = (ProxyType, CallableProxyType)

__all__ = ["ref", "proxy", "getweakrefcount", "getweakrefs",
           "WeakKeyDictionary", "ReferenceType", "ProxyType",
           "CallableProxyType", "ProxyTypes", "WeakValueDictionary",
           "WeakSet", "WeakMethod", "finalize"]

_collections_abc.MutableSet.register(WeakSet)


class WeakMethod(ref):
    """A weak reference to a bound method.

    Holds weak references to both the instance and the function, so the
    reference stays valid exactly as long as both are alive.
    """

    __slots__ = "_func_ref", "_meth_type", "_alive", "__weakref__"

    def __new__(cls, meth, callback=None):
        try:
            instance = meth.__self__
            function = meth.__func__
        except AttributeError:
            raise TypeError(f"argument should be a bound method, "
                            f"not {type(meth)}") from None

        def on_death(_unused):
            this = this_ref()
            if this._alive:
                this._alive = False
                if callback is not None:
                    callback(this)

        self = ref.__new__(cls, instance, on_death)
        self._func_ref = ref(function, on_death)
        self._meth_type = type(meth)
        self._alive = True
        this_ref = ref(self)
        return self

    def __call__(self):
        instance = super().__call__()
        function = self._func_ref()
        if instance is None or function is None:
            return None
        return self._meth_type(function, instance)

    def __eq__(self, other):
        if not isinstance(other, WeakMethod):
            return NotImplemented
        if self._alive and other._alive:
            return ref.__eq__(self, other) and self._func_ref == other._func_ref
        return self is other

    def __ne__(self, other):
        if not isinstance(other, WeakMethod):
            return NotImplemented
        if self._alive and other._alive:
            return ref.__ne__(self, other) or self._func_ref != other._func_ref
        return self is not other

    __hash__ = ref.__hash__


class _DeferredRemovals:
    """Mixin for weak mappings: while an iteration guard is active, entries
    whose referent died are queued and removed once iteration finishes."""

    def _flush(self):
        if self._pending_removals:
            self._commit_removals()


class WeakValueDictionary(_DeferredRemovals, _collections_abc.MutableMapping):
    """Mapping whose values are held through weak references.

    An entry disappears once its value is no longer strongly referenced.
    """

    def __init__(self, other=(), /, **kw):
        def on_value_death(wr, owner_ref=ref(self), drop=_remove_dead_weakref):
            owner = owner_ref()
            if owner is None:
                return
            if owner._iterating:
                owner._pending_removals.append(wr.key)
            else:
                drop(owner.data, wr.key)
        self._remove = on_value_death
        self._pending_removals = []
        self._iterating = set()
        self.data = {}
        self.update(other, **kw)

    def _commit_removals(self, drop=_remove_dead_weakref):
        queue = self._pending_removals
        data = self.data
        while queue:
            drop(data, queue.pop())

    def __getitem__(self, key):
        self._flush()
        value = self.data[key]()
        if value is None:
            raise KeyError(key)
        return value

    def __delitem__(self, key):
        self._flush()
        del self.data[key]

    def __len__(self):
        self._flush()
        return len(self.data)

    def __contains__(self, key):
        self._flush()
        try:
            wr = self.data[key]
        except KeyError:
            return False
        return wr() is not None

    def __repr__(self):
        return f"<{self.__class__.__name__} at {id(self):#x}>"

    def __setitem__(self, key, value):
        self._flush()
        self.data[key] = KeyedRef(value, self._remove, key)

    def _live_items(self):
        with _IterationGuard(self):
            for key, wr in self.data.items():
                value = wr()
                if value is not None:
                    yield key, value

    def copy(self):
        self._flush()
        result = WeakValueDictionary()
        for key, value in self._live_items():
            result[key] = value
        return result

    __copy__ = copy

    def __deepcopy__(self, memo):
        from copy import deepcopy
        self._flush()
        result = self.__class__()
        for key, value in self._live_items():
            result[deepcopy(key, memo)] = value
        return result

    def get(self, key, default=None):
        self._flush()
        try:
            wr = self.data[key]
        except KeyError:
            return default
        value = wr()
        return default if value is None else value

    def items(self):
        self._flush()
        yield from self._live_items()

    def keys(self):
        self._flush()
        for key, _value in self._live_items():
            yield key

    __iter__ = keys

    def itervaluerefs(self):
        """Iterate over the weak references to the values (which may be dead)."""
        self._flush()
        with _IterationGuard(self):
            yield from self.data.values()

    def values(self):
        self._flush()
        for _key, value in self._live_items():
            yield value

    def valuerefs(self):
        """Return a list of the weak references to the values (which may be dead)."""
        self._flush()
        return list(self.data.values())

    def popitem(self):
        self._flush()
        while True:
            key, wr = self.data.popitem()
            value = wr()
            if value is not None:
                return key, value

    def pop(self, key, *default):
        self._flush()
        try:
            value = self.data.pop(key)()
        except KeyError:
            value = None
        if value is not None:
            return value
        if default:
            return default[0]
        raise KeyError(key)

    def setdefault(self, key, default=None):
        try:
            value = self.data[key]()
        except KeyError:
            value = None
        if value is not None:
            return value
        self._flush()
        self.data[key] = KeyedRef(default, self._remove, key)
        return default

    def update(self, other=None, /, **kwargs):
        self._flush()
        data = self.data
        if other is not None:
            if not hasattr(other, "items"):
                other = dict(other)
            for key, value in other.items():
                data[key] = KeyedRef(value, self._remove, key)
        for key, value in kwargs.items():
            data[key] = KeyedRef(value, self._remove, key)

    def __ior__(self, other):
        self.update(other)
        return self

    def __or__(self, other):
        if not isinstance(other, _collections_abc.Mapping):
            return NotImplemented
        result = self.copy()
        result.update(other)
        return result

    def __ror__(self, other):
        if not isinstance(other, _collections_abc.Mapping):
            return NotImplemented
        result = self.__class__()
        result.update(other)
        result.update(self)
        return result


class KeyedRef(ref):
    """A weak reference that also remembers the dictionary key it belongs to,
    so that one shared callback can remove the right entry."""

    __slots__ = "key",

    def __new__(type, ob, callback, key):
        self = ref.__new__(type, ob, callback)
        self.key = key
        return self

    def __init__(self, ob, callback, key):
        super().__init__(ob, callback)


class WeakKeyDictionary(_DeferredRemovals, _collections_abc.MutableMapping):
    """Mapping whose keys are held through weak references.

    An entry disappears once its key is no longer strongly referenced.
    """

    def __init__(self, dict=None):
        self.data = {}

        def on_key_death(wr, owner_ref=ref(self)):
            owner = owner_ref()
            if owner is None:
                return
            if owner._iterating:
                owner._pending_removals.append(wr)
            else:
                owner.data.pop(wr, None)
        self._remove = on_key_death
        self._pending_removals = []
        self._iterating = set()
        self._dirty_len = False
        if dict is not None:
            self.update(dict)

    def _commit_removals(self):
        # Dead references never compare equal to live ones, so queued
        # removals cannot hit a live entry; some may already be gone.
        queue = self._pending_removals
        data = self.data
        while queue:
            data.pop(queue.pop(), None)

    def _scrub_removals(self):
        data = self.data
        self._pending_removals = [wr for wr in self._pending_removals if wr in data]
        self._dirty_len = False

    def __delitem__(self, key):
        self._dirty_len = True
        del self.data[ref(key)]

    def __getitem__(self, key):
        return self.data[ref(key)]

    def __len__(self):
        if self._dirty_len and self._pending_removals:
            # explicit deletions may have removed queued entries already
            self._scrub_removals()
        return len(self.data) - len(self._pending_removals)

    def __repr__(self):
        return f"<{self.__class__.__name__} at {id(self):#x}>"

    def __setitem__(self, key, value):
        self.data[ref(key, self._remove)] = value

    def _live_items(self):
        with _IterationGuard(self):
            for wr, value in self.data.items():
                key = wr()
                if key is not None:
                    yield key, value

    def copy(self):
        result = WeakKeyDictionary()
        for key, value in self._live_items():
            result[key] = value
        return result

    __copy__ = copy

    def __deepcopy__(self, memo):
        from copy import deepcopy
        result = self.__class__()
        for key, value in self._live_items():
            result[key] = deepcopy(value, memo)
        return result

    def get(self, key, default=None):
        return self.data.get(ref(key), default)

    def __contains__(self, key):
        try:
            wr = ref(key)
        except TypeError:
            return False
        return wr in self.data

    def items(self):
        yield from self._live_items()

    def keys(self):
        with _IterationGuard(self):
            for wr in self.data:
                key = wr()
                if key is not None:
                    yield key

    __iter__ = keys

    def values(self):
        for _key, value in self._live_items():
            yield value

    def keyrefs(self):
        """Return a list of the weak references to the keys (which may be dead)."""
        return list(self.data)

    def popitem(self):
        self._dirty_len = True
        while True:
            wr, value = self.data.popitem()
            key = wr()
            if key is not None:
                return key, value

    def pop(self, key, *default):
        self._dirty_len = True
        return self.data.pop(ref(key), *default)

    def setdefault(self, key, default=None):
        return self.data.setdefault(ref(key, self._remove), default)

    def update(self, dict=None, /, **kwargs):
        data = self.data
        if dict is not None:
            if not hasattr(dict, "items"):
                dict = type({})(dict)
            for key, value in dict.items():
                data[ref(key, self._remove)] = value
        if kwargs:
            # keyword names are strings, which cannot be weakly referenced
            raise TypeError("Keyword arguments are not supported: "
                            "cannot create weak reference to 'str' object")

    def __ior__(self, other):
        self.update(other)
        return self

    def __or__(self, other):
        if not isinstance(other, _collections_abc.Mapping):
            return NotImplemented
        result = self.copy()
        result.update(other)
        return result

    def __ror__(self, other):
        if not isinstance(other, _collections_abc.Mapping):
            return NotImplemented
        result = self.__class__()
        result.update(other)
        result.update(self)
        return result


class finalize:
    """finalize(obj, func, /, *args, **kwargs)

    A callable that runs func(*args, **kwargs) once, either when called
    explicitly or when obj is garbage collected, whichever happens first.
    Finalizers still alive at interpreter exit whose `atexit` attribute is
    true are run then, most recently created first.
    """

    # Instances carry no state: everything lives in _registry, keyed by the
    # finalizer, so finalizers can never be part of a reference cycle.
    __slots__ = ()
    _registry = {}
    _shutdown = False
    _index_iter = itertools.count()
    _dirty = False
    _registered_with_atexit = False

    class _Info:
        __slots__ = ("weakref", "func", "args", "kwargs", "atexit", "index")

    def __init__(self, obj, func, /, *args, **kwargs):
        if not finalize._registered_with_atexit:
            import atexit
            atexit.register(self._exitfunc)
            finalize._registered_with_atexit = True
        info = self._Info()
        info.weakref = ref(obj, self)
        info.func = func
        info.args = args
        info.kwargs = kwargs or None
        info.atexit = True
        info.index = next(self._index_iter)
        self._registry[self] = info
        finalize._dirty = True

    def __call__(self, _=None):
        """Run the callback if still alive (and return its result)."""
        info = self._registry.pop(self, None)
        if info and not self._shutdown:
            return info.func(*info.args, **(info.kwargs or {}))

    def detach(self):
        """Unregister without calling; return (obj, func, args, kwargs) if alive."""
        info = self._registry.get(self)
        obj = info and info.weakref()
        if obj is not None and self._registry.pop(self, None):
            return (obj, info.func, info.args, info.kwargs or {})

    def peek(self):
        """Return (obj, func, args, kwargs) if alive, else None."""
        info = self._registry.get(self)
        obj = info and info.weakref()
        if obj is not None:
            return (obj, info.func, info.args, info.kwargs or {})

    @property
    def alive(self):
        """True until the finalizer has been called or detached."""
        return self in self._registry

    @property
    def atexit(self):
        """Whether the finalizer runs at interpreter exit."""
        info = self._registry.get(self)
        return bool(info) and info.atexit

    @atexit.setter
    def atexit(self, value):
        info = self._registry.get(self)
        if info:
            info.atexit = bool(value)

    def __repr__(self):
        info = self._registry.get(self)
        obj = info and info.weakref()
        name = type(self).__name__
        if obj is None:
            return f'<{name} object at {id(self):#x}; dead>'
        return (f'<{name} object at {id(self):#x}; for '
                f'{type(obj).__name__!r} at {id(obj):#x}>')

    @classmethod
    def _select_for_exit(cls):
        # live finalizers marked for exit, in creation order
        chosen = [(f, i) for f, i in cls._registry.items() if i.atexit]
        chosen.sort(key=lambda pair: pair[1].index)
        return [f for f, _ in chosen]

    @classmethod
    def _exitfunc(cls):
        gc_was_enabled = False
        try:
            if cls._registry:
                import gc
                if gc.isenabled():
                    gc_was_enabled = True
                    gc.disable()
                todo = None
                while True:
                    if todo is None or finalize._dirty:
                        todo = cls._select_for_exit()
                        finalize._dirty = False
                    if not todo:
                        break
                    fin = todo.pop()
                    try:
                        fin()
                    except Exception:
                        sys.excepthook(*sys.exc_info())
        finally:
            finalize._shutdown = True
            if gc_was_enabled:
                gc.enable()
