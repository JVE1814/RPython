"""Abstract base classes for containers (the implementation behind
collections.abc).

The "one-trick pony" ABCs recognise any class that defines the relevant
special methods (setting such a method to None opts out).  The larger ABCs
provide mixin methods on top of a small set of abstract ones.
"""

from abc import ABCMeta, abstractmethod
import sys

GenericAlias = type(list[int])
EllipsisType = type(...)


def _probe():
    pass


FunctionType = type(_probe)
del _probe

__all__ = ["Awaitable", "Coroutine",
           "AsyncIterable", "AsyncIterator", "AsyncGenerator",
           "Hashable", "Iterable", "Iterator", "Generator", "Reversible",
           "Sized", "Container", "Callable", "Collection",
           "Set", "MutableSet",
           "Mapping", "MutableMapping",
           "MappingView", "KeysView", "ItemsView", "ValuesView",
           "Sequence", "MutableSequence",
           "Buffer",
           ]

# The classes below report the public module as their home, so reprs,
# pickles and typing show "collections.abc.X".
__name__ = "collections.abc"


# ---------------------------------------------------------------------------
# Structural checks
# ---------------------------------------------------------------------------

def _check_methods(C, *methods):
    """Return True if every name in *methods* is defined somewhere in the
    MRO of C with a value other than None, else NotImplemented."""
    mro = C.__mro__
    for name in methods:
        for klass in mro:
            namespace = klass.__dict__
            if name in namespace:
                if namespace[name] is None:
                    return NotImplemented
                break
        else:
            return NotImplemented
    return True


def _structural(*methods):
    """Make a __subclasshook__ that recognises classes providing *methods*.

    The hook only answers for the ABC whose namespace it was placed in;
    explicit subclasses of that ABC fall back to normal ABC rules.
    """
    def __subclasshook__(cls, C):
        if cls.__dict__.get("__subclasshook__") is hook:
            return _check_methods(C, *methods)
        return NotImplemented
    hook = classmethod(__subclasshook__)
    return hook


def _raise_thrown(typ, val, tb):
    # Default behaviour shared by the abstract throw()/athrow() methods:
    # raise what was thrown in, like a generator without handlers would.
    if val is None:
        if tb is None:
            raise typ
        val = typ()
    if tb is not None:
        val = val.with_traceback(tb)
    raise val


# ---------------------------------------------------------------------------
# One-trick ponies
# ---------------------------------------------------------------------------

class Hashable(metaclass=ABCMeta):

    __slots__ = ()

    @abstractmethod
    def __hash__(self):
        return 0

    __subclasshook__ = _structural("__hash__")


class Awaitable(metaclass=ABCMeta):

    __slots__ = ()

    @abstractmethod
    def __await__(self):
        yield

    __subclasshook__ = _structural("__await__")

    __class_getitem__ = classmethod(GenericAlias)


class Coroutine(Awaitable):

    __slots__ = ()

    @abstractmethod
    def send(self, value):
        """Resume the coroutine with *value*; returns what it yields next
        (StopIteration signals completion)."""
        raise StopIteration

    @abstractmethod
    def throw(self, typ, val=None, tb=None):
        """Resume the coroutine by raising an exception at its suspension
        point; returns what it yields next."""
        _raise_thrown(typ, val, tb)

    def close(self):
        """Finish the coroutine by throwing GeneratorExit into it."""
        try:
            self.throw(GeneratorExit)
        except (GeneratorExit, StopIteration):
            return
        raise RuntimeError("coroutine ignored GeneratorExit")

    __subclasshook__ = _structural("__await__", "send", "throw", "close")


class AsyncIterable(metaclass=ABCMeta):

    __slots__ = ()

    @abstractmethod
    def __aiter__(self):
        return AsyncIterator()

    __subclasshook__ = _structural("__aiter__")

    __class_getitem__ = classmethod(GenericAlias)


class AsyncIterator(AsyncIterable):

    __slots__ = ()

    @abstractmethod
    async def __anext__(self):
        """Produce the next value; StopAsyncIteration when there is none."""
        raise StopAsyncIteration

    def __aiter__(self):
        return self

    __subclasshook__ = _structural("__anext__", "__aiter__")


