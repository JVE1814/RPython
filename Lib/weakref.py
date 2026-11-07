"""Weak references and containers built on them.

A weak reference refers to an object without keeping it alive.  Besides the
primitives from ``_weakref`` (ref, proxy, getweakrefcount, getweakrefs) this
module provides WeakMethod, the weak containers WeakKeyDictionary,
WeakValueDictionary and WeakSet, and finalize for running cleanup code when
an object dies.

Iteration over the weak dictionaries works on a snapshot of their entries,
so entries may disappear (their referent dies) while an iteration is in
progress without disturbing it; entries whose referent died are skipped.
"""

from _weakref import (getweakrefcount, getweakrefs, ref, proxy,
                      CallableProxyType, ProxyType, ReferenceType,
                      _remove_dead_weakref)

from _weakrefset import WeakSet

import _collections_abc
import itertools
import sys

ProxyTypes = (ProxyType, CallableProxyType)

# _weakrefset is loaded too early to know the ABCs; register WeakSet here.
_collections_abc.MutableSet.register(WeakSet)

__all__ = ["ref", "proxy", "getweakrefcount", "getweakrefs",
           "WeakKeyDictionary", "ReferenceType", "ProxyType",
           "CallableProxyType", "ProxyTypes", "WeakValueDictionary",
           "WeakSet", "WeakMethod", "finalize"]


# --------------------------------------------------------------------------
# WeakMethod

class _MethodDeathNotice:
    """Callback shared by the two weak references inside a WeakMethod (to
    the instance and to the function).  The first death marks the
    WeakMethod dead and runs the user's callback; later ones do nothing.
    It refers to its WeakMethod only weakly, so no cycle is created."""

    __slots__ = ('owner', 'callback')

    def __init__(self, callback):
        self.owner = None
        self.callback = callback

    def __call__(self, _dead_ref):
        method_ref = self.owner() if self.owner is not None else None
        if method_ref is None or not method_ref._alive:
            return
        method_ref._alive = False
        if self.callback is not None:
            self.callback(method_ref)


class WeakMethod(ref):
    """A weak reference to a bound method.

    A plain ref to a bound method dies at once, since the method object is
    created on each attribute access.  WeakMethod instead refers weakly to
    the instance and to the function and rebuilds the bound method when
    called; it returns None once either of them is gone.
    """

    __slots__ = ('_func_ref', '_meth_type', '_alive', '__weakref__')

    def __new__(cls, meth, callback=None):
        try:
            instance = meth.__self__
            function = meth.__func__
        except AttributeError:
            raise TypeError(f'argument should be a bound method, '
                            f'not {type(meth)}') from None
        notice = _MethodDeathNotice(callback)
        self = ref.__new__(cls, instance, notice)
        self._func_ref = ref(function, notice)
        self._meth_type = type(meth)
        self._alive = True
        notice.owner = ref(self)
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
        # a dead WeakMethod is only equal to itself
        return self is other

    def __ne__(self, other):
        if not isinstance(other, WeakMethod):
            return NotImplemented
        if self._alive and other._alive:
            return ref.__ne__(self, other) or self._func_ref != other._func_ref
        return self is not other

    __hash__ = ref.__hash__


# --------------------------------------------------------------------------
# weak dictionaries

class KeyedRef(ref):
    """A weak reference that also remembers a key.

    WeakValueDictionary stores its values through KeyedRefs, so that the
    removal callback knows which entry to drop.
    """

    __slots__ = ('key',)

    def __new__(cls, ob, callback, key):
        self = ref.__new__(cls, ob, callback)
        self.key = key
        return self

    def __init__(self, ob, callback, key):
        super().__init__(ob, callback)


# The removal callbacks are plain functions whose only link to the
# dictionary is a weak reference passed as a default argument: no closure,
# so a dictionary never ends up in a reference cycle with its callback.

def _value_remover(owner_ref):
    def remove(dead, owner_ref=owner_ref):
        owner = owner_ref()
        if owner is not None:
            # Deletes the entry only if it still holds this dead reference:
            # the key may meanwhile have been given a new value.
            _remove_dead_weakref(owner.data, dead.key)
    return remove


def _key_remover(owner_ref):
    def remove(dead, owner_ref=owner_ref):
        owner = owner_ref()
        if owner is not None:
            owner.data.pop(dead, None)
    return remove


def _items_of(source):
    """(key, value) pairs of a mapping or of an iterable of pairs."""
    if hasattr(source, 'items'):
        return source.items()
    return dict(source).items()


