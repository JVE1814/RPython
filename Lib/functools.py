"""Higher-order functions and operations on callable objects."""

__all__ = ['update_wrapper', 'wraps', 'WRAPPER_ASSIGNMENTS', 'WRAPPER_UPDATES',
           'total_ordering', 'cache', 'cmp_to_key', 'lru_cache', 'reduce',
           'partial', 'partialmethod', 'singledispatch', 'singledispatchmethod',
           'cached_property', 'Placeholder']

from abc import get_cache_token
from collections import namedtuple
from reprlib import recursive_repr
from types import GenericAlias, MethodType, MappingProxyType, UnionType
from _thread import RLock


# --------------------------------------------------------------------------
# update_wrapper / wraps

WRAPPER_ASSIGNMENTS = ('__module__', '__name__', '__qualname__', '__doc__',
                       '__annotate__', '__type_params__')
WRAPPER_UPDATES = ('__dict__',)


def update_wrapper(wrapper, wrapped,
                   assigned=WRAPPER_ASSIGNMENTS,
                   updated=WRAPPER_UPDATES):
    """Make *wrapper* look like *wrapped*.

    Attributes named in *assigned* are copied from wrapped to wrapper (missing
    ones are skipped); mappings named in *updated* are merged into the
    wrapper's mappings.  Finally ``wrapper.__wrapped__`` is set to *wrapped*.
    """
    for attr in assigned:
        try:
            v = getattr(wrapped, attr)
        except AttributeError:
            continue
        setattr(wrapper, attr, v)
    for attr in updated:
        getattr(wrapper, attr).update(getattr(wrapped, attr, {}))
    # Done after the updates so that a __wrapped__ copied from wrapped's
    # __dict__ does not win.
    wrapper.__wrapped__ = wrapped
    return wrapper


def wraps(wrapped, assigned=WRAPPER_ASSIGNMENTS, updated=WRAPPER_UPDATES):
    """Decorator factory: apply update_wrapper() to the decorated function,
    with *wrapped*, *assigned* and *updated* as given here."""
    return partial(update_wrapper, wrapped=wrapped,
                   assigned=assigned, updated=updated)


# --------------------------------------------------------------------------
# total_ordering

def _derive(root, name):
    # Build the comparison method *name* from the user supplied method *root*
    # (and __eq__ where needed).  NotImplemented is passed through unchanged.
    if root == '__lt__':
        if name == '__gt__':
            def op(self, other):
                r = type(self).__lt__(self, other)
                return r if r is NotImplemented else (not r and self != other)
        elif name == '__le__':
            def op(self, other):
                r = type(self).__lt__(self, other)
                return r if r is NotImplemented else (r or self == other)
        else:
            def op(self, other):
                r = type(self).__lt__(self, other)
                return r if r is NotImplemented else not r
    elif root == '__le__':
        if name == '__ge__':
            def op(self, other):
                r = type(self).__le__(self, other)
                return r if r is NotImplemented else (not r or self == other)
        elif name == '__lt__':
            def op(self, other):
                r = type(self).__le__(self, other)
                return r if r is NotImplemented else (r and self != other)
        else:
            def op(self, other):
                r = type(self).__le__(self, other)
                return r if r is NotImplemented else not r
    elif root == '__gt__':
        if name == '__lt__':
            def op(self, other):
                r = type(self).__gt__(self, other)
                return r if r is NotImplemented else (not r and self != other)
        elif name == '__ge__':
            def op(self, other):
                r = type(self).__gt__(self, other)
                return r if r is NotImplemented else (r or self == other)
        else:
            def op(self, other):
                r = type(self).__gt__(self, other)
                return r if r is NotImplemented else not r
    else:  # __ge__
        if name == '__le__':
            def op(self, other):
                r = type(self).__ge__(self, other)
                return r if r is NotImplemented else (not r or self == other)
        elif name == '__gt__':
            def op(self, other):
                r = type(self).__ge__(self, other)
                return r if r is NotImplemented else (r and self != other)
        else:
            def op(self, other):
                r = type(self).__ge__(self, other)
                return r if r is NotImplemented else not r
    op.__name__ = name
    op.__qualname__ = name
    op.__doc__ = f'Return the result of {name}, derived by @total_ordering from {root}.'
    return op

