"""Type objects of the interpreter that have no builtin name, plus helpers
for creating classes dynamically (new_class, prepare_class, resolve_bases,
get_original_bases), DynamicClassAttribute and the coroutine() decorator.
"""

try:
    from _types import *
    from _types import __all__ as _native_names
except ImportError:
    # No accelerator: find each type from an object of that type.
    import sys as _sys

    def _function():
        pass

    def _make_cell():
        value = None
        def inner():
            return value
        return inner.__closure__[0]

    def _generator():
        yield

    async def _coroutine():
        pass

    async def _async_generator():
        yield

    class _Holder:
        def method(self):
            pass

    def _locals_proxy_type():
        return type(_sys._getframe().f_locals)

    def _traceback_and_frame():
        try:
            raise TypeError
        except TypeError as exc:
            tb = exc.__traceback__
            return type(tb), type(tb.tb_frame)

    FunctionType = LambdaType = type(_function)
    CodeType = type(_function.__code__)
    MappingProxyType = type(type.__dict__)
    SimpleNamespace = type(_sys.implementation)
    CellType = type(_make_cell())
    GeneratorType = type(_generator())
    _sample = _coroutine()
    CoroutineType = type(_sample)
    _sample.close()
    _sample = _async_generator()
    AsyncGeneratorType = type(_sample)
    MethodType = type(_Holder().method)
    BuiltinFunctionType = BuiltinMethodType = type(len)
    WrapperDescriptorType = type(object.__init__)
    MethodWrapperType = type(object().__str__)
    MethodDescriptorType = type(str.join)
    ClassMethodDescriptorType = type(dict.__dict__['fromkeys'])
    ModuleType = type(_sys)
    TracebackType, FrameType = _traceback_and_frame()
    FrameLocalsProxyType = _locals_proxy_type()
    GetSetDescriptorType = type(FunctionType.__code__)
    MemberDescriptorType = type(FunctionType.__globals__)
    GenericAlias = type(list[int])
    UnionType = type(int | str)
    EllipsisType = type(Ellipsis)
    NoneType = type(None)
    NotImplementedType = type(NotImplemented)

    _native_names = [
        'AsyncGeneratorType', 'BuiltinFunctionType', 'BuiltinMethodType',
        'CellType', 'ClassMethodDescriptorType', 'CodeType', 'CoroutineType',
        'EllipsisType', 'FrameLocalsProxyType', 'FrameType', 'FunctionType',
        'GeneratorType', 'GenericAlias', 'GetSetDescriptorType', 'LambdaType',
        'MappingProxyType', 'MemberDescriptorType', 'MethodDescriptorType',
        'MethodType', 'MethodWrapperType', 'ModuleType', 'NoneType',
        'NotImplementedType', 'SimpleNamespace', 'TracebackType', 'UnionType',
        'WrapperDescriptorType',
    ]
    del (_sys, _function, _make_cell, _generator, _coroutine, _async_generator,
         _Holder, _locals_proxy_type, _traceback_and_frame, _sample)

__all__ = list(_native_names) + [
    'new_class', 'resolve_bases', 'prepare_class', 'get_original_bases',
    'DynamicClassAttribute', 'coroutine',
]
del _native_names


# --------------------------------------------------------------------------
# Dynamic class creation

def new_class(name, bases=(), kwds=None, exec_body=None):
    """Create a class the way a class statement would.

    *kwds* holds the keywords of the class statement (including
    'metaclass'); *exec_body*, if given, is called with the fresh namespace
    to fill it, like the body of the statement.
    """
    resolved = resolve_bases(bases)
    meta, namespace, kwds = prepare_class(name, resolved, kwds)
    if exec_body is not None:
        exec_body(namespace)
    if resolved is not bases:
        namespace['__orig_bases__'] = bases
    return meta(name, resolved, namespace, **kwds)


def resolve_bases(bases):
    """Apply the __mro_entries__ protocol (PEP 560) to *bases*.

    Each base that is not a class but has __mro_entries__ is replaced by the
    tuple that method returns.  *bases* itself is returned when nothing was
    replaced.
    """
    resolved = []
    replaced = False
    for base in bases:
        if not isinstance(base, type):
            hook = getattr(base, '__mro_entries__', None)
            if hook is not None:
                entries = hook(bases)
                if not isinstance(entries, tuple):
                    raise TypeError('__mro_entries__ must return a tuple')
                resolved.extend(entries)
                replaced = True
                continue
        resolved.append(base)
    return tuple(resolved) if replaced else bases


def prepare_class(name, bases=(), kwds=None):
    """Return (metaclass, namespace, remaining keywords) for a class.

    The metaclass is kwds['metaclass'] if given, else the type of the first
    base (or type); when it is a class, the most derived metaclass among it
    and the bases' metaclasses is chosen.  The namespace comes from the
    metaclass's __prepare__, if it has one.  *kwds* is not modified.
    """
    kwds = {} if kwds is None else dict(kwds)
    if 'metaclass' in kwds:
        meta = kwds.pop('metaclass')
    elif bases:
        meta = type(bases[0])
    else:
        meta = type
    if isinstance(meta, type):
        meta = _winning_metaclass(meta, bases)
    prepare = getattr(meta, '__prepare__', None)
    namespace = {} if prepare is None else prepare(name, bases, **kwds)
    return meta, namespace, kwds


