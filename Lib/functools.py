"""Higher-order functions and operations on callable objects.

Contents: wrapper helpers (update_wrapper, wraps), comparison helpers
(total_ordering, cmp_to_key), partial application (partial, partialmethod,
Placeholder), memoization (lru_cache, cache, cached_property), reduce, and
type-based dispatch (singledispatch, singledispatchmethod).

Where the native ``_functools`` module is available, its partial, reduce,
cmp_to_key, Placeholder and LRU wrapper replace the Python versions below;
the Python versions stay importable for tests that block the accelerator.
"""

__all__ = ['update_wrapper', 'wraps', 'WRAPPER_ASSIGNMENTS', 'WRAPPER_UPDATES',
           'total_ordering', 'cache', 'cmp_to_key', 'lru_cache', 'reduce',
           'partial', 'partialmethod', 'singledispatch', 'singledispatchmethod',
           'cached_property', 'Placeholder']

from abc import get_cache_token as _abc_cache_token
from collections import namedtuple, OrderedDict as _OrderedDict
from reprlib import recursive_repr as _recursive_repr
from _thread import RLock
from types import (FunctionType as _FunctionType, GenericAlias,
                   MappingProxyType as _MappingProxyType,
                   MethodType as _MethodType, UnionType as _UnionType)

# Captured at import time: lru_cache can be asked to cache the builtin len()
# itself, so the caching machinery must not look len up dynamically.
_len = len


# --------------------------------------------------------------------------
# update_wrapper / wraps

# Attributes copied verbatim from the wrapped object onto the wrapper.
WRAPPER_ASSIGNMENTS = ('__module__', '__name__', '__qualname__', '__doc__',
                       '__annotate__', '__type_params__')
# Attributes of the wrapper that are dict-updated from the wrapped object.
WRAPPER_UPDATES = ('__dict__',)


def update_wrapper(wrapper, wrapped,
                   assigned=WRAPPER_ASSIGNMENTS, updated=WRAPPER_UPDATES):
    """Make *wrapper* look like *wrapped* and return it.

    Each attribute named in *assigned* that exists on *wrapped* is copied to
    *wrapper*; each mapping named in *updated* is updated in place from the
    one on *wrapped*.  Finally ``wrapper.__wrapped__`` is set to *wrapped*.
    """
    for name in assigned:
        try:
            value = getattr(wrapped, name)
        except AttributeError:
            continue
        setattr(wrapper, name, value)
    for name in updated:
        getattr(wrapper, name).update(getattr(wrapped, name, {}))
    # Set last, so that a __wrapped__ found in wrapped.__dict__ is replaced.
    wrapper.__wrapped__ = wrapped
    return wrapper


def wraps(wrapped, assigned=WRAPPER_ASSIGNMENTS, updated=WRAPPER_UPDATES):
    """Decorator version of update_wrapper().

    ``@wraps(f)`` applied to a function g is ``update_wrapper(g, f, ...)``.
    """
    def apply(wrapper):
        return update_wrapper(wrapper, wrapped, assigned, updated)
    return apply


# --------------------------------------------------------------------------
# total_ordering

# Each factory builds one comparison out of a "root" comparison that the
# class defines.  r is the root's result for (self, other).

def _not_root_and_unequal(root):
    def method(self, other):
        r = getattr(type(self), root)(self, other)
        if r is NotImplemented:
            return r
        return not r and self != other
    return method

def _root_or_equal(root):
    def method(self, other):
        r = getattr(type(self), root)(self, other)
        if r is NotImplemented:
            return r
        return r or self == other
    return method

def _not_root(root):
    def method(self, other):
        r = getattr(type(self), root)(self, other)
        if r is NotImplemented:
            return r
        return not r
    return method

def _not_root_or_equal(root):
    def method(self, other):
        r = getattr(type(self), root)(self, other)
        if r is NotImplemented:
            return r
        return not r or self == other
    return method

def _root_and_unequal(root):
    def method(self, other):
        r = getattr(type(self), root)(self, other)
        if r is NotImplemented:
            return r
        return r and self != other
    return method