_ORDER_OPS = ('__lt__', '__le__', '__gt__', '__ge__')


def total_ordering(cls):
    """Class decorator that fills in the missing rich comparison methods,
    given __eq__ and one of __lt__, __le__, __gt__ or __ge__."""
    defined = [op for op in _ORDER_OPS
               if getattr(cls, op, None) is not getattr(object, op, None)]
    if not defined:
        raise ValueError('must define at least one ordering operation: < > <= >=')
    # Preference order: __lt__, __le__, __gt__, __ge__
    root = defined[0]
    for name in _ORDER_OPS:
        if name not in defined:
            setattr(cls, name, _derive(root, name))
    return cls


# --------------------------------------------------------------------------
# cmp_to_key

def cmp_to_key(mycmp):
    """Convert a cmp= function into a key= function"""
    class K(object):
        __slots__ = ['obj']

        def __init__(self, obj):
            self.obj = obj

        def __lt__(self, other):
            return mycmp(self.obj, other.obj) < 0

        def __le__(self, other):
            return mycmp(self.obj, other.obj) <= 0

        def __eq__(self, other):
            return mycmp(self.obj, other.obj) == 0

        def __ge__(self, other):
            return mycmp(self.obj, other.obj) >= 0

        def __gt__(self, other):
            return mycmp(self.obj, other.obj) > 0

        __hash__ = None
    return K

try:
    from _functools import cmp_to_key
except ImportError:
    pass


# --------------------------------------------------------------------------
# reduce

_initial_missing = object()

def reduce(function, sequence, initial=_initial_missing):
    """
    reduce(function, iterable[, initial], /) -> value

    Apply a function of two arguments cumulatively to the items of an iterable, from left to right.

    This effectively reduces the iterable to a single value.  If initial is present,
    it is placed before the items of the iterable in the calculation, and serves as
    a default when the iterable is empty.

    For example, reduce(lambda x, y: x+y, [1, 2, 3, 4, 5])
    calculates ((((1 + 2) + 3) + 4) + 5).
    """
    it = iter(sequence)
    if initial is _initial_missing:
        for acc in it:
            break
        else:
            raise TypeError("reduce() of empty iterable with no initial value")
    else:
        acc = initial
    for x in it:
        acc = function(acc, x)
    return acc


def _warn_python_reduce_kwargs(py_reduce):
    @wraps(py_reduce)
    def reduce(*args, **kwargs):
        if 'function' in kwargs or 'sequence' in kwargs:
            import warnings
            warnings.warn(
                "Passing 'function' or 'sequence' as keyword arguments "
                "to reduce() is deprecated",
                DeprecationWarning, stacklevel=2)
        return py_reduce(*args, **kwargs)
    return reduce

reduce = _warn_python_reduce_kwargs(reduce)
del _warn_python_reduce_kwargs

try:
    from _functools import reduce
except ImportError:
    pass


# --------------------------------------------------------------------------
# partial

class _PlaceholderType:
    """The type of the Placeholder singleton.

    Used as a placeholder for partial arguments.
    """
    __instance = None
    __slots__ = ()

    def __init_subclass__(cls, *args, **kwargs):
        raise TypeError(f"type '{cls.__name__}' is not an acceptable base type")

    def __new__(cls, *args, **kwargs):
        if args or kwargs:
            raise TypeError(f"{cls.__name__} takes no arguments")
        inst = cls.__instance
        if inst is None:
            inst = cls.__instance = object.__new__(cls)
        return inst

    def __repr__(self):
        return 'Placeholder'

    def __reduce__(self):
        return 'Placeholder'

Placeholder = _PlaceholderType()


def _placeholder_plan(args):
    """Return (number of placeholders, merge function or None).

    The merge function receives ``stored_args + call_args`` and returns the
    stored arguments with each placeholder replaced by the next call argument.
    """
    n = len(args)
    picks = []
    nxt = n
    for i, a in enumerate(args):
        if a is Placeholder:
            picks.append(nxt)
            nxt += 1
        else:
            picks.append(i)
    count = nxt - n
    if not count:
        return 0, None
    def merge(seq):
        return tuple([seq[i] for i in picks])
    return count, merge


