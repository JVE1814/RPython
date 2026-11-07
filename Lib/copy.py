"""Generic (shallow and deep) copying operations.

Interface summary:

        import copy

        x = copy.copy(y)                # make a shallow copy of y
        x = copy.deepcopy(y)            # make a deep copy of y
        x = copy.replace(y, a=1, b=2)   # new object with fields replaced

For module specific errors, copy.Error is raised.

A shallow copy constructs a new compound object and inserts references to
the objects found in the original; a deep copy inserts copies of them,
recursively, using a memo dictionary to handle shared and recursive
structures.

Classes can customise copying with __copy__() / __deepcopy__(memo), or
through the pickle protocol (__reduce_ex__, __reduce__, __getstate__,
__setstate__, __getnewargs__ ...).
"""

import types
import weakref
from copyreg import dispatch_table


class Error(Exception):
    pass
error = Error   # backward compatibility

__all__ = ["Error", "copy", "deepcopy", "replace"]


def copy(x):
    """Shallow copy operation on arbitrary Python objects.

    See the module's __doc__ string for more info.
    """
    cls = type(x)

    if cls in _copy_atomic:
        return x
    if cls in _copy_builtin_containers:
        return cls.copy(x)

    if issubclass(cls, type):
        # treat it as a regular class
        return x

    copier = getattr(cls, "__copy__", None)
    if copier is not None:
        return copier(x)

    reductor = dispatch_table.get(cls, None)
    if reductor is not None:
        rv = reductor(x)
    else:
        rv = _call_reduce(x)

    if isinstance(rv, str):
        return x
    return _reconstruct(x, None, *rv)


def _call_reduce(x):
    reductor = getattr(x, "__reduce_ex__", None)
    if reductor is not None:
        return reductor(4)
    reductor = getattr(x, "__reduce__", None)
    if reductor:
        return reductor()
    raise Error(f"un(deep)copyable object of type {type(x)}")


_copy_atomic = {types.NoneType, int, float, bool, complex, str, tuple,
                bytes, frozenset, type, range, slice, property,
                types.BuiltinFunctionType, types.EllipsisType,
                types.NotImplementedType, types.FunctionType, types.CodeType,
                weakref.ref, super}
_copy_builtin_containers = {list, dict, set, bytearray}
try:
    _copy_atomic.add(frozendict)
except NameError:
    pass


_KEEP = object()   # marker: no memo entry


def deepcopy(x, memo=None):
    """Deep copy operation on arbitrary Python objects.

    See the module's __doc__ string for more info.
    """
    cls = type(x)

    if cls in _atomic_types:
        return x

    d = id(x)
    if memo is None:
        memo = {}
    else:
        y = memo.get(d, _KEEP)
        if y is not _KEEP:
            return y

    copier = _deepcopy_dispatch.get(cls)
    if copier is not None:
        y = copier(x, memo)
    else:
        if issubclass(cls, type):
            y = x   # treat it as a regular class
        else:
            copier = getattr(x, "__deepcopy__", None)
            if copier is not None:
                y = copier(memo)
            else:
                reductor = dispatch_table.get(cls)
                if reductor:
                    rv = reductor(x)
                else:
                    rv = _call_reduce(x)
                if isinstance(rv, str):
                    y = x
                else:
                    y = _reconstruct(x, memo, *rv)

    # If it is its own copy, don't memoize.
    if y is not x:
        memo[d] = y
        _keep_alive(x, memo)  # Make sure x lives at least as long as d
    return y


_atomic_types = {types.NoneType, types.EllipsisType, types.NotImplementedType,
                 int, float, bool, complex, bytes, str, types.CodeType, type,
                 range, types.BuiltinFunctionType, types.FunctionType,
                 weakref.ref, property}

_deepcopy_dispatch = d = {}


def _deepcopy_list(x, memo, deepcopy=deepcopy, atomic=_atomic_types):
    y = []
    memo[id(x)] = y
    append = y.append
    for a in x:
        # atomic items are their own copies: skip the call
        append(a if type(a) in atomic else deepcopy(a, memo))
    return y
d[list] = _deepcopy_list


def _deepcopy_tuple(x, memo, deepcopy=deepcopy, atomic=_atomic_types):
    y = [a if type(a) in atomic else deepcopy(a, memo) for a in x]
    # A recursive tuple may already have been copied through one of its
    # elements; use that copy.
    try:
        return memo[id(x)]
    except KeyError:
        pass
    for k, j in zip(x, y):
        if k is not j:
            return tuple(y)
    return x
d[tuple] = _deepcopy_tuple


def _deepcopy_dict(x, memo, deepcopy=deepcopy, atomic=_atomic_types):
    y = {}
    memo[id(x)] = y
    for key, value in x.items():
        if type(key) not in atomic:
            key = deepcopy(key, memo)
        y[key] = value if type(value) in atomic else deepcopy(value, memo)
    return y
d[dict] = _deepcopy_dict


try:
    def _deepcopy_frozendict(x, memo, deepcopy=deepcopy):
        items = [(deepcopy(k, memo), deepcopy(v, memo)) for k, v in x.items()]
        # as for tuples: a recursive frozendict may have been copied already
        # while its values were
        try:
            return memo[id(x)]
        except KeyError:
            pass
        y = frozendict(items)
        for (k1, v1), (k2, v2) in zip(x.items(), y.items()):
            if k1 is not k2 or v1 is not v2:
                return y
        return x
    d[frozendict] = _deepcopy_frozendict
except NameError:
    pass


def _deepcopy_method(x, memo):  # Copy instance methods
    return type(x)(x.__func__, deepcopy(x.__self__, memo))
d[types.MethodType] = _deepcopy_method

del d


def _keep_alive(x, memo):
    """Keeps a reference to the object x in the memo.

    The memo maps id() of copied objects to their copies; the originals must
    stay alive while the memo is in use or their ids could be reused.  They
    are kept in a list stored in the memo under the memo's own id.
    """
    try:
        memo[id(memo)].append(x)
    except KeyError:
        memo[id(memo)] = [x]


def _reconstruct(x, memo, func, args,
                 state=None, listiter=None, dictiter=None,
                 *, deepcopy=deepcopy):
    deep = memo is not None
    if deep and args:
        args = deepcopy(args, memo)
    y = func(*args)
    if deep:
        memo[id(x)] = y

    if state is not None:
        if deep:
            state = deepcopy(state, memo)
        if hasattr(y, '__setstate__'):
            y.__setstate__(state)
        else:
            if isinstance(state, tuple) and len(state) == 2:
                state, slotstate = state
            else:
                slotstate = None
            if state is not None:
                y.__dict__.update(state)
            if slotstate is not None:
                for key, value in slotstate.items():
                    setattr(y, key, value)

    if listiter is not None:
        if deep:
            for item in listiter:
                y.append(deepcopy(item, memo))
        else:
            for item in listiter:
                y.append(item)
    if dictiter is not None:
        if deep:
            for key, value in dictiter:
                y[deepcopy(key, memo)] = deepcopy(value, memo)
        else:
            for key, value in dictiter:
                y[key] = value
    return y


def replace(obj, /, **changes):
    """Return a new object replacing specified fields with new values.

    This is especially useful for immutable objects, like named tuples or
    frozen dataclasses.
    """
    cls = obj.__class__
    func = getattr(cls, '__replace__', None)
    if func is None:
        raise TypeError(f"replace() does not support {cls.__name__} objects")
    return func(obj, **changes)