# root -> {missing comparison: factory}; the order of the keys is the order
# of preference when a class defines several roots.
_ORDERING_RECIPES = {
    '__lt__': {'__gt__': _not_root_and_unequal, '__le__': _root_or_equal,
               '__ge__': _not_root},
    '__le__': {'__ge__': _not_root_or_equal, '__lt__': _root_and_unequal,
               '__gt__': _not_root},
    '__gt__': {'__lt__': _not_root_and_unequal, '__ge__': _root_or_equal,
               '__le__': _not_root},
    '__ge__': {'__le__': _not_root_or_equal, '__gt__': _root_and_unequal,
               '__lt__': _not_root},
}

_OPERATOR_SYMBOLS = {'__lt__': '<', '__le__': '<=', '__gt__': '>', '__ge__': '>='}


def total_ordering(cls):
    """Class decorator that fills in missing rich comparison methods.

    The class must define __eq__ and at least one of __lt__, __le__, __gt__
    or __ge__; the remaining ordering methods are derived from it.
    """
    defined = [name for name in _ORDERING_RECIPES
               if getattr(cls, name, None) is not getattr(object, name, None)]
    if not defined:
        raise ValueError(f'total_ordering: {cls.__name__} defines none of '
                         '__lt__, __le__, __gt__, __ge__')
    root = defined[0]
    for name, factory in _ORDERING_RECIPES[root].items():
        if name in defined:
            continue
        method = factory(root)
        method.__name__ = name
        # A qualified name inside the class keeps the method picklable by
        # reference, like any method written in the class body.
        method.__qualname__ = f'{cls.__qualname__}.{name}'
        method.__module__ = cls.__module__
        method.__doc__ = (f'Return a {_OPERATOR_SYMBOLS[name]} b, derived from '
                          f'{_OPERATOR_SYMBOLS[root]} and ==.')
        setattr(cls, name, method)
    return cls


# --------------------------------------------------------------------------
# cmp_to_key

class _ComparisonKey:
    """Sort key wrapping *obj*; ordering is decided by an old-style cmp
    function (negative, zero or positive result)."""

    __slots__ = ('_cmp', 'obj')
    __hash__ = None

    def __init__(self, cmp, obj):
        self._cmp = cmp
        self.obj = obj

    def __lt__(self, other):
        return self._cmp(self.obj, other.obj) < 0

    def __gt__(self, other):
        return self._cmp(self.obj, other.obj) > 0

    def __eq__(self, other):
        return self._cmp(self.obj, other.obj) == 0

    def __le__(self, other):
        return self._cmp(self.obj, other.obj) <= 0

    def __ge__(self, other):
        return self._cmp(self.obj, other.obj) >= 0


def cmp_to_key(mycmp):
    """Turn a cmp= style function into a key= function (one argument, obj)."""
    def key(obj):
        return _ComparisonKey(mycmp, obj)
    return key

try:
    from _functools import cmp_to_key
except ImportError:
    pass


# --------------------------------------------------------------------------
# reduce

_NO_INITIAL = object()


def reduce(*args, **kwargs):
    """reduce(function, iterable[, initial], /) -> value

    Combine the items of *iterable* from left to right with the two-argument
    *function*: reduce(f, [a, b, c]) is f(f(a, b), c).  *initial*, when
    given (positionally or by keyword), goes before the first item and is
    the result for an empty iterable.
    """
    function, iterable, initial = _reduce_arguments(args, kwargs)
    items = iter(iterable)
    if initial is _NO_INITIAL:
        for value in items:
            break
        else:
            raise TypeError('reduce() got an empty iterable and no initial value')
    else:
        value = initial
    for item in items:
        value = function(value, item)
    return value


def _reduce_arguments(args, kwargs):
    # Sort out reduce()'s arguments.  'function' and 'sequence' were accepted
    # as keywords by older versions; they still work, with a warning.
    count = _len(args)
    if count > 3:
        raise TypeError(f'reduce expected at most 3 arguments, got {count}')
    slots = list(args) + [_NO_INITIAL] * (3 - count)
    for position, name in ((0, 'function'), (1, 'sequence'), (2, 'initial')):
        if name not in kwargs:
            continue
        if slots[position] is not _NO_INITIAL:
            raise TypeError(f"reduce() got multiple values for argument {name!r}")
        if name != 'initial':
            import warnings
            warnings.warn(f"Passing {name!r} to reduce() as a keyword argument "
                          f"is deprecated", DeprecationWarning, stacklevel=3)
        slots[position] = kwargs.pop(name)
    if kwargs:
        name = next(iter(kwargs))
        raise TypeError(f"reduce() got an unexpected keyword argument {name!r}")
    if slots[0] is _NO_INITIAL or slots[1] is _NO_INITIAL:
        raise TypeError('reduce() needs at least a function and an iterable')
    return slots