class WeakValueDictionary(_collections_abc.MutableMapping):
    """Mapping whose values are weak references: an entry disappears
    when its value is no longer strongly referenced elsewhere."""

    def __init__(self, other=(), /, **kw):
        self._remove = _value_remover(ref(self))
        self.data = {}           # key -> KeyedRef to the value
        self.update(other, **kw)

    def __getitem__(self, key):
        value = self.data[key]()
        if value is None:
            raise KeyError(key)
        return value

    def __setitem__(self, key, value):
        self.data[key] = KeyedRef(value, self._remove, key)

    def __delitem__(self, key):
        del self.data[key]

    def __len__(self):
        return len(self.data)

    def __contains__(self, key):
        try:
            value_ref = self.data[key]
        except KeyError:
            return False
        return value_ref() is not None

    def __repr__(self):
        return f'<{type(self).__name__} at {id(self):#x}>'

    def get(self, key, default=None):
        value_ref = self.data.get(key)
        if value_ref is None:
            return default
        value = value_ref()
        return default if value is None else value

    def keys(self):
        for key, value_ref in tuple(self.data.items()):
            if value_ref() is not None:
                yield key

    __iter__ = keys

    def values(self):
        for value_ref in tuple(self.data.values()):
            value = value_ref()
            if value is not None:
                yield value

    def items(self):
        for key, value_ref in tuple(self.data.items()):
            value = value_ref()
            if value is not None:
                yield key, value

    def itervaluerefs(self):
        """Iterate over the KeyedRefs to the values.

        A reference may be dead by the time it is used, so callers must
        check what calling it returns.
        """
        yield from tuple(self.data.values())

    def valuerefs(self):
        """Return a list of the KeyedRefs to the values (see itervaluerefs)."""
        return list(self.data.values())

    def pop(self, key, *default):
        if len(default) > 1:
            raise TypeError(f'pop expected at most 2 arguments, '
                            f'got {1 + len(default)}')
        value_ref = self.data.pop(key, None)
        value = value_ref() if value_ref is not None else None
        if value is None:
            if default:
                return default[0]
            raise KeyError(key)
        return value

    def popitem(self):
        while True:
            key, value_ref = self.data.popitem()
            value = value_ref()
            if value is not None:
                return key, value

    def setdefault(self, key, default=None):
        value_ref = self.data.get(key)
        value = value_ref() if value_ref is not None else None
        if value is None:
            self.data[key] = KeyedRef(default, self._remove, key)
            return default
        return value

    def update(self, other=None, /, **kwargs):
        data = self.data
        remove = self._remove
        for source in (other, kwargs):
            if source is None:
                continue
            for key, value in _items_of(source):
                data[key] = KeyedRef(value, remove, key)

    def copy(self):
        clone = type(self)()
        for key, value in self.items():
            clone[key] = value
        return clone

    __copy__ = copy

    def __deepcopy__(self, memo):
        from copy import deepcopy
        clone = type(self)()
        for key, value in self.items():
            clone[deepcopy(key, memo)] = value
        return clone

    def __or__(self, other):
        if not isinstance(other, _collections_abc.Mapping):
            return NotImplemented
        result = self.copy()
        result.update(other)
        return result

    def __ror__(self, other):
        if not isinstance(other, _collections_abc.Mapping):
            return NotImplemented
        result = type(self)()
        result.update(other)
        result.update(self)
        return result

    def __ior__(self, other):
        self.update(other)
        return self


class WeakKeyDictionary(_collections_abc.MutableMapping):
    """Mapping whose keys are weak references: an entry disappears when
    its key is no longer strongly referenced elsewhere.

    Keys must be hashable and weak-referenceable; equal keys are the same
    key, as in a dict.
    """

    def __init__(self, dict=None):
        self.data = {}           # ref to the key -> value
        self._remove = _key_remover(ref(self))
        if dict is not None:
            self.update(dict)

    def __getitem__(self, key):
        return self.data[ref(key)]

    def __setitem__(self, key, value):
        self.data[ref(key, self._remove)] = value

    def __delitem__(self, key):
        del self.data[ref(key)]

    def __len__(self):
        return len(self.data)

    def __contains__(self, key):
        try:
            key_ref = ref(key)
        except TypeError:
            return False
        return key_ref in self.data

    def __repr__(self):
        return f'<{type(self).__name__} at {id(self):#x}>'

    def get(self, key, default=None):
        return self.data.get(ref(key), default)

    def keys(self):
        for key_ref in tuple(self.data):
            key = key_ref()
            if key is not None:
                yield key

    __iter__ = keys

    def values(self):
        for key_ref, value in tuple(self.data.items()):
            if key_ref() is not None:
                yield value

    def items(self):
        for key_ref, value in tuple(self.data.items()):
            key = key_ref()
            if key is not None:
                yield key, value

    def keyrefs(self):
        """Return a list of weak references to the keys.

        A reference may be dead by the time it is used, so callers must
        check what calling it returns.
        """
        return list(self.data)

    def pop(self, key, *default):
        return self.data.pop(ref(key), *default)

    def popitem(self):
        while True:
            key_ref, value = self.data.popitem()
            key = key_ref()
            if key is not None:
                return key, value

    def setdefault(self, key, default=None):
        return self.data.setdefault(ref(key, self._remove), default)

    def update(self, dict=None, /, **kwargs):
        data = self.data
        remove = self._remove
        if dict is not None:
            for key, value in _items_of(dict):
                data[ref(key, remove)] = value
        if kwargs:
            # Keyword names are strings, which cannot be weakly referenced.
            try:
                ref(next(iter(kwargs)))
            except TypeError as exc:
                raise TypeError(f'Keyword arguments are not supported: '
                                f'{exc}') from None
            self.update(kwargs)

    def copy(self):
        clone = type(self)()
        for key, value in self.items():
            clone[key] = value
        return clone

    __copy__ = copy

    def __deepcopy__(self, memo):
        from copy import deepcopy
        clone = type(self)()
        for key, value in self.items():
            clone[key] = deepcopy(value, memo)
        return clone

    def __or__(self, other):
        if not isinstance(other, _collections_abc.Mapping):
            return NotImplemented
        result = self.copy()
        result.update(other)
        return result

    def __ror__(self, other):
        if not isinstance(other, _collections_abc.Mapping):
            return NotImplemented
        result = type(self)()
        result.update(other)
        result.update(self)
        return result

    def __ior__(self, other):
        self.update(other)
        return self