class AsyncGenerator(AsyncIterator):

    __slots__ = ()

    async def __anext__(self):
        """Advance the generator; equivalent to ``await self.asend(None)``."""
        return await self.asend(None)

    @abstractmethod
    async def asend(self, value):
        """Resume the generator with *value*; returns what it yields next
        (StopAsyncIteration signals exhaustion)."""
        raise StopAsyncIteration

    @abstractmethod
    async def athrow(self, typ, val=None, tb=None):
        """Resume the generator by raising an exception at its suspension
        point; returns what it yields next."""
        _raise_thrown(typ, val, tb)

    async def aclose(self):
        """Finish the generator by throwing GeneratorExit into it."""
        try:
            await self.athrow(GeneratorExit)
        except (GeneratorExit, StopAsyncIteration):
            return
        raise RuntimeError("asynchronous generator ignored GeneratorExit")

    __subclasshook__ = _structural("__aiter__", "__anext__",
                                   "asend", "athrow", "aclose")


class Iterable(metaclass=ABCMeta):

    __slots__ = ()

    @abstractmethod
    def __iter__(self):
        return
        yield

    __subclasshook__ = _structural("__iter__")

    __class_getitem__ = classmethod(GenericAlias)


class Iterator(Iterable):

    __slots__ = ()

    @abstractmethod
    def __next__(self):
        """Produce the next value; StopIteration when there is none."""
        raise StopIteration

    def __iter__(self):
        return self

    __subclasshook__ = _structural("__iter__", "__next__")


class Reversible(Iterable):

    __slots__ = ()

    @abstractmethod
    def __reversed__(self):
        return
        yield

    __subclasshook__ = _structural("__reversed__", "__iter__")


class Generator(Iterator):

    __slots__ = ()

    def __next__(self):
        """Advance the generator; equivalent to ``self.send(None)``."""
        return self.send(None)

    @abstractmethod
    def send(self, value):
        """Resume the generator with *value*; returns what it yields next
        (StopIteration signals exhaustion)."""
        raise StopIteration

    @abstractmethod
    def throw(self, typ, val=None, tb=None):
        """Resume the generator by raising an exception at its suspension
        point; returns what it yields next."""
        _raise_thrown(typ, val, tb)

    def close(self):
        """Finish the generator by throwing GeneratorExit into it."""
        try:
            self.throw(GeneratorExit)
        except (GeneratorExit, StopIteration):
            return
        raise RuntimeError("generator ignored GeneratorExit")

    __subclasshook__ = _structural("__iter__", "__next__",
                                   "send", "throw", "close")


class Sized(metaclass=ABCMeta):

    __slots__ = ()

    @abstractmethod
    def __len__(self):
        return 0

    __subclasshook__ = _structural("__len__")


class Container(metaclass=ABCMeta):

    __slots__ = ()

    @abstractmethod
    def __contains__(self, x):
        return False

    __subclasshook__ = _structural("__contains__")

    __class_getitem__ = classmethod(GenericAlias)


class Collection(Sized, Iterable, Container):

    __slots__ = ()

    __subclasshook__ = _structural("__len__", "__iter__", "__contains__")


class Buffer(metaclass=ABCMeta):

    __slots__ = ()

    @abstractmethod
    def __buffer__(self, flags: int, /) -> memoryview:
        raise NotImplementedError

    __subclasshook__ = _structural("__buffer__")


# ---------------------------------------------------------------------------
# Callable and its parameterised alias
# ---------------------------------------------------------------------------

def _is_param_expr(obj):
    """True for things that may stand for a whole parameter list of a
    Callable: ``...``, a list, a ParamSpec or a typing.Concatenate form."""
    if obj is Ellipsis or isinstance(obj, list):
        return True
    kind = type(obj)
    return (getattr(kind, "__module__", None) == "typing"
            and kind.__name__ in ("ParamSpec", "_ConcatenateGenericAlias"))


def _type_repr(obj):
    """repr() for a type argument, in the style typing uses."""
    if isinstance(obj, type) and not isinstance(obj, GenericAlias):
        if obj.__module__ == "builtins":
            return obj.__qualname__
        return f"{obj.__module__}.{obj.__qualname__}"
    if obj is Ellipsis:
        return "..."
    if isinstance(obj, FunctionType):
        return obj.__name__
    return repr(obj)


def _has_param_expr(args):
    # Callable[P, R] style: a single parameter expression plus the result.
    return len(args) == 2 and _is_param_expr(args[0])