def _partial_new(cls, func, /, *args, **keywords):
    if issubclass(cls, partial):
        flatten_type = partial
        if not callable(func):
            raise TypeError("the first argument must be callable")
    else:
        flatten_type = partialmethod
        # descriptors such as classmethod objects are acceptable too
        if not callable(func) and not hasattr(func, "__get__"):
            raise TypeError(f"the first argument {func!r} must be a callable "
                            "or a descriptor")
    if args and args[-1] is Placeholder:
        raise TypeError("trailing Placeholders are not allowed")
    if any(v is Placeholder for v in keywords.values()):
        raise TypeError("Placeholder cannot be passed as a keyword argument")
    if isinstance(func, flatten_type):
        inner_count = func._phcount
        all_args = func.args
        if args:
            if inner_count:
                filled = args[:inner_count]
                if len(filled) < inner_count:
                    filled += (Placeholder,) * (inner_count - len(filled))
                all_args = func._merger(all_args + filled) + args[inner_count:]
            else:
                all_args = all_args + args
            count, merger = _placeholder_plan(all_args)
        else:
            count, merger = inner_count, func._merger
        keywords = {**func.keywords, **keywords}
        func = func.func
    else:
        all_args = args
        count, merger = _placeholder_plan(all_args)

    self = object.__new__(cls)
    self.func = func
    self.args = all_args
    self.keywords = keywords
    self._phcount = count
    self._merger = merger
    return self


def _partial_repr(self):
    cls = type(self)
    parts = [repr(self.func)]
    parts += [repr(a) for a in self.args]
    parts += [f"{k}={v!r}" for k, v in self.keywords.items()]
    return f"{cls.__module__}.{cls.__qualname__}({', '.join(parts)})"


def _partial_split_args(self, args, kind):
    count = self._phcount
    if not count:
        return self.args, args
    if len(args) < count:
        raise TypeError(f"missing positional arguments in '{kind}' call; "
                        f"expected at least {count}, got {len(args)}")
    return self._merger(self.args + args[:count]), args[count:]


class partial:
    """New function with partial application of the given arguments
    and keywords.
    """

    __slots__ = ("func", "args", "keywords", "_phcount", "_merger",
                 "__dict__", "__weakref__")

    __new__ = _partial_new
    __repr__ = recursive_repr()(_partial_repr)

    def __call__(self, /, *args, **keywords):
        head, rest = _partial_split_args(self, args, 'partial')
        return self.func(*head, *rest, **{**self.keywords, **keywords})

    def __get__(self, obj, objtype=None):
        if obj is None:
            return self
        return MethodType(self, obj)

    def __reduce__(self):
        return type(self), (self.func,), (self.func, self.args,
               self.keywords or None, self.__dict__ or None)

    def __setstate__(self, state):
        if not isinstance(state, tuple):
            raise TypeError("argument to __setstate__ must be a tuple")
        if len(state) != 4:
            raise TypeError(f"expected 4 items in state, got {len(state)}")
        func, args, kwds, namespace = state
        if (not callable(func) or not isinstance(args, tuple)
                or not (kwds is None or isinstance(kwds, dict))
                or not (namespace is None or isinstance(namespace, dict))):
            raise TypeError("invalid partial state")
        if args and args[-1] is Placeholder:
            raise TypeError("trailing Placeholders are not allowed")
        count, merger = _placeholder_plan(args)
        args = tuple(args)
        if kwds is None:
            kwds = {}
        elif type(kwds) is not dict:
            kwds = dict(kwds)
        self.__dict__ = {} if namespace is None else namespace
        self.func = func
        self.args = args
        self.keywords = kwds
        self._phcount = count
        self._merger = merger

    __class_getitem__ = classmethod(GenericAlias)


try:
    from _functools import partial, Placeholder, _PlaceholderType
except ImportError:
    pass