try:
    from _functools import reduce
except ImportError:
    pass


# --------------------------------------------------------------------------
# Placeholder

class _PlaceholderType:
    """The type of Placeholder, which marks a positional argument of a
    partial object that is supplied later, at call time."""

    __slots__ = ()

    def __new__(cls, /, *args, **kwargs):
        if args or kwargs:
            raise TypeError(f"{cls.__name__} takes no arguments")
        return Placeholder

    def __repr__(self):
        return 'Placeholder'

    def __reduce__(self):
        return 'Placeholder'

# Created without calling __new__, which hands out this very object.
Placeholder = object.__new__(_PlaceholderType)

try:
    from _functools import Placeholder, _PlaceholderType
except ImportError:
    pass


def _check_partial_arguments(args, keywords):
    if args and args[-1] is Placeholder:
        raise TypeError('trailing Placeholders are not allowed')
    for value in keywords.values():
        if value is Placeholder:
            raise TypeError('Placeholder is only allowed among the positional arguments')


def _holes(args):
    """Positions of the Placeholders in *args*."""
    return tuple(i for i, value in enumerate(args) if value is Placeholder)


def _plug_holes(template, holes, supplied):
    """Return *template* with its Placeholder positions (*holes*) filled, in
    order, from *supplied*; surplus items of *supplied* are appended.  When
    *supplied* is too short the remaining holes stay open."""
    if not holes:
        return template + supplied
    merged = list(template)
    for position, value in zip(holes, supplied):
        merged[position] = value
    merged.extend(supplied[_len(holes):])
    return tuple(merged)


# --------------------------------------------------------------------------
# partial

class partial:
    """partial(func, /, *args, **keywords)

    A callable that calls *func* with *args* and *keywords* prepended to the
    arguments of each call.  Placeholder items in *args* are filled, in
    order, by the first positional arguments of the call.
    """

    __slots__ = ('func', 'args', 'keywords', '_open', '__dict__', '__weakref__')

    def __new__(cls, func, /, *args, **keywords):
        if not callable(func):
            raise TypeError('the first argument must be callable')
        _check_partial_arguments(args, keywords)
        if isinstance(func, partial):
            # Collapse nested partials into a single level.
            args = _plug_holes(func.args, func._open, args)
            keywords = {**func.keywords, **keywords}
            func = func.func
        self = object.__new__(cls)
        self.func = func
        self.args = args
        self.keywords = keywords
        self._open = _holes(args)
        return self

    def __call__(self, /, *args, **keywords):
        merged = {**self.keywords, **keywords}
        holes = self._open
        if holes:
            if _len(args) < _len(holes):
                raise TypeError(
                    "missing positional arguments in 'partial' call; "
                    f"expected at least {_len(holes)}, got {_len(args)}")
            return self.func(*_plug_holes(self.args, holes, args), **merged)
        return self.func(*self.args, *args, **merged)

    def __get__(self, obj, objtype=None):
        # Behave like a function when stored as a class attribute.
        if obj is None:
            return self
        return _MethodType(self, obj)

    @_recursive_repr()
    def __repr__(self):
        func, args, keywords = self.func, self.args, self.keywords
        pieces = [repr(func)]
        pieces.extend(repr(value) for value in args)
        pieces.extend(f'{key}={value!r}' for key, value in list(keywords.items()))
        cls = type(self)
        return f"{cls.__module__}.{cls.__qualname__}({', '.join(pieces)})"

    def __reduce__(self):
        return (type(self), (self.func,),
                (self.func, self.args, self.keywords or None,
                 self.__dict__ or None))

    def __setstate__(self, state):
        if not isinstance(state, tuple):
            raise TypeError('partial state must be a tuple')
        if _len(state) != 4:
            raise TypeError(f'partial state needs 4 items, not {_len(state)}')
        func, args, keywords, namespace = state
        if (not callable(func) or not isinstance(args, tuple)
                or (keywords is not None and not isinstance(keywords, dict))
                or (namespace is not None and not isinstance(namespace, dict))):
            raise TypeError('malformed partial state')
        if args and args[-1] is Placeholder:
            raise TypeError('trailing Placeholders are not allowed')
        if type(args) is not tuple:
            args = tuple(args)
        if keywords is None:
            keywords = {}
        elif type(keywords) is not dict:
            keywords = dict(keywords)
        self.__dict__ = {} if namespace is None else dict(namespace)
        self.func = func
        self.args = args
        self.keywords = keywords
        self._open = _holes(args)

    __class_getitem__ = classmethod(GenericAlias)

