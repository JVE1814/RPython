"""Names for built-in types that are not directly accessible as builtins."""

try:
    from _types import *
except ImportError:
    import sys

    def _f(): pass
    FunctionType = type(_f)
    LambdaType = type(lambda: None)
    CodeType = type(_f.__code__)
    MappingProxyType = type(type.__dict__)
    SimpleNamespace = type(sys.implementation)

    def _cell_factory():
        a = 1
        def f():
            nonlocal a
        return f.__closure__[0]
    CellType = type(_cell_factory())

    def _g():
        yield 1
    GeneratorType = type(_g())

    async def _c(): pass
    _c = _c()
    CoroutineType = type(_c)
    _c.close()

    async def _ag():
        yield
    _ag = _ag()
    AsyncGeneratorType = type(_ag)

    class _C:
        def _m(self): pass
    MethodType = type(_C()._m)

    BuiltinFunctionType = type(len)
    BuiltinMethodType = type([].append)

    WrapperDescriptorType = type(object.__init__)
    MethodWrapperType = type(object().__str__)
    MethodDescriptorType = type(str.join)
    ClassMethodDescriptorType = type(dict.__dict__['fromkeys'])

    ModuleType = type(sys)

    try:
        raise TypeError
    except TypeError as exc:
        TracebackType = type(exc.__traceback__)
        FrameType = type(exc.__traceback__.tb_frame)

    def _get_framelocalsproxy():
        return type(sys._getframe().f_locals)
    FrameLocalsProxyType = _get_framelocalsproxy()

    GetSetDescriptorType = type(FunctionType.__code__)
    MemberDescriptorType = type(FunctionType.__globals__)

    GenericAlias = type(list[int])
    UnionType = type(int | str)

    EllipsisType = type(Ellipsis)
    NoneType = type(None)
    NotImplementedType = type(NotImplemented)

    del sys, _f, _g, _C, _c, _ag, _cell_factory, _get_framelocalsproxy



# ---------------------------------------------------------------------------
# Dynamic class creation (PEP 3115 / PEP 560)

def new_class(name, bases=(), kwds=None, exec_body=None):
    """Create a class object dynamically using the appropriate metaclass."""
    real_bases = resolve_bases(bases)
    meta, ns, kwds = prepare_class(name, real_bases, kwds)
    if exec_body is not None:
        exec_body(ns)
    if real_bases is not bases:
        ns['__orig_bases__'] = bases
    return meta(name, real_bases, ns, **kwds)


def resolve_bases(bases):
    """Resolve MRO entries dynamically as specified by PEP 560."""
    out = []
    changed = False
    for base in bases:
        if not isinstance(base, type) and hasattr(base, "__mro_entries__"):
            entries = base.__mro_entries__(bases)
            if not isinstance(entries, tuple):
                raise TypeError("__mro_entries__ must return a tuple")
            out.extend(entries)
            changed = True
        else:
            out.append(base)
    return tuple(out) if changed else bases


def prepare_class(name, bases=(), kwds=None):
    """Call the __prepare__ method of the appropriate metaclass.

    Returns (metaclass, namespace, kwds) as a 3-tuple

    *metaclass* is the appropriate metaclass
    *namespace* is the prepared class namespace
    *kwds* is an updated copy of the passed in kwds argument with any
    'metaclass' entry removed. If no kwds argument is passed in, this will
    be an empty dict.
    """
    kwds = {} if kwds is None else dict(kwds)
    if 'metaclass' in kwds:
        meta = kwds.pop('metaclass')
    elif bases:
        meta = type(bases[0])
    else:
        meta = type
    if isinstance(meta, type):
        # pick the most derived metaclass among meta and the bases' metaclasses
        meta = _calculate_meta(meta, bases)
    prepare = getattr(meta, '__prepare__', None)
    ns = prepare(name, bases, **kwds) if prepare is not None else {}
    return meta, ns, kwds


def _calculate_meta(meta, bases):
    """Calculate the most derived metaclass."""
    winner = meta
    for base in bases:
        m = type(base)
        if issubclass(winner, m):
            continue
        if issubclass(m, winner):
            winner = m
            continue
        raise TypeError("metaclass conflict: "
                        "the metaclass of a derived class "
                        "must be a (non-strict) subclass "
                        "of the metaclasses of all its bases")
    return winner


def get_original_bases(cls, /):
    """Return the class's "original" bases prior to modification by `__mro_entries__`.

    For a class built from e.g. ``Foo[int]`` or ``list[str]`` these are the
    generic aliases themselves; for other classes it is ``cls.__bases__``.
    """
    try:
        return cls.__dict__.get("__orig_bases__", cls.__bases__)
    except AttributeError:
        raise TypeError(
            f"Expected an instance of type, not {type(cls).__name__!r}"
        ) from None