class partialmethod:
    """Method descriptor with partial application of the given arguments
    and keywords.

    Supports wrapping existing descriptors and handles non-descriptor
    callables as instance methods.
    """

    __new__ = _partial_new
    __repr__ = _partial_repr

    def _make_unbound_method(self):
        def _method(cls_or_self, /, *args, **keywords):
            head, rest = _partial_split_args(self, args, 'partialmethod')
            return self.func(cls_or_self, *head, *rest,
                             **{**self.keywords, **keywords})
        _method.__isabstractmethod__ = self.__isabstractmethod__
        _method.__partialmethod__ = self
        return _method

    def __get__(self, obj, cls=None):
        getter = getattr(self.func, "__get__", None)
        if getter is not None and not isinstance(self.func, partial):
            bound = getter(obj, cls)
            if bound is not self.func:
                # The wrapped descriptor produced its own callable: bind our
                # arguments on top of it.
                result = partial(bound, *self.args, **self.keywords)
                try:
                    result.__self__ = bound.__self__
                except AttributeError:
                    pass
                return result
        return self._make_unbound_method().__get__(obj, cls)

    @property
    def __isabstractmethod__(self):
        return getattr(self.func, "__isabstractmethod__", False)

    __class_getitem__ = classmethod(GenericAlias)


def _unwrap_partial(func):
    while isinstance(func, partial):
        func = func.func
    return func

def _unwrap_partialmethod(func):
    while True:
        before = func
        while isinstance(getattr(func, "__partialmethod__", None), partialmethod):
            func = func.__partialmethod__
        while isinstance(func, partialmethod):
            func = getattr(func, 'func')
        func = _unwrap_partial(func)
        if func is before:
            return func


# --------------------------------------------------------------------------
# lru_cache

_CacheInfo = namedtuple("CacheInfo", ["hits", "misses", "maxsize", "currsize"])


class _HashedSeq(list):
    """A list used as a cache key whose hash is computed exactly once."""

    __slots__ = 'hashvalue'

    def __init__(self, tup, hash=hash):
        self[:] = tup
        self.hashvalue = hash(tup)

    def __hash__(self):
        return self.hashvalue


_KWD_MARK = (object(),)
_FAST_TYPES = frozenset({int, str})

def _make_key(args, kwds, typed):
    """Flat, hashable cache key for a call with *args* and *kwds*."""
    key = args
    if kwds:
        key = key + _KWD_MARK
        for pair in kwds.items():
            key = key + pair
    if typed:
        key = key + tuple([type(a) for a in args])
        if kwds:
            key = key + tuple([type(v) for v in kwds.values()])
    elif len(key) == 1 and type(key[0]) in _FAST_TYPES:
        return key[0]
    return _HashedSeq(key)


def lru_cache(maxsize=128, typed=False):
    """Least-recently-used cache decorator.

    If *maxsize* is set to None, the LRU features are disabled and the cache
    can grow without bound.

    If *typed* is True, arguments of different types will be cached separately.
    For example, f(decimal.Decimal("3.0")) and f(3.0) will be treated as
    distinct calls with distinct results. Some types such as str and int may
    be cached separately even when typed is false.

    Arguments to the cached function must be hashable.

    View the cache statistics named tuple (hits, misses, maxsize, currsize)
    with f.cache_info().  Clear the cache and statistics with f.cache_clear().
    Access the underlying function with f.__wrapped__.
    """
    if isinstance(maxsize, int):
        maxsize = max(maxsize, 0)
    elif callable(maxsize) and isinstance(typed, bool):
        # used as @lru_cache without arguments
        return lru_cache(128, typed)(maxsize)
    elif maxsize is not None:
        raise TypeError(
            'Expected first argument to be an integer, a callable, or None')

    def decorating_function(user_function):
        wrapper = _lru_cache_wrapper(user_function, maxsize, typed, _CacheInfo)
        wrapper.cache_parameters = lambda: {'maxsize': maxsize, 'typed': typed}
        return update_wrapper(wrapper, user_function)

    return decorating_function