class _CallableGenericAlias(GenericAlias):
    """The result of subscripting Callable, e.g. Callable[[int], str].

    __args__ are stored flattened, ``(int, str)``, so that this alias
    compares equal to typing.Callable[[int], str]; a parameter expression
    (``...``, ParamSpec, Concatenate) is kept as a single first argument.
    """

    __slots__ = ()

    def __new__(cls, origin, args):
        if not (isinstance(args, tuple) and len(args) == 2):
            raise TypeError(
                "Callable must be used as Callable[[arg, ...], result].")
        params, result = args
        if isinstance(params, (list, tuple)):
            flat = (*params, result)
        elif _is_param_expr(params):
            flat = (params, result)
        else:
            raise TypeError(
                "Expected a list of types, an ellipsis, ParamSpec, or "
                f"Concatenate. Got {params}")
        return super().__new__(cls, origin, flat)

    def __repr__(self):
        if _has_param_expr(self.__args__):
            return super().__repr__()
        *params, result = self.__args__
        inner = ", ".join(_type_repr(p) for p in params)
        return (f"collections.abc.Callable"
                f"[[{inner}], {_type_repr(result)}]")

    def __reduce__(self):
        args = self.__args__
        if not _has_param_expr(self.__args__):
            args = (list(args[:-1]), args[-1])
        return _CallableGenericAlias, (self.__origin__, args)

    def __getitem__(self, item):
        # Let GenericAlias do the type variable substitution, then fold the
        # flat result back into (parameters, result) form.
        substituted = super().__getitem__(item).__args__
        first = substituted[0]
        if isinstance(first, (tuple, list)) and len(substituted) == 2:
            pair = (list(first), substituted[1])
        elif len(substituted) == 2 and _is_param_expr(first):
            pair = (first, substituted[1])
        else:
            pair = (list(substituted[:-1]), substituted[-1])
        return _CallableGenericAlias(self.__origin__, pair)


class Callable(metaclass=ABCMeta):

    __slots__ = ()

    @abstractmethod
    def __call__(self, *args, **kwds):
        return False

    __subclasshook__ = _structural("__call__")

    __class_getitem__ = classmethod(_CallableGenericAlias)


# ---------------------------------------------------------------------------
# Sets
# ---------------------------------------------------------------------------

class Set(Collection):
    """Read-only set interface.

    Subclasses supply __contains__, __iter__ and __len__; comparisons and
    the &, |, -, ^ operators are derived from them.  Operator results are
    created through the _from_iterable() hook, which subclasses whose
    constructor does not take an iterable should override.
    """

    __slots__ = ()

    def __le__(self, other):
        if not isinstance(other, Set):
            return NotImplemented
        if len(self) > len(other):
            return False
        for elem in self:
            if elem not in other:
                return False
        return True

    def __lt__(self, other):
        if not isinstance(other, Set):
            return NotImplemented
        return len(self) < len(other) and self.__le__(other)

    def __gt__(self, other):
        if not isinstance(other, Set):
            return NotImplemented
        return len(self) > len(other) and self.__ge__(other)

    def __ge__(self, other):
        if not isinstance(other, Set):
            return NotImplemented
        if len(self) < len(other):
            return False
        for elem in other:
            if elem not in self:
                return False
        return True

    def __eq__(self, other):
        if not isinstance(other, Set):
            return NotImplemented
        return len(self) == len(other) and self.__le__(other)

    @classmethod
    def _from_iterable(cls, it):
        """Create an instance holding the values of *it* (used by the
        operators to build their results)."""
        return cls(it)

    def __and__(self, other):
        if not isinstance(other, Iterable):
            return NotImplemented
        return self._from_iterable(x for x in other if x in self)

    __rand__ = __and__

    def isdisjoint(self, other):
        """True when no value of *other* is a member of this set."""
        for x in other:
            if x in self:
                return False
        return True

    def __or__(self, other):
        if not isinstance(other, Iterable):
            return NotImplemented
        def both():
            yield from self
            yield from other
        return self._from_iterable(both())

    __ror__ = __or__

    def __sub__(self, other):
        if not isinstance(other, Set):
            if not isinstance(other, Iterable):
                return NotImplemented
            other = self._from_iterable(other)
        return self._from_iterable(x for x in self if x not in other)

    def __rsub__(self, other):
        if not isinstance(other, Set):
            if not isinstance(other, Iterable):
                return NotImplemented
            other = self._from_iterable(other)
        return self._from_iterable(x for x in other if x not in self)

    def __xor__(self, other):
        if not isinstance(other, Set):
            if not isinstance(other, Iterable):
                return NotImplemented
            other = self._from_iterable(other)
        return (self - other) | (other - self)

    __rxor__ = __xor__

    def _hash(self):
        """Hash the members the same way frozenset does.

        Set itself is not hashable; an immutable subclass may use
        ``__hash__ = Set._hash``.
        """
        bits = 2 * sys.maxsize + 1          # all-ones machine word
        acc = 0
        count = 0
        for elem in self:
            eh = hash(elem) & bits
            acc ^= (((eh ^ 89869747) ^ (eh << 16)) * 3644798167) & bits
            count += 1
        acc ^= ((count + 1) * 1927868237) & bits
        acc ^= (acc >> 11) ^ (acc >> 25)
        acc = (acc * 69069 + 907133923) & bits
        if acc > sys.maxsize:
            acc -= bits + 1
        if acc == -1:
            acc = 590923713
        return acc


