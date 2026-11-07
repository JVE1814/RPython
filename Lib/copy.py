"""Shallow and deep copies of arbitrary objects.

copy(x)                 a new object of the same kind holding the same
                        references as x (containers are not copied further)
deepcopy(x[, memo])     a new object in which every contained object is
                        copied too; shared and recursive references are
                        reproduced with the help of the memo dictionary
replace(obj, **fields)  a new object with some fields changed, for types
                        that implement __replace__ (named tuples, frozen
                        dataclasses, ...)

Objects take part through __copy__ / __deepcopy__(memo) or, failing that,
through the pickle protocol (copyreg.dispatch_table, __reduce_ex__,
__reduce__, __getstate__/__setstate__).  Problems are reported as
copy.Error.
"""

import types
import weakref
from copyreg import dispatch_table

__all__ = ["Error", "copy", "deepcopy", "replace"]


class Error(Exception):
    pass


error = Error           # older spelling, still in use


# Types whose instances are returned unchanged by copy() and deepcopy().
_IMMUTABLE = frozenset({
    type(None), type(Ellipsis), type(NotImplemented), int, float, bool,
    complex, str, bytes, type, range, property, types.CodeType,
    types.FunctionType, types.BuiltinFunctionType, weakref.ref,
})
# copy() may also share these (their contents are not copied anyway).
_SHALLOW_SHARED = _IMMUTABLE | {tuple, frozenset, slice, super}
try:
    _SHALLOW_SHARED |= {frozendict}
except NameError:
    pass
# Built-in mutable containers with a copy() method.
_HAS_COPY_METHOD = frozenset({list, dict, set, bytearray})


def _reduction(x, cls):
    """The pickle-protocol description of x (a tuple, or a str meaning
    "x is a global and copies to itself")."""
    reducer = dispatch_table.get(cls)
    if reducer is not None:
        return reducer(x)
    method = getattr(x, '__reduce_ex__', None)
    if method is not None:
        return method(4)
    method = getattr(x, '__reduce__', None)
    if method:
        return method()
    raise Error('un(deep)copyable object of type %s' % cls)


def copy(x):
    """Return a shallow copy of x."""
    cls = type(x)
    if cls in _SHALLOW_SHARED:
        return x
    if cls in _HAS_COPY_METHOD:
        return cls.copy(x)
    if issubclass(cls, type):
        return x            # classes (including metaclass instances)
    method = getattr(cls, '__copy__', None)
    if method is not None:
        return method(x)
    rv = _reduction(x, cls)
    if isinstance(rv, str):
        return x
    return _rebuild(x, None, *rv)


def deepcopy(x, memo=None):
    """Return a deep copy of x.

    memo maps id(original) to its copy; pass the same dict to several calls
    to share copies between them.
    """
    cls = type(x)
    if cls in _IMMUTABLE:
        return x
    if memo is None:
        memo = {}
    else:
        done = memo.get(id(x), _MISSING)
        if done is not _MISSING:
            return done

    special = _deepcopy_dispatch.get(cls)
    if special is not None:
        y = special(x, memo)
    elif issubclass(cls, type):
        y = x
    else:
        method = getattr(x, '__deepcopy__', None)
        if method is not None:
            y = method(memo)
        else:
            rv = _reduction(x, cls)
            y = x if isinstance(rv, str) else _rebuild(x, memo, *rv)

    if y is not x:          # objects that are their own copy are not memoised
        memo[id(x)] = y
        _hold(x, memo)
    return y


_MISSING = object()


def _hold(x, memo):
    # Keep the originals alive while the memo is in use, so that their ids
    # cannot be reused by new objects; they are listed under id(memo).
    keep = memo.get(id(memo))
    if keep is None:
        memo[id(memo)] = [x]
    else:
        keep.append(x)


# -- per-type deep copiers: f(x, memo) -> copy ------------------------------

def _copy_list(x, memo):
    result = []
    memo[id(x)] = result            # before recursing, for cycles
    add = result.append
    for item in x:
        add(item if type(item) in _IMMUTABLE else deepcopy(item, memo))
    return result


def _copy_dict(x, memo):
    result = {}
    memo[id(x)] = result
    for key, value in x.items():
        if type(key) not in _IMMUTABLE:
            key = deepcopy(key, memo)
        if type(value) not in _IMMUTABLE:
            value = deepcopy(value, memo)
        result[key] = value
    return result


def _copy_tuple(x, memo):
    items = [item if type(item) in _IMMUTABLE else deepcopy(item, memo)
             for item in x]
    # copying the items may have copied x itself (a cycle through a mutable
    # element); then that copy must be used
    if id(x) in memo:
        return memo[id(x)]
    if all(a is b for a, b in zip(items, x)):
        return x
    return tuple(items)


def _copy_method(x, memo):
    return type(x)(x.__func__, deepcopy(x.__self__, memo))


_deepcopy_dispatch = {
    list: _copy_list,
    dict: _copy_dict,
    tuple: _copy_tuple,
    types.MethodType: _copy_method,
}

try:
    def _copy_frozendict(x, memo):
        pairs = [(deepcopy(k, memo), deepcopy(v, memo)) for k, v in x.items()]
        if id(x) in memo:           # see _copy_tuple
            return memo[id(x)]
        if all(k1 is k2 and v1 is v2
               for (k1, v1), (k2, v2) in zip(pairs, x.items())):
            return x
        return frozendict(pairs)
    _deepcopy_dispatch[frozendict] = _copy_frozendict
except NameError:
    pass


def _rebuild(x, memo, func, args, state=None, listiter=None, dictiter=None):
    """Create the copy from a reduce tuple; deep when memo is not None."""
    deep = memo is not None
    if deep and args:
        args = deepcopy(args, memo)
    y = func(*args)
    if deep:
        memo[id(x)] = y
    if state is not None:
        if deep:
            state = deepcopy(state, memo)
        setstate = getattr(y, '__setstate__', None)
        if setstate is not None:
            setstate(state)
        else:
            slots = None
            if isinstance(state, tuple) and len(state) == 2:
                state, slots = state
            if state is not None:
                y.__dict__.update(state)
            if slots is not None:
                for name, value in slots.items():
                    setattr(y, name, value)
    if listiter is not None:
        for item in listiter:
            y.append(deepcopy(item, memo) if deep else item)
    if dictiter is not None:
        for key, value in dictiter:
            if deep:
                key = deepcopy(key, memo)
                value = deepcopy(value, memo)
            y[key] = value
    return y


def replace(obj, /, **changes):
    """Return a copy of obj with the given fields replaced (via the type's
    __replace__ method)."""
    cls = type(obj)
    method = getattr(cls, '__replace__', None)
    if method is None:
        raise TypeError(f"replace() does not support {cls.__name__} objects")
    return method(obj, **changes)