def _winning_metaclass(meta, bases):
    # The candidate must be a subclass of every base's metaclass, or be
    # replaced by one that is.
    winner = meta
    for base in bases:
        other = type(base)
        if issubclass(winner, other):
            continue
        if issubclass(other, winner):
            winner = other
            continue
        raise TypeError('metaclass conflict: the metaclass of a derived class '
                        'must be a (non-strict) subclass of the metaclasses '
                        'of all its bases')
    return winner


def get_original_bases(cls, /):
    """Return the bases of *cls* as written in its class statement, before
    __mro_entries__ was applied (e.g. Generic[T] rather than Generic).

    For classes created without such bases this is cls.__bases__.
    """
    try:
        return cls.__dict__.get('__orig_bases__', cls.__bases__)
    except AttributeError:
        raise TypeError(f'Expected an instance of type, '
                        f'not {type(cls).__name__!r}') from None


# --------------------------------------------------------------------------
# DynamicClassAttribute

class DynamicClassAttribute:
    """Property whose class-level lookup fails, deferring to __getattr__.

    On instances it behaves like property.  Looked up on the class it
    raises AttributeError (unless the getter is abstract), so a
    metaclass __getattr__ can supply a different value for the same name;
    enum uses this for members that share names with attributes.
    """

    def __init__(self, fget=None, fset=None, fdel=None, doc=None):
        self.fget = fget
        self.fset = fset
        self.fdel = fdel
        # a doc taken from the getter is replaced along with the getter
        self.overwrite_doc = doc is None
        self.__doc__ = doc if doc is not None else getattr(fget, '__doc__', None)
        self.__isabstractmethod__ = bool(
            getattr(fget, '__isabstractmethod__', False))

    def __get__(self, instance, ownerclass=None):
        if instance is None:
            if self.__isabstractmethod__:
                return self
            raise AttributeError()
        if self.fget is None:
            raise AttributeError('unreadable attribute')
        return self.fget(instance)

    def __set__(self, instance, value):
        if self.fset is None:
            raise AttributeError("can't set attribute")
        self.fset(instance, value)

    def __delete__(self, instance):
        if self.fdel is None:
            raise AttributeError("can't delete attribute")
        self.fdel(instance)

    def _with(self, fget, fset, fdel, doc):
        clone = type(self)(fget, fset, fdel, doc)
        clone.overwrite_doc = self.overwrite_doc
        return clone

    def getter(self, fget):
        doc = getattr(fget, '__doc__', None) if self.overwrite_doc else None
        return self._with(fget, self.fset, self.fdel, doc or self.__doc__)

    def setter(self, fset):
        return self._with(self.fget, fset, self.fdel, self.__doc__)

    def deleter(self, fdel):
        return self._with(self.fget, self.fset, fdel, self.__doc__)


# --------------------------------------------------------------------------
# coroutine()

_FLAG_COROUTINE = 0x80
_FLAG_ITERABLE_COROUTINE = 0x100
_FLAG_GENERATOR = 0x20


class _GeneratorWrapper:
    """Makes a generator (or generator-like object) returned by a
    types.coroutine-decorated function usable with 'await'.

    Everything is forwarded to the wrapped object; the cr_* attributes are
    aliases of the corresponding gi_* ones.
    """

    def __init__(self, gen):
        self.__wrapped = gen
        self.__native = gen.__class__ is GeneratorType
        self.__name__ = getattr(gen, '__name__', None)
        self.__qualname__ = getattr(gen, '__qualname__', None)

    def send(self, value):
        return self.__wrapped.send(value)

    def throw(self, typ, *rest):
        return self.__wrapped.throw(typ, *rest)

    def close(self):
        return self.__wrapped.close()

    def __next__(self):
        return next(self.__wrapped)

    def __iter__(self):
        # A real generator can be handed to 'await' directly.
        return self.__wrapped if self.__native else self

    __await__ = __iter__

    def _forward(name):
        return property(lambda self: getattr(self.__wrapped, name))

    gi_code = cr_code = _forward('gi_code')
    gi_frame = cr_frame = _forward('gi_frame')
    gi_running = cr_running = _forward('gi_running')
    gi_suspended = cr_suspended = _forward('gi_suspended')
    gi_yieldfrom = cr_await = _forward('gi_yieldfrom')
    del _forward


def coroutine(func):
    """Decorator letting a generator-based function be awaited.

    A generator function gets the CO_ITERABLE_COROUTINE code flag (and is
    returned itself); 'async def' functions are returned unchanged.  Any
    other callable is wrapped so that a plain generator it returns is
    wrapped in an awaitable proxy.
    """
    if not callable(func):
        raise TypeError('types.coroutine() expects a callable')

    if func.__class__ is FunctionType:
        code = getattr(func, '__code__', None)
        if code is not None:
            flags = code.co_flags
            if flags & (_FLAG_COROUTINE | _FLAG_ITERABLE_COROUTINE):
                return func
            if flags & _FLAG_GENERATOR:
                func.__code__ = code.replace(
                    co_flags=flags | _FLAG_ITERABLE_COROUTINE)
                return func

    import functools
    import _collections_abc

    @functools.wraps(func)
    def wrapped(*args, **kwargs):
        result = func(*args, **kwargs)
        if result.__class__ is CoroutineType:
            return result
        if result.__class__ is GeneratorType:
            if result.gi_code.co_flags & _FLAG_ITERABLE_COROUTINE:
                return result
            return _GeneratorWrapper(result)
        if (isinstance(result, _collections_abc.Generator)
                and not isinstance(result, _collections_abc.Coroutine)):
            # a generator-like object that cannot be awaited by itself
            return _GeneratorWrapper(result)
        return result

    return wrapped