try:
    from _functools import partial
except ImportError:
    pass


# --------------------------------------------------------------------------
# partialmethod

class partialmethod:
    """partialmethod(func, /, *args, **keywords)

    The descriptor counterpart of partial: used as a class attribute it
    produces methods whose arguments start with the bound object (or class)
    followed by *args*, with *keywords* added.  *func* may itself be a
    descriptor such as classmethod or staticmethod.
    """

    def __init__(self, func, /, *args, **keywords):
        if not callable(func) and not hasattr(func, '__get__'):
            raise TypeError(f'expected a callable or a descriptor, got {func!r}')
        _check_partial_arguments(args, keywords)
        if isinstance(func, partialmethod):
            args = _plug_holes(func.args, _holes(func.args), args)
            keywords = {**func.keywords, **keywords}
            func = func.func
        self.func = func
        self.args = args
        self.keywords = keywords

    def __repr__(self):
        pieces = [repr(self.func)]
        pieces.extend(repr(value) for value in self.args)
        pieces.extend(f'{key}={value!r}' for key, value in self.keywords.items())
        cls = type(self)
        return f"{cls.__module__}.{cls.__qualname__}({', '.join(pieces)})"

    def _plain_method(self):
        # A function taking the instance (or class) first; the stored
        # arguments follow it.  A leading Placeholder reserves that slot.
        if self.args:
            target = partial(self.func, Placeholder, *self.args, **self.keywords)
        else:
            target = partial(self.func, **self.keywords)

        def method(cls_or_self, /, *args, **keywords):
            return target(cls_or_self, *args, **keywords)

        method.__isabstractmethod__ = self.__isabstractmethod__
        method.__partialmethod__ = self
        return method

    def __get__(self, obj, cls=None):
        func = self.func
        binder = getattr(func, '__get__', None)
        if binder is not None and not isinstance(func, partial):
            bound = binder(obj, cls)
            if bound is not func:
                # func is a descriptor that produced something new (a bound
                # method, the function behind a staticmethod, ...).
                result = partial(bound, *self.args, **self.keywords)
                try:
                    result.__self__ = bound.__self__
                except AttributeError:
                    pass
                return result
        return self._plain_method().__get__(obj, cls)

    @property
    def __isabstractmethod__(self):
        return getattr(self.func, '__isabstractmethod__', False)

    __class_getitem__ = classmethod(GenericAlias)


def _unwrap_partial(func):
    """Follow .func through any number of partial layers."""
    while isinstance(func, partial):
        func = func.func
    return func


def _unwrap_partialmethod(func):
    """Strip partialmethod layers -- including the functions partialmethod
    hands out, which point back at it via __partialmethod__ -- and the
    partial objects found underneath."""
    while True:
        origin = getattr(func, '__partialmethod__', None)
        if isinstance(origin, partialmethod):
            func = origin.func
        elif isinstance(func, partialmethod):
            func = func.func
        elif isinstance(func, partial):
            func = func.func
        else:
            return func


# --------------------------------------------------------------------------
# lru_cache / cache

_CacheInfo = namedtuple('CacheInfo', ['hits', 'misses', 'maxsize', 'currsize'])


class _CallKey:
    """Hashable record of one call's arguments.

    The hash is computed when the key is built and then reused for every
    lookup, so an argument's __hash__ runs only once per call.
    """

    __slots__ = ('parts', 'hashvalue')

    def __init__(self, parts):
        self.parts = parts
        self.hashvalue = hash(parts)

    def __hash__(self):
        return self.hashvalue

    def __eq__(self, other):
        if type(other) is not _CallKey:
            return NotImplemented
        return self.parts == other.parts


# Separates positional from keyword data inside a key.
_KEYWORDS_FOLLOW = object()
# A lone argument of one of these exact types serves as its own key: hashing
# them is cheap and they compare reliably.
_SELF_KEYED = frozenset({int, str})