def _lru_cache_wrapper(user_function, maxsize, typed, _CacheInfo):
    if not callable(user_function):
        raise TypeError("the first argument must be callable")

    missing = object()
    make_key = _make_key
    lock = RLock()
    cache = {}
    cache_get = cache.get
    cache_len = cache.__len__
    stats = [0, 0]      # hits, misses
    # Doubly linked list of [prev, next, key, result] cells; `head` is a
    # sentinel cell.  The entry after `head` is the least recently used.
    head = []
    head[:] = [head, head, None, None]

    if maxsize == 0:
        def wrapper(*args, **kwds):
            stats[1] += 1
            return user_function(*args, **kwds)

    elif maxsize is None:
        def wrapper(*args, **kwds):
            key = make_key(args, kwds, typed)
            result = cache_get(key, missing)
            if result is not missing:
                stats[0] += 1
                return result
            stats[1] += 1
            result = user_function(*args, **kwds)
            cache[key] = result
            return result

    else:
        def wrapper(*args, **kwds):
            key = make_key(args, kwds, typed)
            with lock:
                cell = cache_get(key)
                if cell is not None:
                    # unlink and move to the most recently used position
                    cell[0][1] = cell[1]
                    cell[1][0] = cell[0]
                    tail = head[0]
                    tail[1] = cell
                    head[0] = cell
                    cell[0] = tail
                    cell[1] = head
                    stats[0] += 1
                    return cell[3]
                stats[1] += 1
            result = user_function(*args, **kwds)
            with lock:
                if key in cache:
                    # a recursive or concurrent call already stored it
                    return result
                if cache_len() >= maxsize:
                    oldest = head[1]
                    if oldest is not head:
                        oldest[0][1] = oldest[1]
                        oldest[1][0] = oldest[0]
                        del cache[oldest[2]]
                tail = head[0]
                cell = [tail, head, key, result]
                tail[1] = cell
                head[0] = cell
                cache[key] = cell
            return result

    def cache_info():
        """Report cache statistics"""
        with lock:
            return _CacheInfo(stats[0], stats[1], maxsize, cache_len())

    def cache_clear():
        """Clear the cache and cache statistics"""
        with lock:
            cache.clear()
            head[:] = [head, head, None, None]
            stats[0] = stats[1] = 0

    wrapper.cache_info = cache_info
    wrapper.cache_clear = cache_clear
    return wrapper

try:
    from _functools import _lru_cache_wrapper
except ImportError:
    pass


def cache(user_function, /):
    'Simple lightweight unbounded cache.  Sometimes called "memoize".'
    return lru_cache(maxsize=None)(user_function)


# --------------------------------------------------------------------------
# singledispatch

def _c3_merge(seqs):
    """Merge linearizations with the C3 rule."""
    out = []
    seqs = [list(s) for s in seqs]
    while True:
        seqs = [s for s in seqs if s]
        if not seqs:
            return out
        for s in seqs:
            head = s[0]
            if not any(head in other[1:] for other in seqs):
                break
        else:
            raise RuntimeError("Inconsistent hierarchy")
        out.append(head)
        for s in seqs:
            if s[0] == head:
                del s[0]


def _c3_mro(cls, abcs=None):
    """C3 linearization of *cls* that also places the relevant ABCs from
    *abcs* right where their behaviour is first provided."""
    bases = cls.__bases__
    boundary = 0
    for i in range(len(bases) - 1, -1, -1):
        if hasattr(bases[i], '__abstractmethods__'):
            boundary = i + 1
            break
    abcs = list(abcs) if abcs else []
    explicit = list(bases[:boundary])
    others = list(bases[boundary:])
    implicit = [b for b in abcs
                if issubclass(cls, b) and not any(issubclass(x, b) for x in bases)]
    for b in implicit:
        abcs.remove(b)
    return _c3_merge(
        [[cls]]
        + [_c3_mro(b, abcs=abcs) for b in explicit]
        + [_c3_mro(b, abcs=abcs) for b in implicit]
        + [_c3_mro(b, abcs=abcs) for b in others]
        + [explicit, implicit, others]
    )


