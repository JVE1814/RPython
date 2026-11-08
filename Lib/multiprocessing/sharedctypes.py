#
# multiprocessing.sharedctypes -- ctypes objects living in shared memory.
#
# The memory comes from multiprocessing.heap; such objects reach a child
# only as part of starting it (they pickle as "heap block + ctypes type").
# The synchronized wrappers guard every access with a lock.
#

import ctypes
import weakref

from . import heap
from . import get_context

from .context import reduction, assert_spawning
_ForkingPickler = reduction.ForkingPickler

__all__ = ['RawValue', 'RawArray', 'Value', 'Array', 'copy', 'synchronized']

typecode_to_type = {
    'c': ctypes.c_char, 'u': ctypes.c_wchar,
    'b': ctypes.c_byte, 'B': ctypes.c_ubyte,
    'h': ctypes.c_short, 'H': ctypes.c_ushort,
    'i': ctypes.c_int, 'I': ctypes.c_uint,
    'l': ctypes.c_long, 'L': ctypes.c_ulong,
    'q': ctypes.c_longlong, 'Q': ctypes.c_ulonglong,
    'f': ctypes.c_float, 'd': ctypes.c_double,
}


def _ctype_of(typecode_or_type):
    if not isinstance(typecode_or_type, str):
        return typecode_or_type
    try:
        return typecode_to_type[typecode_or_type]
    except KeyError:
        raise TypeError(
            f"bad typecode {typecode_or_type!r} (must be one of "
            f"{''.join(typecode_to_type)} or a ctypes type)") from None


def _allocate(type_, zero=False):
    # An instance of *type_* placed in a fresh block of shared memory.
    obj = rebuild_ctype(type_, heap.BufferWrapper(ctypes.sizeof(type_)), None)
    if zero:
        ctypes.memset(ctypes.addressof(obj), 0, ctypes.sizeof(obj))
    return obj


def RawValue(typecode_or_type, *args):
    """A ctypes object in shared memory, without any locking."""
    obj = _allocate(_ctype_of(typecode_or_type), zero=True)
    obj.__init__(*args)
    return obj


def RawArray(typecode_or_type, size_or_initializer):
    """A ctypes array in shared memory, without any locking.  The second
    argument is either the length (zero-filled) or the initial items."""
    item_type = _ctype_of(typecode_or_type)
    if isinstance(size_or_initializer, int):
        return _allocate(item_type * size_or_initializer, zero=True)
    array = _allocate(item_type * len(size_or_initializer))
    array.__init__(*size_or_initializer)
    return array


def _with_lock(obj, lock, ctx):
    # lock=False: the raw object; True/None: a new RLock; anything else must
    # behave like a lock.
    if lock is False:
        return obj
    if lock in (True, None):
        ctx = ctx or get_context()
        lock = ctx.RLock()
    if not hasattr(lock, 'acquire'):
        raise AttributeError("%r has no method 'acquire'" % lock)
    return synchronized(obj, lock, ctx=ctx)


def Value(typecode_or_type, *args, lock=True, ctx=None):
    """A shared ctypes object, wrapped with a lock unless lock=False."""
    return _with_lock(RawValue(typecode_or_type, *args), lock, ctx)


def Array(typecode_or_type, size_or_initializer, *, lock=True, ctx=None):
    """A shared ctypes array, wrapped with a lock unless lock=False."""
    return _with_lock(RawArray(typecode_or_type, size_or_initializer),
                      lock, ctx)


def copy(obj):
    """A new shared ctypes object holding a copy of *obj*'s contents."""
    duplicate = _allocate(type(obj))
    ctypes.pointer(duplicate)[0] = obj
    return duplicate


def synchronized(obj, lock=None, ctx=None):
    """Wrap the shared ctypes object *obj* so that accesses hold *lock*."""
    assert not isinstance(obj, SynchronizedBase), 'object already synchronized'
    ctx = ctx or get_context()
    if isinstance(obj, ctypes._SimpleCData):
        return Synchronized(obj, lock, ctx)
    if isinstance(obj, ctypes.Array):
        if obj._type_ is ctypes.c_char:
            return SynchronizedString(obj, lock, ctx)
        return SynchronizedArray(obj, lock, ctx)
    return _wrapper_class_for(type(obj))(obj, lock, ctx)


def _wrapper_class_for(struct_type):
    # Structures get a generated wrapper class with one locked property per
    # field, cached per structure type.
    wrapper = class_cache.get(struct_type)
    if wrapper is None:
        fields = {field[0]: make_property(field[0])
                  for field in struct_type._fields_}
        wrapper = type('Synchronized' + struct_type.__name__,
                       (SynchronizedBase,), fields)
        class_cache[struct_type] = wrapper
    return wrapper


# --- pickling --------------------------------------------------------------

def reduce_ctype(obj):
    assert_spawning(obj)
    if isinstance(obj, ctypes.Array):
        return rebuild_ctype, (obj._type_, obj._wrapper, obj._length_)
    return rebuild_ctype, (type(obj), obj._wrapper, None)


def rebuild_ctype(type_, wrapper, length):
    if length is not None:
        type_ = type_ * length
    _ForkingPickler.register(type_, reduce_ctype)
    obj = type_.from_buffer(wrapper.create_memoryview())
    # Keep the shared block alive as long as the object.
    obj._wrapper = wrapper
    return obj


# --- synchronized wrappers -------------------------------------------------

prop_cache = {}
class_cache = weakref.WeakKeyDictionary()


def make_property(name):
    """A property reading/writing attribute *name* of the wrapped object
    while holding the wrapper's lock (one shared property per name)."""
    prop = prop_cache.get(name)
    if prop is None:
        def get(self):
            with self._lock:
                return getattr(self._obj, name)

        def put(self, value):
            with self._lock:
                setattr(self._obj, name, value)

        prop = prop_cache[name] = property(get, put)
    return prop


class SynchronizedBase(object):

    def __init__(self, obj, lock=None, ctx=None):
        self._obj = obj
        if not lock:
            lock = (ctx or get_context()).RLock()
        self._lock = lock
        self.acquire = lock.acquire
        self.release = lock.release

    def __enter__(self):
        return self._lock.__enter__()

    def __exit__(self, *exc_info):
        return self._lock.__exit__(*exc_info)

    def __reduce__(self):
        assert_spawning(self)
        return synchronized, (self._obj, self._lock)

    def get_obj(self):
        return self._obj

    def get_lock(self):
        return self._lock

    def __repr__(self):
        return f'<{type(self).__name__} wrapper for {self._obj}>'


class Synchronized(SynchronizedBase):
    value = make_property('value')


class SynchronizedArray(SynchronizedBase):

    def __len__(self):
        return len(self._obj)

    def __getitem__(self, i):
        with self:
            return self._obj[i]

    def __setitem__(self, i, value):
        with self:
            self._obj[i] = value

    def __getslice__(self, start, stop):
        with self:
            return self._obj[start:stop]

    def __setslice__(self, start, stop, values):
        with self:
            self._obj[start:stop] = values


class SynchronizedString(SynchronizedArray):
    value = make_property('value')
    raw = make_property('raw')