def _call_key(args, kwds, typed):
    if not kwds:
        if typed:
            return _CallKey((args, tuple(map(type, args))))
        if _len(args) == 1 and type(args[0]) in _SELF_KEYED:
            return args[0]
        return _CallKey(args)
    names = tuple(kwds)
    values = tuple(kwds.values())
    if typed:
        return _CallKey((args, _KEYWORDS_FOLLOW, names, values,
                         tuple(map(type, args)), tuple(map(type, values))))
    return _CallKey((args, _KEYWORDS_FOLLOW, names, values))


_MISSING = object()


def _make_lru_wrapper(user_function, maxsize, typed, info_type):
    # Pure-Python cache wrapper (normally replaced by the native one).
    # maxsize is None (unbounded), 0 (no caching) or a positive int.
    if not callable(user_function):
        raise TypeError('the first argument must be callable')
    store = {} if maxsize is None else _OrderedDict()
    store_get = store.get
    store_size = store.__len__
    lock = RLock()
    hits = misses = 0

    if maxsize == 0:
        def wrapper(*args, **kwds):
            nonlocal misses
            misses += 1
            return user_function(*args, **kwds)

    elif maxsize is None:
        def wrapper(*args, **kwds):
            nonlocal hits, misses
            key = _call_key(args, kwds, typed)
            value = store_get(key, _MISSING)
            if value is not _MISSING:
                hits += 1
                return value
            misses += 1
            value = user_function(*args, **kwds)
            store[key] = value
            return value

    else:
        refresh = store.move_to_end
        evict_oldest = store.popitem

        def wrapper(*args, **kwds):
            nonlocal hits, misses
            key = _call_key(args, kwds, typed)
            with lock:
                value = store_get(key, _MISSING)
                if value is not _MISSING:
                    refresh(key)
                    hits += 1
                    return value
                misses += 1
            # The user function runs without the lock held: it may recurse
            # into this cache or take a long time.
            value = user_function(*args, **kwds)
            with lock:
                # A recursive call or another thread may have stored the
                # key meanwhile; then the entry is left as it is.
                if key not in store:
                    store[key] = value
                    if store_size() > maxsize:
                        evict_oldest(last=False)
            return value

    def cache_info():
        """Report cache statistics."""
        with lock:
            return info_type(hits, misses, maxsize, store_size())

    def cache_clear():
        """Empty the cache and reset the statistics."""
        nonlocal hits, misses
        with lock:
            store.clear()
            hits = misses = 0

    wrapper.cache_info = cache_info
    wrapper.cache_clear = cache_clear
    return wrapper

try:
    from _functools import _lru_cache_wrapper as _make_lru_wrapper
except ImportError:
    pass


def lru_cache(maxsize=128, typed=False):
    """Decorator that memoizes a function, keeping the *maxsize* most
    recently used results.

    maxsize=None removes the bound; maxsize=0 disables caching.  With
    typed=True arguments of different types are cached separately (so f(3)
    and f(3.0) are distinct calls).  Arguments must be hashable.

    The decorated function gains cache_info() (hits, misses, maxsize,
    currsize), cache_clear() and cache_parameters(); the original function
    is reachable as __wrapped__.  The decorator can also be applied without
    arguments: @lru_cache.
    """
    if isinstance(maxsize, int):
        if maxsize < 0:
            maxsize = 0
    elif callable(maxsize) and isinstance(typed, bool):
        # Used as a bare decorator: maxsize is really the function.
        return _decorate_with_cache(maxsize, 128, typed)
    elif maxsize is not None:
        raise TypeError('lru_cache: maxsize must be an int, None or the '
                        'function to decorate')

    def decorating_function(user_function):
        return _decorate_with_cache(user_function, maxsize, typed)
    return decorating_function


def _decorate_with_cache(user_function, maxsize, typed):
    wrapper = _make_lru_wrapper(user_function, maxsize, typed, _CacheInfo)
    wrapper.cache_parameters = lambda: {'maxsize': maxsize, 'typed': typed}
    return update_wrapper(wrapper, user_function)


def cache(user_function, /):
    """Unbounded memoizing decorator; the same as lru_cache(maxsize=None)."""
    return lru_cache(maxsize=None)(user_function)


# --------------------------------------------------------------------------
# singledispatch: MRO computation with ABCs