def _compose_mro(cls, types):
    """MRO of *cls* extended with the ABCs among *types* it relates to."""
    own = set(cls.__mro__)
    candidates = [t for t in types
                  if t not in own and hasattr(t, '__mro__')
                  and not isinstance(t, GenericAlias) and issubclass(cls, t)]
    candidates = [t for t in candidates
                  if not any(t is not o and t in o.__mro__ for o in candidates)]
    cand_set = set(candidates)
    ordered = []
    for t in candidates:
        groups = []
        for sub in t.__subclasses__():
            if sub not in own and issubclass(cls, sub):
                groups.append([s for s in sub.__mro__ if s in cand_set])
        if not groups:
            ordered.append(t)
            continue
        groups.sort(key=len, reverse=True)
        for g in groups:
            for s in g:
                if s not in ordered:
                    ordered.append(s)
    return _c3_mro(cls, abcs=ordered)


def _find_impl(cls, registry):
    """Best implementation in *registry* for *cls* (or None)."""
    mro = _compose_mro(cls, registry.keys())
    best = None
    for t in mro:
        if best is not None:
            # Two unrelated implicit ABCs at the same level: ambiguous.
            if (t in registry and t not in cls.__mro__
                    and best not in cls.__mro__
                    and not issubclass(best, t)):
                raise RuntimeError(f"Ambiguous dispatch: {best} or {t}")
            break
        if t in registry:
            best = t
    return registry.get(best)


def _valid_dispatch_type(cls):
    if isinstance(cls, type):
        return True
    return (isinstance(cls, UnionType)
            and all(isinstance(a, type) for a in cls.__args__))


def singledispatch(func):
    """Single-dispatch generic function decorator.

    Transforms a function into a generic function, which can have different
    behaviours depending upon the type of its first argument. The decorated
    function acts as the default implementation, and additional
    implementations can be registered using the register() attribute of the
    generic function.
    """
    import weakref   # imported lazily: most users of functools never get here

    registry = {}
    dispatch_cache = weakref.WeakKeyDictionary()
    token = None

    def dispatch(cls):
        """generic_func.dispatch(cls) -> <function implementation>

        Runs the dispatch algorithm to return the best available implementation
        for the given *cls* registered on *generic_func*.
        """
        nonlocal token
        if token is not None:
            now = get_cache_token()
            if token != now:
                dispatch_cache.clear()
                token = now
        try:
            return dispatch_cache[cls]
        except KeyError:
            pass
        impl = registry.get(cls)
        if impl is None:
            impl = _find_impl(cls, registry)
        dispatch_cache[cls] = impl
        return impl

    def register(cls, func=None):
        """generic_func.register(cls, func) -> func

        Registers a new implementation for the given *cls* on a *generic_func*.
        """
        nonlocal token
        if _valid_dispatch_type(cls):
            if func is None:
                return lambda f: register(cls, f)
        else:
            if func is not None:
                raise TypeError(
                    f"Invalid first argument to `register()`. "
                    f"{cls!r} is not a class or union type.")
            if getattr(cls, '__annotate__', None) is None:
                raise TypeError(
                    f"Invalid first argument to `register()`: {cls!r}. "
                    f"Use either `@register(some_class)` or plain `@register` "
                    f"on an annotated function.")
            func = cls
            from annotationlib import Format, ForwardRef
            from typing import get_type_hints
            hints = get_type_hints(func, format=Format.FORWARDREF)
            argname, cls = next(iter(hints.items()))
            if not _valid_dispatch_type(cls):
                if isinstance(cls, UnionType):
                    why = "not all arguments are classes."
                elif isinstance(cls, ForwardRef):
                    why = "is an unresolved forward reference."
                else:
                    why = "is not a class."
                raise TypeError(
                    f"Invalid annotation for {argname!r}. {cls!r} {why}")

        if isinstance(cls, UnionType):
            for t in cls.__args__:
                registry[t] = func
        else:
            registry[cls] = func
        if token is None and hasattr(cls, '__abstractmethods__'):
            token = get_cache_token()
        dispatch_cache.clear()
        return func

    funcname = getattr(func, '__name__', 'singledispatch function')

    def wrapper(*args, **kw):
        if not args:
            raise TypeError(f'{funcname} requires at least '
                            '1 positional argument')
        return dispatch(args[0].__class__)(*args, **kw)

    registry[object] = func
    wrapper.register = register
    wrapper.dispatch = dispatch
    wrapper.registry = MappingProxyType(registry)
    wrapper._clear_cache = dispatch_cache.clear
    update_wrapper(wrapper, func)
    return wrapper