class MutableSet(Set):
    """Set interface with in-place modification.

    Subclasses additionally supply add() and discard().
    """

    __slots__ = ()

    @abstractmethod
    def add(self, value):
        """Insert *value* into the set."""
        raise NotImplementedError

    @abstractmethod
    def discard(self, value):
        """Take *value* out of the set if present (no error otherwise)."""
        raise NotImplementedError

    def remove(self, value):
        """Take *value* out of the set; KeyError if it is not a member."""
        if value not in self:
            raise KeyError(value)
        self.discard(value)

    def pop(self):
        """Remove some member and return it; KeyError if the set is empty."""
        for value in self:
            break
        else:
            raise KeyError from None
        self.discard(value)
        return value

    def clear(self):
        """Remove every member (one pop() at a time)."""
        while True:
            try:
                self.pop()
            except KeyError:
                return

    def __ior__(self, it):
        for value in it:
            self.add(value)
        return self

    def __iand__(self, it):
        for value in self - it:
            self.discard(value)
        return self

    def __ixor__(self, it):
        if it is self:
            self.clear()
            return self
        if not isinstance(it, Set):
            it = self._from_iterable(it)
        for value in it:
            if value in self:
                self.discard(value)
            else:
                self.add(value)
        return self

    def __isub__(self, it):
        if it is self:
            self.clear()
        else:
            for value in it:
                self.discard(value)
        return self


# ---------------------------------------------------------------------------
# Mappings
# ---------------------------------------------------------------------------

class Mapping(Collection):
    """Read-only mapping interface.

    Subclasses supply __getitem__, __iter__ and __len__; lookups with
    defaults, membership, views and equality are derived from them.
    """

    __slots__ = ()

    # Py_TPFLAGS_MAPPING: lets instances match mapping patterns.
    __abc_tpflags__ = 1 << 6

    @abstractmethod
    def __getitem__(self, key):
        raise KeyError

    def get(self, key, default=None):
        """Look up *key*, giving *default* when it is missing."""
        try:
            return self[key]
        except KeyError:
            return default

    def __contains__(self, key):
        try:
            self[key]
        except KeyError:
            return False
        return True

    def keys(self):
        """Set-like view of the keys."""
        return KeysView(self)

    def items(self):
        """Set-like view of the (key, value) pairs."""
        return ItemsView(self)

    def values(self):
        """View of the values."""
        return ValuesView(self)

    def __eq__(self, other):
        if not isinstance(other, Mapping):
            return NotImplemented
        return dict(self.items()) == dict(other.items())

    __reversed__ = None


class MappingView(Sized):

    __slots__ = ('_mapping',)

    def __init__(self, mapping):
        self._mapping = mapping

    def __len__(self):
        return len(self._mapping)

    def __repr__(self):
        return f'{type(self).__name__}({self._mapping!r})'

    __class_getitem__ = classmethod(GenericAlias)


class KeysView(MappingView, Set):

    __slots__ = ()

    @classmethod
    def _from_iterable(cls, it):
        return set(it)

    def __contains__(self, key):
        return key in self._mapping

    def __iter__(self):
        yield from self._mapping


class ItemsView(MappingView, Set):

    __slots__ = ()

    @classmethod
    def _from_iterable(cls, it):
        return set(it)

    def __contains__(self, item):
        key, value = item
        try:
            actual = self._mapping[key]
        except KeyError:
            return False
        return actual is value or actual == value

    def __iter__(self):
        mapping = self._mapping
        for key in mapping:
            yield (key, mapping[key])