# --------------------------------------------------------------------------
# finalize

class _Finalizer:
    """What a finalize object stands for while it is alive."""

    __slots__ = ('target', 'func', 'args', 'kwargs', 'atexit', 'serial')


class finalize:
    """finalize(obj, func, /, *args, **kwargs)

    Arrange for func(*args, **kwargs) to be called once, when obj is
    garbage collected -- or earlier, if the finalize object is called.  By
    default finalizers that are still alive when the interpreter exits are
    run at exit, newest first; set .atexit to False to skip that.
    """

    __slots__ = ()

    # Live finalizers and what they will run.  Being a key here keeps a
    # finalize object alive even when the caller drops it.
    _live = {}
    _serials = itertools.count()
    _exit_hook_installed = False
    # True after the exit pass: finalizers triggered later (by collections
    # during interpreter teardown) no longer run.
    _done = False
    # Bumped on every registration so the exit pass notices finalizers
    # created by other finalizers.
    _generation = 0

    def __init__(self, obj, func, /, *args, **kwargs):
        cls = finalize
        if not cls._exit_hook_installed:
            import atexit
            atexit.register(cls._run_at_exit)
            cls._exit_hook_installed = True
        entry = _Finalizer()
        entry.target = ref(obj, self)
        entry.func = func
        entry.args = args
        entry.kwargs = kwargs or None
        entry.atexit = True
        entry.serial = next(cls._serials)
        cls._live[self] = entry
        cls._generation += 1

    def __call__(self, _=None):
        """Run the finalizer now, if it is still alive; return its result."""
        entry = finalize._live.pop(self, None)
        if entry is not None and not finalize._done:
            return entry.func(*entry.args, **(entry.kwargs or {}))

    def detach(self):
        """Disarm the finalizer; return (obj, func, args, kwargs) if it was
        alive, else None."""
        entry = finalize._live.get(self)
        target = entry.target() if entry is not None else None
        if target is not None and finalize._live.pop(self, None) is not None:
            return (target, entry.func, entry.args, entry.kwargs or {})

    def peek(self):
        """Return (obj, func, args, kwargs) if the finalizer is alive,
        else None."""
        entry = finalize._live.get(self)
        target = entry.target() if entry is not None else None
        if target is not None:
            return (target, entry.func, entry.args, entry.kwargs or {})

    @property
    def alive(self):
        """Whether the finalizer has yet to run (and was not detached)."""
        return self in finalize._live

    @property
    def atexit(self):
        """Whether the finalizer runs at interpreter exit if still alive."""
        entry = finalize._live.get(self)
        return entry is not None and entry.atexit

    @atexit.setter
    def atexit(self, value):
        entry = finalize._live.get(self)
        if entry is not None:
            entry.atexit = bool(value)

    def __repr__(self):
        entry = finalize._live.get(self)
        target = entry.target() if entry is not None else None
        if target is None:
            return f'<{type(self).__name__} object at {id(self):#x}; dead>'
        return (f'<{type(self).__name__} object at {id(self):#x}; for '
                f'{type(target).__name__!r} at {id(target):#x}>')

    @classmethod
    def _run_at_exit(cls):
        # Run the finalizers flagged for exit, newest first.  The cyclic
        # collector is paused meanwhile so that it cannot fire finalizers
        # out of that order.
        try:
            if not cls._live:
                return
            import gc
            collector_was_on = gc.isenabled()
            gc.disable()
            try:
                queue = []
                seen_generation = None
                while True:
                    if seen_generation != cls._generation:
                        seen_generation = cls._generation
                        queue = sorted(
                            (entry.serial, fin)
                            for fin, entry in list(cls._live.items())
                            if entry.atexit)
                    if not queue:
                        break
                    _, fin = queue.pop()
                    entry = cls._live.get(fin)
                    if entry is None or not entry.atexit:
                        continue
                    try:
                        fin()
                    except Exception:
                        sys.excepthook(*sys.exc_info())
            finally:
                if collector_was_on:
                    gc.enable()
        finally:
            cls._done = True