class singledispatchmethod:
    """Single-dispatch generic method descriptor.

    Supports wrapping existing descriptors and handles non-descriptor
    callables as instance methods.
    """

    def __init__(self, func):
        if not callable(func) and not hasattr(func, "__get__"):
            raise TypeError(f"{func!r} is not callable or a descriptor")
        self.dispatcher = singledispatch(func)
        self.func = func

    def register(self, cls, method=None):
        """generic_method.register(cls, func) -> func

        Registers a new implementation for the given *cls* on a *generic_method*.
        """
        return self.dispatcher.register(cls, func=method)

    def __get__(self, obj, cls=None):
        return _singledispatchmethod_get(self, obj, cls)

    @property
    def __isabstractmethod__(self):
        return getattr(self.func, '__isabstractmethod__', False)

    def __repr__(self):
        name = getattr(self.func, '__qualname__', None)
        if name is None:
            name = getattr(self.func, '__name__', '?')
        return f'<single dispatch method descriptor {name}>'


class _singledispatchmethod_get:
    def __init__(self, unbound, obj, cls):
        self._unbound = unbound
        self._dispatch = unbound.dispatcher.dispatch
        self._obj = obj
        self._cls = cls
        f = unbound.func
        # These clash with descriptors on the class, so copy them eagerly.
        try:
            self.__module__ = f.__module__
        except AttributeError:
            pass
        try:
            self.__doc__ = f.__doc__
        except AttributeError:
            pass

    def __repr__(self):
        try:
            name = self.__qualname__
        except AttributeError:
            try:
                name = self.__name__
            except AttributeError:
                name = '?'
        if self._obj is None:
            return f'<single dispatch method {name}>'
        return f'<bound single dispatch method {name} of {self._obj!r}>'

    def __call__(self, /, *args, **kwargs):
        if not args:
            funcname = getattr(self._unbound.func, '__name__',
                               'singledispatchmethod method')
            raise TypeError(f'{funcname} requires at least '
                            '1 positional argument')
        impl = self._dispatch(args[0].__class__)
        return impl.__get__(self._obj, self._cls)(*args, **kwargs)

    def __getattr__(self, name):
        if name in ('__name__', '__qualname__', '__isabstractmethod__',
                    '__annotations__', '__type_params__'):
            return getattr(self._unbound.func, name)
        raise AttributeError

    @property
    def __wrapped__(self):
        return self._unbound.func

    @property
    def register(self):
        return self._unbound.register


# --------------------------------------------------------------------------
# cached_property

_NOT_FOUND = object()

class cached_property:
    def __init__(self, func):
        self.func = func
        self.attrname = None
        self.__doc__ = func.__doc__
        self.__module__ = func.__module__

    def __set_name__(self, owner, name):
        if self.attrname is None:
            self.attrname = name
        elif name != self.attrname:
            raise TypeError(
                "Cannot assign the same cached_property to two different names "
                f"({self.attrname!r} and {name!r}).")

    def __get__(self, instance, owner=None):
        if instance is None:
            return self
        name = self.attrname
        if name is None:
            raise TypeError(
                "Cannot use cached_property instance without calling __set_name__ on it.")
        try:
            store = instance.__dict__
        except AttributeError:
            raise TypeError(
                f"No '__dict__' attribute on {type(instance).__name__!r} "
                f"instance to cache {name!r} property.") from None
        value = store.get(name, _NOT_FOUND)
        if value is _NOT_FOUND:
            value = self.func(instance)
            try:
                store[name] = value
            except TypeError:
                raise TypeError(
                    f"The '__dict__' attribute on {type(instance).__name__!r} instance "
                    f"does not support item assignment for caching {name!r} property."
                ) from None
        return value

    __class_getitem__ = classmethod(GenericAlias)