class ValuesView(MappingView, Collection):

    __slots__ = ()

    def __contains__(self, value):
        mapping = self._mapping
        for key in mapping:
            candidate = mapping[key]
            if candidate is value or candidate == value:
                return True
        return False

    def __iter__(self):
        mapping = self._mapping
        for key in mapping:
            yield mapping[key]


_absent = object()


class MutableMapping(Mapping):
    """Mapping interface with in-place modification.

    Subclasses additionally supply __setitem__ and __delitem__.
    """

    __slots__ = ()

    @abstractmethod
    def __setitem__(self, key, value):
        raise KeyError

    @abstractmethod
    def __delitem__(self, key):
        raise KeyError

    def pop(self, key, default=_absent):
        """Remove *key* and return its value.  A missing key gives
        *default* if one was passed, KeyError otherwise."""
        try:
            value = self[key]
        except KeyError:
            if default is _absent:
                raise
            return default
        del self[key]
        return value

    def popitem(self):
        """Remove some (key, value) pair and return it; KeyError when
        the mapping is empty."""
        try:
            key = next(iter(self))
        except StopIteration:
            raise KeyError from None
        value = self[key]
        del self[key]
        return key, value

    def clear(self):
        """Remove every item."""
        while True:
            try:
                self.popitem()
            except KeyError:
                return

    def update(self, other=(), /, **kwds):
        """Store the items of *other* and then of *kwds*.

        *other* may be a mapping, an object with a keys() method, or an
        iterable of (key, value) pairs.
        """
        if isinstance(other, Mapping):
            for key in other:
                self[key] = other[key]
        elif hasattr(other, "keys"):
            for key in other.keys():
                self[key] = other[key]
        else:
            for key, value in other:
                self[key] = value
        for key, value in kwds.items():
            self[key] = value

    def setdefault(self, key, default=None):
        """Return the value of *key*, first storing *default* under it if
        the key is missing."""
        try:
            return self[key]
        except KeyError:
            self[key] = default
        return default


# ---------------------------------------------------------------------------
# Sequences
# ---------------------------------------------------------------------------

class Sequence(Reversible, Collection):
    """Read-only sequence interface.

    Subclasses supply __getitem__ (integer indexes, IndexError past the
    end) and __len__; iteration, membership, reversal, index() and
    count() are derived from them.
    """

    __slots__ = ()

    # Py_TPFLAGS_SEQUENCE: lets instances match sequence patterns.
    __abc_tpflags__ = 1 << 5

    @abstractmethod
    def __getitem__(self, index):
        raise IndexError

    def __iter__(self):
        i = 0
        try:
            while True:
                item = self[i]
                yield item
                i += 1
        except IndexError:
            return

    def __contains__(self, value):
        for item in self:
            if item is value or item == value:
                return True
        return False

    def __reversed__(self):
        for i in range(len(self) - 1, -1, -1):
            yield self[i]

    def index(self, value, start=0, stop=None):
        """Position of the first occurrence of *value* within
        [start:stop]; ValueError when it does not occur."""
        if start is not None and start < 0:
            start = max(len(self) + start, 0)
        if stop is not None and stop < 0:
            stop += len(self)

        i = start
        while stop is None or i < stop:
            try:
                item = self[i]
            except IndexError:
                break
            if item is value or item == value:
                return i
            i += 1
        raise ValueError

    def count(self, value):
        """How many items are equal to (or are) *value*."""
        n = 0
        for item in self:
            if item is value or item == value:
                n += 1
        return n


def _warn_bytestring():
    import warnings
    warnings._deprecated("collections.abc.ByteString", remove=(3, 17))


class _ByteStringWarningMeta(ABCMeta):
    # ByteString is deprecated: warn whenever it is subclassed or used in
    # an isinstance() check.

    def __new__(mcls, name, bases, namespace, /, **kwargs):
        if any(isinstance(b, _ByteStringWarningMeta) for b in bases):
            _warn_bytestring()
        return super().__new__(mcls, name, bases, namespace, **kwargs)

    def __instancecheck__(cls, instance):
        _warn_bytestring()
        return super().__instancecheck__(instance)


class ByteString(Sequence, metaclass=_ByteStringWarningMeta):
    """Deprecated common supertype of bytes and bytearray."""

    __slots__ = ()


# Only reachable through the warning __getattr__ hooks below and in
# collections.abc.
_ByteString = ByteString
del ByteString