def _c3_merge(sequences):
    """C3 merge of linearizations: repeatedly take the first head that does
    not occur in the tail of any sequence."""
    pending = [list(seq) for seq in sequences if seq]
    merged = []
    while pending:
        for seq in pending:
            head = seq[0]
            if not any(head in other[1:] for other in pending):
                break
        else:
            raise RuntimeError('cannot build a consistent MRO from these classes')
        merged.append(head)
        for seq in pending:
            if seq[0] is head:
                del seq[0]
        pending = [seq for seq in pending if seq]
    return merged


def _c3_mro(cls, abcs=None):
    """Linearize *cls* like type.__mro__, but also place the ABCs from
    *abcs* that cls implements without inheriting them.

    Such an implicitly implemented ABC goes right after the explicit bases up
    to and including the last base that is itself an ABC, and before the
    remaining bases.
    """
    bases = cls.__bases__
    cut = 0
    for position in range(_len(bases), 0, -1):
        if hasattr(bases[position - 1], '__abstractmethods__'):
            cut = position
            break
    leading, trailing = list(bases[:cut]), list(bases[cut:])
    candidates = list(abcs) if abcs else []
    implied = [abc for abc in candidates
               if issubclass(cls, abc)
               and not any(issubclass(base, abc) for base in bases)]
    rest = [abc for abc in candidates if abc not in implied]
    groups = leading + implied + trailing
    return _c3_merge([[cls]]
                     + [_c3_mro(base, abcs=rest) for base in groups]
                     + [leading, implied, trailing])


def _compose_mro(cls, types):
    """Return cls's MRO extended with those of *types* (typically the
    registered dispatch types) that cls is a virtual subclass of."""
    own = set(cls.__mro__)

    def matters(typ):
        return (typ not in own and hasattr(typ, '__mro__')
                and not isinstance(typ, GenericAlias)
                and issubclass(cls, typ))

    found = [typ for typ in types if matters(typ)]
    # Leave out a type when another found type already derives from it.
    found = [typ for typ in found
             if not any(typ is not other and typ in other.__mro__
                        for other in found)]
    found_set = set(found)
    ordered = []
    for typ in found:
        # Subclasses of typ that cls also virtually inherits from decide
        # where typ goes relative to the other found types.
        chains = [[t for t in sub.__mro__ if t in found_set]
                  for sub in typ.__subclasses__()
                  if sub not in own and issubclass(cls, sub)]
        if not chains:
            ordered.append(typ)
            continue
        chains.sort(key=_len, reverse=True)
        for chain in chains:
            for t in chain:
                if t not in ordered:
                    ordered.append(t)
    return _c3_mro(cls, abcs=ordered)


def _find_impl(cls, registry):
    """Pick the implementation in *registry* for an argument of type *cls*:
    the first registered type in the composed MRO.  Raise RuntimeError when
    the next type in that MRO is an equally good, unrelated match."""
    mro = _compose_mro(cls, registry.keys())
    for index, match in enumerate(mro):
        if match in registry:
            break
    else:
        return None
    if index + 1 < _len(mro):
        follower = mro[index + 1]
        if (follower in registry
                and follower not in cls.__mro__
                and match not in cls.__mro__
                and not issubclass(match, follower)):
            raise RuntimeError(f'Ambiguous dispatch: {match} or {follower}')
    return registry[match]


# --------------------------------------------------------------------------
# singledispatch

def _is_plain_class(obj):
    return isinstance(obj, type) and not isinstance(obj, GenericAlias)


def _is_dispatch_type(obj):
    """A class, or a union whose members are all classes."""
    if _is_plain_class(obj):
        return True
    if isinstance(obj, _UnionType):
        return all(_is_plain_class(member) for member in obj.__args__)
    return False


_REGISTER_USAGE = ('Use either `@register(some_class)` or plain `@register` '
                   'on an annotated function.')