class DynamicClassAttribute:
    """Route attribute access on a class to __getattr__.

    Instance access works like a property; access through the class raises
    AttributeError, so that the class's (metaclass's) __getattr__ is used.
    Enum uses this to have members and attributes of the same name.
    """
    def __init__(self, fget=None, fset=None, fdel=None, doc=None):
        self.fget = fget
        self.fset = fset
        self.fdel = fdel
        self.__doc__ = doc or fget.__doc__
        self.overwrite_doc = doc is None
        self.__isabstractmethod__ = bool(getattr(fget, '__isabstractmethod__', False))

    def __get__(self, instance, ownerclass=None):
        if instance is None:
            if self.__isabstractmethod__:
                return self
            raise AttributeError()
        if self.fget is None:
            raise AttributeError("unreadable attribute")
        return self.fget(instance)

    def __set__(self, instance, value):
        if self.fset is None:
            raise AttributeError("can't set attribute")
        self.fset(instance, value)

    def __delete__(self, instance):
        if self.fdel is None:
            raise AttributeError("can't delete attribute")
        self.fdel(instance)

    def _copy(self, fget, fset, fdel, doc):
        result = type(self)(fget, fset, fdel, doc)
        result.overwrite_doc = self.overwrite_doc
        return result

    def getter(self, fget):
        fdoc = fget.__doc__ if self.overwrite_doc else None
        return self._copy(fget, self.fset, self.fdel, fdoc or self.__doc__)

    def setter(self, fset):
        return self._copy(self.fget, fset, self.fdel, self.__doc__)

    def deleter(self, fdel):
        return self._copy(self.fget, self.fset, fdel, self.__doc__)


# ---------------------------------------------------------------------------
# types.coroutine

_CO_GENERATOR = 0x20
_CO_COROUTINE = 0x80
_CO_ITERABLE_COROUTINE = 0x100


class _GeneratorWrapper:
    # Makes a generator-like object awaitable.
    def __init__(self, gen):
        self.__wrapped = gen
        self.__isgen = gen.__class__ is GeneratorType
        self.__name__ = getattr(gen, '__name__', None)
        self.__qualname__ = getattr(gen, '__qualname__', None)
    def send(self, val):
        return self.__wrapped.send(val)
    def throw(self, tp, *rest):
        return self.__wrapped.throw(tp, *rest)
    def close(self):
        return self.__wrapped.close()
    @property
    def gi_code(self):
        return self.__wrapped.gi_code
    @property
    def gi_frame(self):
        return self.__wrapped.gi_frame
    @property
    def gi_running(self):
        return self.__wrapped.gi_running
    @property
    def gi_yieldfrom(self):
        return self.__wrapped.gi_yieldfrom
    @property
    def gi_suspended(self):
        return self.__wrapped.gi_suspended
    cr_code = gi_code
    cr_frame = gi_frame
    cr_running = gi_running
    cr_await = gi_yieldfrom
    cr_suspended = gi_suspended
    def __next__(self):
        return next(self.__wrapped)
    def __iter__(self):
        if self.__isgen:
            return self.__wrapped
        return self
    __await__ = __iter__


def coroutine(func):
    """Convert regular generator function to a coroutine."""
    if not callable(func):
        raise TypeError('types.coroutine() expects a callable')

    if (func.__class__ is FunctionType and
            getattr(func, '__code__', None).__class__ is CodeType):
        flags = func.__code__.co_flags
        if flags & (_CO_COROUTINE | _CO_ITERABLE_COROUTINE):
            return func                     # already a coroutine function
        if flags & _CO_GENERATOR:
            co = func.__code__
            func.__code__ = co.replace(co_flags=flags | _CO_ITERABLE_COROUTINE)
            return func

    # Some other callable returning a generator-like object: wrap its result.
    import functools
    import _collections_abc

    @functools.wraps(func)
    def wrapped(*args, **kwargs):
        coro = func(*args, **kwargs)
        if (coro.__class__ is CoroutineType or
                coro.__class__ is GeneratorType
                and coro.gi_code.co_flags & _CO_ITERABLE_COROUTINE):
            return coro
        if (isinstance(coro, _collections_abc.Generator) and
                not isinstance(coro, _collections_abc.Coroutine)):
            return _GeneratorWrapper(coro)
        return coro

    return wrapped


__all__ = [n for n in globals() if not n.startswith('_')]  # for pydoc