def __getattr__(attr):
    if attr == "ByteString":
        _warn_bytestring()
        return _ByteString
    raise AttributeError(f"module {__name__!r} has no attribute {attr!r}")


class MutableSequence(Sequence):
    """Sequence interface with in-place modification.

    Subclasses additionally supply __setitem__, __delitem__ and insert().
    """

    __slots__ = ()

    @abstractmethod
    def __setitem__(self, index, value):
        raise IndexError

    @abstractmethod
    def __delitem__(self, index):
        raise IndexError

    @abstractmethod
    def insert(self, index, value):
        """Put *value* in front of position *index*."""
        raise IndexError

    def append(self, value):
        """Add *value* after the last item."""
        self.insert(len(self), value)

    def clear(self):
        """Remove every item."""
        try:
            while True:
                self.pop()
        except IndexError:
            pass

    def reverse(self):
        """Reverse the order of the items in place."""
        lo, hi = 0, len(self) - 1
        while lo < hi:
            self[lo], self[hi] = self[hi], self[lo]
            lo += 1
            hi -= 1

    def extend(self, values):
        """Append every value produced by the iterable *values*."""
        if values is self:
            values = list(values)
        for v in values:
            self.append(v)

    def pop(self, index=-1):
        """Remove the item at *index* (the last one by default) and return
        it; IndexError when there is no such item."""
        item = self[index]
        del self[index]
        return item

    def remove(self, value):
        """Delete the first item equal to *value*; ValueError if none is."""
        del self[self.index(value)]

    def __iadd__(self, values):
        self.extend(values)
        return self


# ---------------------------------------------------------------------------
# Registration of the built-in types
# ---------------------------------------------------------------------------

def _type_of_iter(obj):
    return type(iter(obj))


# Several built-in types have no public name; they are bound here so that
# they can be registered (and looked up by name).
bytes_iterator = _type_of_iter(b'')
bytearray_iterator = _type_of_iter(bytearray())
dict_keyiterator = _type_of_iter({}.keys())
dict_valueiterator = _type_of_iter({}.values())
dict_itemiterator = _type_of_iter({}.items())
list_iterator = _type_of_iter([])
list_reverseiterator = _type_of_iter(reversed([]))
range_iterator = _type_of_iter(range(0))
longrange_iterator = _type_of_iter(range(1 << 1000))
set_iterator = _type_of_iter(set())
str_iterator = _type_of_iter("")
tuple_iterator = _type_of_iter(())
zip_iterator = type(zip())
dict_keys = type({}.keys())
dict_values = type({}.values())
dict_items = type({}.items())
mappingproxy = type(type.__dict__)


def _frame_locals():
    return sys._getframe().f_locals


framelocalsproxy = type(_frame_locals())
del _frame_locals


def _plain_generator():
    yield


generator = type(_plain_generator())
del _plain_generator


async def _plain_coroutine():
    pass


_coro = _plain_coroutine()
coroutine = type(_coro)
_coro.close()
del _coro, _plain_coroutine


async def _plain_async_generator():
    yield


async_generator = type(_plain_async_generator())
del _plain_async_generator


def _register_builtins():
    table = {
        Iterator: (bytes_iterator, bytearray_iterator, dict_keyiterator,
                   dict_valueiterator, dict_itemiterator, list_iterator,
                   list_reverseiterator, range_iterator, longrange_iterator,
                   set_iterator, str_iterator, tuple_iterator, zip_iterator),
        Generator: (generator,),
        Coroutine: (coroutine,),
        AsyncGenerator: (async_generator,),
        Set: (frozenset,),
        MutableSet: (set,),
        KeysView: (dict_keys,),
        ItemsView: (dict_items,),
        ValuesView: (dict_values,),
        Mapping: (mappingproxy,),
        MutableMapping: (dict,),
        Sequence: (tuple, str, range, memoryview, bytes),
        MutableSequence: (list, bytearray),
        _ByteString: (bytes, bytearray),
    }
    for abc, concrete in table.items():
        for cls in concrete:
            abc.register(cls)
    # Frame-locals proxies are read/write views but not full mutable
    # mappings (no deletion); some builds hand out plain dicts instead.
    if framelocalsproxy is not dict:
        Mapping.register(framelocalsproxy)
    frozen = getattr(sys.modules.get("builtins"), "frozendict", None)
    if frozen is not None:
        Mapping.register(frozen)


_register_builtins()
del _register_builtins