def _dispatch_type_from_annotations(func):
    """Return the type annotation of the first annotated parameter of
    *func* (a function, or a staticmethod/classmethod around one), checked
    to be usable for dispatch."""
    target = func
    if isinstance(target, (staticmethod, classmethod)):
        target = target.__func__
    if getattr(target, '__annotate__', None) is None:
        raise TypeError(f'Invalid first argument to `register()`: {func!r}. '
                        f'{_REGISTER_USAGE}')
    from annotationlib import Format, ForwardRef
    import typing
    hints = typing.get_type_hints(target, format=Format.FORWARDREF)
    for argname, annotation in hints.items():
        if argname != 'return':
            break
    else:
        raise TypeError(f'Invalid first argument to `register()`: {func!r}. '
                        f'{_REGISTER_USAGE}')
    if isinstance(annotation, ForwardRef):
        raise TypeError(f'Invalid annotation for {argname!r}. '
                        f'{annotation!r} is an unresolved forward reference.')
    if not _is_dispatch_type(annotation):
        if isinstance(annotation, _UnionType):
            raise TypeError(f'Invalid annotation for {argname!r}. '
                            f'{annotation!r} not all arguments are classes.')
        raise TypeError(f'Invalid annotation for {argname!r}. '
                        f'{annotation!r} is not a class.')
    return annotation


def singledispatch(func):
    """Make *func* a single-dispatch generic function.

    Calls are routed on the type of the first positional argument to the
    implementation registered for the closest matching type (taking ABCs
    into account), falling back to *func* itself.  Implementations are added
    with the .register() attribute of the returned function.
    """
    # Imported here so that importing functools stays cheap.
    import weakref

    registry = {object: func}
    dispatch_cache = weakref.WeakKeyDictionary()
    # Set once an ABC is registered: virtual subclass registrations then
    # have to invalidate the cache.
    abc_token = None

    def dispatch(cls):
        """Return the implementation used for arguments of type *cls*."""
        nonlocal abc_token
        if abc_token is not None:
            current = _abc_cache_token()
            if current != abc_token:
                dispatch_cache.clear()
                abc_token = current
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
        """Register *func* for type *cls* (a class or a union of classes).

        Forms: register(cls, func); @register(cls); and plain @register on
        a function whose first annotated parameter names the type.
        """
        nonlocal abc_token
        if _is_dispatch_type(cls):
            if func is None:
                return lambda f: register(cls, f)
        elif func is not None:
            raise TypeError(f'Invalid first argument to `register()`: '
                            f'{cls!r} is not a class or union type.')
        elif (isinstance(cls, (staticmethod, classmethod))
              or (callable(cls)
                  and not isinstance(cls, (type, GenericAlias, _UnionType)))):
            # Plain @register: the type comes from the annotations.
            func = cls
            cls = _dispatch_type_from_annotations(func)
        else:
            raise TypeError(f'Invalid first argument to `register()`: {cls!r}. '
                            f'{_REGISTER_USAGE}')

        members = cls.__args__ if isinstance(cls, _UnionType) else (cls,)
        for member in members:
            registry[member] = func
            if abc_token is None and hasattr(member, '__abstractmethods__'):
                abc_token = _abc_cache_token()
        dispatch_cache.clear()
        return func

    funcname = getattr(func, '__name__', 'generic function')

    def wrapper(*args, **kwargs):
        if not args:
            raise TypeError(f'{funcname} requires at least 1 positional argument')
        return dispatch(args[0].__class__)(*args, **kwargs)

    wrapper.register = register
    wrapper.dispatch = dispatch
    wrapper.registry = _MappingProxyType(registry)
    wrapper._clear_cache = dispatch_cache.clear
    update_wrapper(wrapper, func)
    return wrapper


# --------------------------------------------------------------------------
# singledispatchmethod

def _display_name(func):
    name = getattr(func, '__qualname__', None)
    if name is None:
        name = getattr(func, '__name__', '?')
    return name


class singledispatchmethod:
    """Descriptor version of singledispatch, for methods.

    Dispatch happens on the first argument after self/cls.  The decorated
    object may be a function, classmethod, staticmethod or another callable;
    implementations are added with .register(), as for singledispatch.
    """

    def __init__(self, func):
        if not callable(func) and not hasattr(func, '__get__'):
            raise TypeError(f'expected a callable or a descriptor, got {func!r}')
        self.dispatcher = singledispatch(func)
        self.func = func
        # Stored on every object __get__ returns (see _DispatchingMethod).
        self._func_module = getattr(func, '__module__', None)
        self._func_doc = getattr(func, '__doc__', None)

    def register(self, cls, method=None):
        """Register *method* for *cls*; see singledispatch's register()."""
        return self.dispatcher.register(cls, func=method)

    def __get__(self, obj, cls=None):
        return _DispatchingMethod(self, obj, cls)

    @property
    def __isabstractmethod__(self):
        return getattr(self.func, '__isabstractmethod__', False)

    def __repr__(self):
        return f'<single dispatch method descriptor {_display_name(self.func)}>'


class _DispatchingMethod:
    """The callable handed out by singledispatchmethod.__get__.

    It routes each call on the type of the first argument and binds the
    chosen implementation to the instance (or class) it was fetched from.
    Other attributes are looked up on the decorated function.
    """

    __slots__ = ('_descriptor', '_obj', '_owner', '__dict__')

    def __init__(self, descriptor, obj, owner):
        # The class attributes __module__ and __doc__ would hide the
        # function's, so these two live in the instance dict; everything
        # else is forwarded by __getattr__.
        self.__module__ = descriptor._func_module
        self.__doc__ = descriptor._func_doc
        self._descriptor = descriptor
        self._obj = obj
        self._owner = owner

    @property
    def __wrapped__(self):
        return self._descriptor.func

    def register(self, cls, method=None):
        """Register *method* for *cls* on the underlying singledispatchmethod."""
        return self._descriptor.register(cls, method)

    def __getattr__(self, name):
        if name in _DispatchingMethod.__slots__:
            raise AttributeError(name)
        func = self._descriptor.func
        try:
            return getattr(func, name)
        except AttributeError:
            inner = getattr(func, '__func__', None)
            if inner is None:
                raise
            return getattr(inner, name)

    def __repr__(self):
        name = _display_name(self._descriptor.func)
        if self._obj is None:
            return f'<single dispatch method {name}>'
        return f'<bound single dispatch method {name} of {self._obj!r}>'

    def _missing_argument(self):
        name = getattr(self._descriptor.func, '__name__',
                       'generic method')
        return TypeError(f'{name} requires at least 1 positional argument')

    def __call__(self, /, *args, **kwargs):
        descriptor = self._descriptor
        obj, owner = self._obj, self._owner
        if obj is None and isinstance(descriptor.func, _FunctionType):
            # A plain method fetched from the class: the instance is passed
            # explicitly and the dispatch argument comes after it.
            if _len(args) < 2:
                raise self._missing_argument()
            impl = descriptor.dispatcher.dispatch(args[1].__class__)
            if isinstance(impl, staticmethod):
                return impl.__func__(*args[1:], **kwargs)
            binder = getattr(type(impl), '__get__', None)
            if binder is None:
                return impl(*args[1:], **kwargs)
            bound = binder(impl, None, owner)
            if getattr(bound, '__self__', None) is not None:
                # Already bound (e.g. a classmethod): the instance argument
                # is not passed on.
                return bound(*args[1:], **kwargs)
            return bound(*args, **kwargs)
        if not args:
            raise self._missing_argument()
        impl = descriptor.dispatcher.dispatch(args[0].__class__)
        binder = getattr(type(impl), '__get__', None)
        if binder is not None:
            impl = binder(impl, obj, owner)
        return impl(*args, **kwargs)


# --------------------------------------------------------------------------
# cached_property

class cached_property:
    """Like property, but the getter runs once per instance: its result is
    stored in the instance __dict__ under the attribute's name, where later
    lookups find it.  Deleting the attribute makes the next access compute
    it again.  Instances without a writable __dict__ are not supported.
    """

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
                'Cannot assign the same cached_property to two different '
                f'names ({self.attrname!r} and {name!r}).')

    def __get__(self, instance, owner=None):
        if instance is None:
            return self
        name = self.attrname
        if name is None:
            raise TypeError('Cannot use cached_property instance without '
                            'calling __set_name__ on it.')
        try:
            namespace = instance.__dict__
        except AttributeError:
            raise TypeError(
                f"No '__dict__' attribute on {type(instance).__name__!r} "
                f"instance to cache {name!r} property.") from None
        value = namespace.get(name, _MISSING)
        if value is _MISSING:
            value = self.func(instance)
            try:
                namespace[name] = value
            except TypeError:
                raise TypeError(
                    f"The '__dict__' attribute on {type(instance).__name__!r} "
                    f"instance does not support item assignment for caching "
                    f"{name!r} property.") from None
        return value

    __class_getitem__ = classmethod(GenericAlias)
