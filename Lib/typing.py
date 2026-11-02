"""Support for type hints (PEP 484 and later typing PEPs).

Provides the special forms (Any, Union, Optional, Literal, Callable, ...),
generic machinery (Generic, Protocol, TypeVar, ParamSpec, TypeVarTuple),
helpers such as get_type_hints/get_origin/get_args/cast/overload, the
NamedTuple/TypedDict/NewType constructors and deprecated aliases of the
builtin and collections.abc container types.

Names not listed in __all__ are private implementation details.
"""

from abc import abstractmethod, ABCMeta
import collections
from collections import defaultdict
import collections.abc
import copyreg
import functools
import operator
import sys
import types
from types import GenericAlias

from _typing import (
    _idfunc,
    TypeVar,
    ParamSpec,
    TypeVarTuple,
    ParamSpecArgs,
    ParamSpecKwargs,
    TypeAliasType,
    Generic,
    Union,
    NoDefault,
)

__all__ = [
    # special primitives
    'Annotated', 'Any', 'Callable', 'ClassVar', 'Concatenate', 'Final',
    'ForwardRef', 'Generic', 'Literal', 'Optional', 'ParamSpec', 'Protocol',
    'Tuple', 'Type', 'TypeForm', 'TypeVar', 'TypeVarTuple', 'Union',
    # collections.abc aliases
    'AbstractSet', 'Container', 'ContextManager', 'Hashable', 'ItemsView',
    'Iterable', 'Iterator', 'KeysView', 'Mapping', 'MappingView',
    'MutableMapping', 'MutableSequence', 'MutableSet', 'Sequence', 'Sized',
    'ValuesView', 'Awaitable', 'AsyncIterator', 'AsyncIterable', 'Coroutine',
    'Collection', 'AsyncGenerator', 'AsyncContextManager',
    # protocols
    'Reversible', 'SupportsAbs', 'SupportsBytes', 'SupportsComplex',
    'SupportsFloat', 'SupportsIndex', 'SupportsInt', 'SupportsRound',
    # concrete collections
    'ChainMap', 'Counter', 'Deque', 'Dict', 'DefaultDict', 'List',
    'OrderedDict', 'Set', 'FrozenSet', 'NamedTuple', 'TypedDict', 'Generator',
    # other concrete types
    'BinaryIO', 'IO', 'Match', 'Pattern', 'TextIO',
    # one-off things
    'AnyStr', 'assert_type', 'assert_never', 'cast', 'clear_overloads',
    'dataclass_transform', 'disjoint_base', 'evaluate_forward_ref', 'final',
    'get_args', 'get_origin', 'get_overloads', 'get_protocol_members',
    'get_type_hints', 'is_protocol', 'is_typeddict', 'LiteralString', 'Never',
    'NewType', 'no_type_check', 'NoDefault', 'NoExtraItems', 'NoReturn',
    'NotRequired', 'overload', 'override', 'ParamSpecArgs', 'ParamSpecKwargs',
    'ReadOnly', 'Required', 'reveal_type', 'runtime_checkable', 'Self', 'Text',
    'TYPE_CHECKING', 'TypeAlias', 'TypeGuard', 'TypeIs', 'TypeAliasType',
    'Unpack',
]


class _LazyAnnotationLib:
    """Imports annotationlib on first attribute access (keeps `import typing` light)."""
    def __getattr__(self, attr):
        global _lazy_annotationlib
        import annotationlib
        _lazy_annotationlib = annotationlib
        return getattr(annotationlib, attr)

_lazy_annotationlib = _LazyAnnotationLib()


class _Sentinel:
    __slots__ = ()
    def __repr__(self):
        return '<sentinel>'

_sentinel = _Sentinel()


# ---------------------------------------------------------------- basic helpers

def _type_convert(arg, module=None, *, allow_special_forms=False, owner=None):
    """None -> NoneType, str -> ForwardRef, anything else unchanged."""
    if arg is None:
        return type(None)
    if isinstance(arg, str):
        return _make_forward_ref(arg, module=module, is_class=allow_special_forms, owner=owner)
    return arg


def _type_check(arg, msg, is_argument=True, module=None, *, allow_special_forms=False, owner=None):
    """Validate a type argument and return it (converted with _type_convert).

    `msg` prefixes the error raised for tuples (the repr of the value is appended).
    """
    invalid_generic_forms = (Generic, Protocol)
    if not allow_special_forms:
        invalid_generic_forms += (ClassVar,)
        if is_argument:
            invalid_generic_forms += (Final,)

    arg = _type_convert(arg, module=module, allow_special_forms=allow_special_forms, owner=owner)
    if isinstance(arg, _GenericAlias) and arg.__origin__ in invalid_generic_forms:
        raise TypeError(f"{arg} is not valid as type argument")
    if arg in (Any, LiteralString, NoReturn, Never, Self, TypeAlias):
        return arg
    if allow_special_forms and arg in (ClassVar, Final):
        return arg
    if isinstance(arg, _SpecialForm) or arg in (Generic, Protocol):
        raise TypeError(f"Plain {arg} is not valid as type argument")
    if type(arg) is tuple:
        raise TypeError(f"{msg} Got {arg!r:.100}.")
    return arg


def _is_param_expr(arg):
    return arg is ... or isinstance(arg, (tuple, list, ParamSpec, _ConcatenateGenericAlias))


def _should_unflatten_callable_args(typ, args):
    # collections.abc.Callable[[int, str], float].__args__ is flat: (int, str, float).
    return (
        typ.__origin__ is collections.abc.Callable
        and not (len(args) == 2 and _is_param_expr(args[0]))
    )


def _type_repr(obj):
    """Short repr for types (module.qualname), repr() for everything else."""
    if isinstance(obj, tuple):
        return '[' + ', '.join(_type_repr(t) for t in obj) + ']'
    if isinstance(obj, (type, types.FunctionType, types.BuiltinFunctionType)):
        if obj.__module__ == 'builtins':
            return obj.__qualname__
        return f'{obj.__module__}.{obj.__qualname__}'
    if obj is ...:
        return '...'
    return repr(obj)


def _is_unpacked_typevartuple(x):
    return ((not isinstance(x, type)) and
            getattr(x, '__typing_is_unpacked_typevartuple__', False) is True)


def _is_typevar_like(x):
    return isinstance(x, (TypeVar, ParamSpec)) or _is_unpacked_typevartuple(x)


def _collect_type_parameters(args, *, enforce_default_ordering=True, validate_all=False):
    """Type parameters occurring in `args`, in order of first appearance."""
    default_encountered = False
    type_var_tuple_encountered = False
    parameters = []
    for t in args:
        if isinstance(t, type):
            # a plain class: its __parameters__ descriptor is irrelevant
            pass
        elif isinstance(t, tuple):
            # a ParamSpec substituted with a list of types
            for x in t:
                for collected in _collect_type_parameters([x]):
                    if collected not in parameters:
                        parameters.append(collected)
        elif hasattr(t, '__typing_subst__'):
            if t not in parameters:
                if enforce_default_ordering:
                    if type_var_tuple_encountered and t.has_default():
                        raise TypeError('Type parameter with a default follows TypeVarTuple')
                    if t.has_default():
                        default_encountered = True
                    elif default_encountered:
                        raise TypeError(f'Type parameter {t!r} without a default'
                                        ' follows type parameter with a default')
                parameters.append(t)
        elif (not validate_all and isinstance(t, _GenericAlias)
              and t.__origin__ in (Generic, Protocol)):
            # an explicit Generic[...] / Protocol[...] base fixes the order
            parameters = t.__parameters__
        else:
            if _is_unpacked_typevartuple(t):
                type_var_tuple_encountered = True
            for x in getattr(t, '__parameters__', ()):
                if x not in parameters:
                    parameters.append(x)
    return tuple(parameters)


def _check_generic_specialization(cls, arguments):
    """Raise TypeError when the number of arguments does not fit cls.__parameters__."""
    expected_len = len(cls.__parameters__)
    if not expected_len:
        raise TypeError(f"{cls} is not a generic class")
    actual_len = len(arguments)
    if actual_len != expected_len:
        if actual_len < expected_len:
            # missing trailing parameters are fine when they all have defaults
            if cls.__parameters__[actual_len].has_default():
                return
            expected_len -= sum(p.has_default() for p in cls.__parameters__)
            expect_val = f"at least {expected_len}"
        else:
            expect_val = expected_len
        raise TypeError(f"Too {'many' if actual_len > expected_len else 'few'} arguments"
                        f" for {cls}; actual {actual_len}, expected {expect_val}")


def _unpack_args(*args):
    newargs = []
    for arg in args:
        subargs = getattr(arg, '__typing_unpacked_tuple_args__', None)
        if subargs is not None and not (subargs and subargs[-1] is ...):
            newargs.extend(subargs)
        else:
            newargs.append(arg)
    return newargs


def _deduplicate(params, *, unhashable_fallback=False):
    try:
        return dict.fromkeys(params)
    except TypeError:
        if not unhashable_fallback:
            raise
        result = []
        for t in params:
            if t not in result:
                result.append(t)
        return result


def _flatten_literal_params(parameters):
    params = []
    for p in parameters:
        if isinstance(p, _LiteralGenericAlias):
            params.extend(p.__args__)
        else:
            params.append(p)
    return tuple(params)


_cleanups = []
_caches = {}


def _tp_cache(func=None, /, *, typed=False):
    """Cache results of subscription; unhashable arguments bypass the cache."""
    def decorator(func):
        cache = functools.lru_cache(typed=typed)(func)
        # look the cache up through a global dict so that the wrapper does not
        # keep a direct reference to it
        _caches[func] = cache
        _cleanups.append(cache.cache_clear)
        del cache

        @functools.wraps(func)
        def inner(*args, **kwds):
            try:
                return _caches[func](*args, **kwds)
            except TypeError:
                pass
            return func(*args, **kwds)
        return inner

    if func is not None:
        return decorator(func)
    return decorator


def _deprecation_warning_for_no_type_params_passed(funcname: str) -> None:
    import warnings
    msg = (
        f"Failing to pass a value to the 'type_params' parameter "
        f"of {funcname!r} is deprecated, as it leads to incorrect behaviour "
        f"when calling {funcname} on a stringified annotation "
        f"that references a PEP 695 type parameter. "
        f"It will be disallowed in Python 3.15."
    )
    warnings.warn(msg, category=DeprecationWarning, stacklevel=3)


def _eval_type(t, globalns, localns, type_params=_sentinel, *, recursive_guard=frozenset(),
               format=None, owner=None, parent_fwdref=None):
    """Recursively evaluate the forward references contained in `t`."""
    if type_params is _sentinel:
        _deprecation_warning_for_no_type_params_passed("typing._eval_type")
        type_params = ()
    if isinstance(t, _lazy_annotationlib.ForwardRef):
        if t.__forward_module__ is not None:
            globalns = None
        return evaluate_forward_ref(t, globals=globalns, locals=localns,
                                    type_params=type_params, owner=owner,
                                    _recursive_guard=recursive_guard, format=format)
    if isinstance(t, (_GenericAlias, GenericAlias, Union)):
        if isinstance(t, GenericAlias):
            args = tuple(
                _make_forward_ref(arg, parent_fwdref=parent_fwdref) if isinstance(arg, str) else arg
                for arg in t.__args__
            )
            is_unpacked = t.__unpacked__
            if _should_unflatten_callable_args(t, args):
                t = t.__origin__[(args[:-1], args[-1])]
            elif any(a is not b for a, b in zip(args, t.__args__)):
                # rebuild with the same alias class (subclasses are kept)
                t = type(t)(t.__origin__, args)
            if is_unpacked and not t.__unpacked__:
                t = next(iter(t))

        ev_args = tuple(
            _eval_type(a, globalns, localns, type_params, recursive_guard=recursive_guard,
                       format=format, owner=owner, parent_fwdref=parent_fwdref)
            for a in t.__args__
        )
        if ev_args == t.__args__:
            return t
        if isinstance(t, GenericAlias):
            new = type(t)(t.__origin__, ev_args)
            if t.__unpacked__:
                new = next(iter(new))
            return new
        if isinstance(t, Union):
            return functools.reduce(operator.or_, ev_args)
        return t.copy_with(ev_args)
    return t


# ---------------------------------------------------------------- special forms

class _Final:
    """Mixin forbidding subclassing outside this module."""

    __slots__ = ('__weakref__',)

    def __init_subclass__(cls, /, *args, **kwds):
        if '_root' not in kwds:
            raise TypeError("Cannot subclass special typing classes")


class _NotIterable:
    """Mixin making instances non-iterable without looking Iterable to ABCs."""

    __slots__ = ()
    __iter__ = None


class _SpecialForm(_Final, _NotIterable, _root=True):
    __slots__ = ('_name', '__doc__', '_getitem')

    def __init__(self, getitem):
        self._getitem = getitem
        self._name = getitem.__name__
        self.__doc__ = getitem.__doc__

    def __getattr__(self, item):
        if item in {'__name__', '__qualname__'}:
            return self._name
        raise AttributeError(item)

    def __mro_entries__(self, bases):
        raise TypeError(f"Cannot subclass {self!r}")

    def __repr__(self):
        return 'typing.' + self._name

    def __reduce__(self):
        return self._name

    def __call__(self, *args, **kwds):
        raise TypeError(f"Cannot instantiate {self!r}")

    def __or__(self, other):
        return Union[self, other]

    def __ror__(self, other):
        return Union[other, self]

    def __instancecheck__(self, obj):
        raise TypeError(f"{self} cannot be used with isinstance()")

    def __subclasscheck__(self, cls):
        raise TypeError(f"{self} cannot be used with issubclass()")

    @_tp_cache
    def __getitem__(self, parameters):
        return self._getitem(self, parameters)


class _TypedCacheSpecialForm(_SpecialForm, _root=True):
    def __getitem__(self, parameters):
        if not isinstance(parameters, tuple):
            parameters = (parameters,)
        return self._getitem(self, *parameters)


class _TypeFormForm(_SpecialForm, _root=True):
    # TypeForm(x) returns x unchanged (it only informs type checkers).
    def __call__(self, obj, /):
        return obj


class _AnyMeta(type):
    def __instancecheck__(self, obj):
        if self is Any:
            raise TypeError("typing.Any cannot be used with isinstance()")
        return super().__instancecheck__(obj)

    def __repr__(self):
        if self is Any:
            return "typing.Any"
        return super().__repr__()


class Any(metaclass=_AnyMeta):
    """The type compatible with every other type (not for runtime checks)."""

    def __new__(cls, *args, **kwargs):
        if cls is Any:
            raise TypeError("Any cannot be instantiated")
        return super().__new__(cls)


@_SpecialForm
def NoReturn(self, parameters):
    """Return type of functions that never return normally."""
    raise TypeError(f"{self} is not subscriptable")


@_SpecialForm
def Never(self, parameters):
    """The bottom type: a type with no values."""
    raise TypeError(f"{self} is not subscriptable")


@_SpecialForm
def Self(self, parameters):
    """The type of the current class (for methods returning self)."""
    raise TypeError(f"{self} is not subscriptable")


@_SpecialForm
def LiteralString(self, parameters):
    """Any string built only from literal strings."""
    raise TypeError(f"{self} is not subscriptable")


@_SpecialForm
def ClassVar(self, parameters):
    """Marks an attribute annotation as a class variable: ClassVar[int]."""
    item = _type_check(parameters, f'{self} accepts only single type.', allow_special_forms=True)
    return _GenericAlias(self, (item,))


@_SpecialForm
def Final(self, parameters):
    """Marks a name that must not be reassigned or overridden: Final[int]."""
    item = _type_check(parameters, f'{self} accepts only single type.', allow_special_forms=True)
    return _GenericAlias(self, (item,))


@_SpecialForm
def Optional(self, parameters):
    """Optional[X] means X | None."""
    arg = _type_check(parameters, f"{self} requires a single type.")
    return Union[arg, type(None)]


@_TypedCacheSpecialForm
@_tp_cache(typed=True)
def Literal(self, *parameters):
    """A type whose values are exactly the given literal values: Literal['r', 'w']."""
    # the arguments are values, not types: no _type_check
    parameters = _flatten_literal_params(parameters)
    try:
        parameters = tuple(p for p, _ in _deduplicate(
            list(_value_and_type_iter(parameters)), unhashable_fallback=True))
    except TypeError:  # unhashable values
        pass
    return _LiteralGenericAlias(self, parameters)


@_SpecialForm
def TypeAlias(self, parameters):
    """Annotation marking an explicit type alias: X: TypeAlias = int."""
    raise TypeError(f"{self} is not subscriptable")


@_SpecialForm
def Concatenate(self, parameters):
    """Prepends parameters to a ParamSpec: Callable[Concatenate[int, P], R]."""
    if parameters == ():
        raise TypeError("Cannot take a Concatenate of no types.")
    if not isinstance(parameters, tuple):
        parameters = (parameters,)
    if not (parameters[-1] is ... or isinstance(parameters[-1], ParamSpec)):
        raise TypeError("The last parameter to Concatenate should be a "
                        "ParamSpec variable or ellipsis.")
    msg = "Concatenate[arg, ...]: each arg must be a type."
    parameters = (*(_type_check(p, msg) for p in parameters[:-1]), parameters[-1])
    return _ConcatenateGenericAlias(self, parameters)


@_SpecialForm
def TypeGuard(self, parameters):
    """Return type of a user-defined type predicate (PEP 647)."""
    item = _type_check(parameters, f'{self} accepts only single type.')
    return _GenericAlias(self, (item,))


@_SpecialForm
def TypeIs(self, parameters):
    """Return type of a narrowing type predicate (PEP 742)."""
    item = _type_check(parameters, f'{self} accepts only single type.')
    return _GenericAlias(self, (item,))


@_TypeFormForm
def TypeForm(self, parameters):
    """The type of objects that are type expressions (PEP 747)."""
    item = _type_check(parameters, f'{self} accepts only single type.')
    return _GenericAlias(self, (item,))


def _make_forward_ref(code, *, parent_fwdref=None, **kwargs):
    if parent_fwdref is not None:
        if parent_fwdref.__forward_module__ is not None:
            kwargs['module'] = parent_fwdref.__forward_module__
        if parent_fwdref.__owner__ is not None:
            kwargs['owner'] = parent_fwdref.__owner__
        if parent_fwdref.__forward_is_class__:
            kwargs['is_class'] = True
    forward_ref = _lazy_annotationlib.ForwardRef(code, **kwargs)
    # compile eagerly so that syntax errors surface at creation time
    forward_ref.__forward_code__
    return forward_ref


def evaluate_forward_ref(forward_ref, *, owner=None, globals=None, locals=None,
                         type_params=None, format=None, _recursive_guard=frozenset()):
    """Evaluate a ForwardRef, also evaluating forward references nested in the result."""
    if format == _lazy_annotationlib.Format.STRING:
        return forward_ref.evaluate(format=format)
    if forward_ref.__forward_arg__ in _recursive_guard:
        return forward_ref

    if format is None:
        format = _lazy_annotationlib.Format.VALUE
    value = forward_ref.evaluate(globals=globals, locals=locals,
                                 type_params=type_params, owner=owner, format=format)

    if (isinstance(value, _lazy_annotationlib.ForwardRef)
            and format == _lazy_annotationlib.Format.FORWARDREF):
        return value

    if isinstance(value, str):
        value = _make_forward_ref(value, module=forward_ref.__forward_module__,
                                  owner=owner or forward_ref.__owner__,
                                  is_argument=forward_ref.__forward_is_argument__,
                                  is_class=forward_ref.__forward_is_class__)
    if owner is None:
        owner = forward_ref.__owner__
    return _eval_type(
        value, globals, locals, type_params,
        recursive_guard=_recursive_guard | {forward_ref.__forward_arg__},
        format=format, owner=owner, parent_fwdref=forward_ref,
    )


# ---------------------------------------------------------------- substitution hooks
# (called from the _typing objects: TypeVar.__typing_subst__ etc.)

def _typevar_subst(self, arg):
    msg = "Parameters to generic types must be types."
    arg = _type_check(arg, msg, is_argument=True)
    if ((isinstance(arg, _GenericAlias) and arg.__origin__ is Unpack) or
            (isinstance(arg, GenericAlias) and getattr(arg, '__unpacked__', False))):
        raise TypeError(f"{arg} is not valid as type argument")
    return arg


def _typevartuple_prepare_subst(self, alias, args):
    params = alias.__parameters__
    tvt_index = params.index(self)
    for param in params[tvt_index + 1:]:
        if isinstance(param, TypeVarTuple):
            raise TypeError(f"More than one TypeVarTuple parameter in {alias}")

    alen = len(args)
    plen = len(params)
    left = tvt_index
    right = plen - tvt_index - 1
    var_tuple_index = None
    fillarg = None
    for k, arg in enumerate(args):
        if not isinstance(arg, type):
            subargs = getattr(arg, '__typing_unpacked_tuple_args__', None)
            if subargs and len(subargs) == 2 and subargs[-1] is ...:
                if var_tuple_index is not None:
                    raise TypeError("More than one unpacked arbitrary-length tuple argument")
                var_tuple_index = k
                fillarg = subargs[0]
    if var_tuple_index is not None:
        left = min(left, var_tuple_index)
        right = min(right, alen - var_tuple_index - 1)
    elif left + right > alen:
        raise TypeError(f"Too few arguments for {alias};"
                        f" actual {alen}, expected at least {plen - 1}")
    if left == alen - right and self.has_default():
        replacement = _unpack_args(self.__default__)
    else:
        replacement = args[left: alen - right]

    return (
        *args[:left],
        *([fillarg] * (tvt_index - left)),
        replacement,
        *([fillarg] * (plen - right - left - tvt_index - 1)),
        *args[alen - right:],
    )


def _paramspec_subst(self, arg):
    if isinstance(arg, (list, tuple)):
        arg = tuple(_type_check(a, "Expected a type.") for a in arg)
    elif not _is_param_expr(arg):
        raise TypeError(f"Expected a list of types, an ellipsis, "
                        f"ParamSpec, or Concatenate. Got {arg}")
    return arg


def _paramspec_prepare_subst(self, alias, args):
    params = alias.__parameters__
    i = params.index(self)
    if i == len(args) and self.has_default():
        args = [*args, self.__default__]
    if i >= len(args):
        raise TypeError(f"Too few arguments for {alias}")
    # X[int, str] means X[[int, str]] when X has a single ParamSpec parameter
    if len(params) == 1 and not _is_param_expr(args[0]):
        assert i == 0
        args = (args,)
    elif isinstance(args[i], list):
        args = (*args[:i], tuple(args[i]), *args[i + 1:])
    return args


@_tp_cache
def _generic_class_getitem(cls, args):
    """Generic.__class_getitem__ / Protocol.__class_getitem__ and user generics."""
    if not isinstance(args, tuple):
        args = (args,)
    args = tuple(_type_convert(p) for p in args)
    if cls in (Generic, Protocol):
        if not args:
            raise TypeError(f"Parameter list to {cls.__qualname__}[...] cannot be empty")
        if not all(_is_typevar_like(p) for p in args):
            raise TypeError(f"Parameters to {cls.__name__}[...] must all be type variables "
                            f"or parameter specification variables.")
        if len(set(args)) != len(args):
            raise TypeError(f"Parameters to {cls.__name__}[...] must all be unique")
    else:
        try:
            parameters = cls.__parameters__
        except AttributeError as e:
            init_subclass = getattr(cls, '__init_subclass__', None)
            if init_subclass not in {None, Generic.__init_subclass__}:
                e.add_note(
                    f"Note: this exception may have been caused by "
                    f"{init_subclass.__qualname__!r} (or the "
                    f"'__init_subclass__' method on a superclass) not "
                    f"calling 'super().__init_subclass__()'"
                )
            raise
        for param in parameters:
            prepare = getattr(param, '__typing_prepare_subst__', None)
            if prepare is not None:
                args = prepare(cls, args)
        _check_generic_specialization(cls, args)

        new_args = []
        for param, new_arg in zip(parameters, args):
            if isinstance(param, TypeVarTuple):
                new_args.extend(new_arg)
            else:
                new_args.append(new_arg)
        args = tuple(new_args)

    return _GenericAlias(cls, args)


def _generic_init_subclass(cls, *args, **kwargs):
    """Generic.__init_subclass__: computes cls.__parameters__."""
    super(Generic, cls).__init_subclass__(*args, **kwargs)
    tvars = []
    if '__orig_bases__' in cls.__dict__:
        error = Generic in cls.__orig_bases__
    else:
        error = (Generic in cls.__bases__ and
                 cls.__name__ != 'Protocol' and
                 type(cls) != _TypedDictMeta)
    if error:
        raise TypeError("Cannot inherit from plain Generic")
    if '__orig_bases__' in cls.__dict__:
        tvars = _collect_type_parameters(cls.__orig_bases__, validate_all=True)
        # A Generic[...] / Protocol[...] base, if present, must list every type variable.
        gvars = None
        basename = None
        for base in cls.__orig_bases__:
            if isinstance(base, _GenericAlias) and base.__origin__ in (Generic, Protocol):
                the_base = base.__origin__.__name__
                if gvars is not None:
                    raise TypeError(
                        "Cannot inherit from Generic[...] multiple times.")
                gvars = base.__parameters__
                basename = the_base
        if gvars is not None:
            tvarset = set(tvars)
            gvarset = set(gvars)
            if not tvarset <= gvarset:
                s_vars = ', '.join(str(t) for t in tvars if t not in gvarset)
                s_args = ', '.join(str(g) for g in gvars)
                raise TypeError(f"Some type variables ({s_vars}) are"
                                f" not listed in {basename}[{s_args}]")
            tvars = gvars
    cls.__parameters__ = tuple(tvars)


# ---------------------------------------------------------------- generic aliases

def _is_dunder(attr):
    return attr.startswith('__') and attr.endswith('__')


class _BaseGenericAlias(_Final, _root=True):
    """Common base of typing's generic aliases (List, List[int], ...)."""

    def __init__(self, origin, *, inst=True, name=None):
        self._inst = inst
        self._name = name
        self.__origin__ = origin
        self.__slots__ = None  # historical; not documented

    def __call__(self, *args, **kwargs):
        if not self._inst:
            raise TypeError(f"Type {self._name} cannot be instantiated; "
                            f"use {self.__origin__.__name__}() instead")
        result = self.__origin__(*args, **kwargs)
        try:
            result.__orig_class__ = self
        except Exception:
            # objects without settable attributes
            pass
        return result

    def __mro_entries__(self, bases):
        res = []
        if self.__origin__ not in bases:
            res.append(self.__origin__)
        # Add Generic unless a later base already contributes it (directly or
        # through __mro_entries__); it must appear exactly once.
        i = bases.index(self)
        for b in bases[i + 1:]:
            if isinstance(b, _BaseGenericAlias):
                break
            if not isinstance(b, type):
                meth = getattr(b, "__mro_entries__", None)
                new_bases = meth(bases) if meth else None
                if (isinstance(new_bases, tuple) and
                        any(isinstance(b2, type) and issubclass(b2, Generic) for b2 in new_bases)):
                    break
            elif issubclass(b, Generic):
                break
        else:
            res.append(Generic)
        return tuple(res)

    def __getattr__(self, attr):
        if attr in {'__name__', '__qualname__'}:
            return self._name or self.__origin__.__name__
        if attr in {'__origin__', '__args__'}:
            return super().__getattr__(attr)
        # Forward non-dunder attributes to the origin (but be careful during
        # copy/pickle, when __origin__ may not be set yet).
        if '__origin__' in self.__dict__ and not _is_dunder(attr):
            return getattr(self.__origin__, attr)
        raise AttributeError(attr)

    def __setattr__(self, attr, val):
        if _is_dunder(attr) or attr in {'_name', '_inst', '_nparams', '_defaults'}:
            super().__setattr__(attr, val)
        else:
            setattr(self.__origin__, attr, val)

    def __instancecheck__(self, obj):
        return self.__subclasscheck__(type(obj))

    def __subclasscheck__(self, cls):
        raise TypeError("Subscripted generics cannot be used with"
                        " class and instance checks")

    def __dir__(self):
        return list(set(super().__dir__()
                        + [attr for attr in dir(self.__origin__) if not _is_dunder(attr)]))


class _GenericAlias(_BaseGenericAlias, _root=True):
    """A subscripted generic: List[int], Generic[T], ClassVar[int], user classes C[int]."""

    def __init__(self, origin, args, *, inst=True, name=None):
        super().__init__(origin, inst=inst, name=name)
        if not isinstance(args, tuple):
            args = (args,)
        self.__args__ = tuple(... if a is _TypingEllipsis else a for a in args)
        enforce_default_ordering = origin in (Generic, Protocol)
        self.__parameters__ = _collect_type_parameters(
            args, enforce_default_ordering=enforce_default_ordering)
        if not name:
            self.__module__ = origin.__module__

    def __eq__(self, other):
        if not isinstance(other, _GenericAlias):
            return NotImplemented
        return (self.__origin__ == other.__origin__
                and self.__args__ == other.__args__)

    def __hash__(self):
        return hash((self.__origin__, self.__args__))

    def __or__(self, right):
        return Union[self, right]

    def __ror__(self, left):
        return Union[left, self]

    @_tp_cache
    def __getitem__(self, args):
        # substitute the type parameters of an already parameterized alias
        if self.__origin__ in (Generic, Protocol):
            raise TypeError(f"Cannot subscript already-subscripted {self}")
        if not self.__parameters__:
            raise TypeError(f"{self} is not a generic class")
        if not isinstance(args, tuple):
            args = (args,)
        args = _unpack_args(*(_type_convert(p) for p in args))
        new_args = self._determine_new_args(args)
        return self.copy_with(new_args)

    def _determine_new_args(self, args):
        params = self.__parameters__
        for param in params:
            prepare = getattr(param, '__typing_prepare_subst__', None)
            if prepare is not None:
                args = prepare(self, args)
        alen = len(args)
        plen = len(params)
        if alen != plen:
            raise TypeError(f"Too {'many' if alen > plen else 'few'} arguments for {self};"
                            f" actual {alen}, expected {plen}")
        new_arg_by_param = dict(zip(params, args))
        return tuple(self._make_substitution(self.__args__, new_arg_by_param))

    def _make_substitution(self, args, new_arg_by_param):
        new_args = []
        for old_arg in args:
            if isinstance(old_arg, type):
                new_args.append(old_arg)
                continue

            substfunc = getattr(old_arg, '__typing_subst__', None)
            if substfunc:
                new_arg = substfunc(new_arg_by_param[old_arg])
            else:
                subparams = getattr(old_arg, '__parameters__', ())
                if not subparams:
                    new_arg = old_arg
                else:
                    subargs = []
                    for x in subparams:
                        if isinstance(x, TypeVarTuple):
                            subargs.extend(new_arg_by_param[x])
                        else:
                            subargs.append(new_arg_by_param[x])
                    new_arg = old_arg[tuple(subargs)]

            if self.__origin__ == collections.abc.Callable and isinstance(new_arg, tuple):
                # Callable args stay flat: Callable[P, T][[int, str], float] -> (int, str, float)
                new_args.extend(new_arg)
            elif _is_unpacked_typevartuple(old_arg):
                # *Ts substituted by a tuple of types: splice them in
                new_args.extend(new_arg)
            elif isinstance(old_arg, tuple):
                # a ParamSpec substituted with a list containing type variables
                new_args.append(tuple(self._make_substitution(old_arg, new_arg_by_param)))
            else:
                new_args.append(new_arg)
        return new_args

    def copy_with(self, args):
        return self.__class__(self.__origin__, args, name=self._name, inst=self._inst)

    def __repr__(self):
        if self._name:
            name = 'typing.' + self._name
        else:
            name = _type_repr(self.__origin__)
        if self.__args__:
            args = ", ".join([_type_repr(a) for a in self.__args__])
        else:
            args = "()"
        return f'{name}[{args}]'

    def __reduce__(self):
        if self._name:
            origin = globals()[self._name]
        else:
            origin = self.__origin__
        args = tuple(self.__args__)
        if len(args) == 1 and not isinstance(args[0], tuple):
            args, = args
        return operator.getitem, (origin, args)

    def __mro_entries__(self, bases):
        if isinstance(self.__origin__, _SpecialForm):
            raise TypeError(f"Cannot subclass {self!r}")

        if self._name:  # generic version of an ABC or builtin class
            return super().__mro_entries__(bases)
        if self.__origin__ is Generic:
            if Protocol in bases:
                return ()
            i = bases.index(self)
            for b in bases[i + 1:]:
                if isinstance(b, _BaseGenericAlias) and b is not self:
                    return ()
        return (self.__origin__,)

    def __iter__(self):
        yield Unpack[self]


class _SpecialGenericAlias(_NotIterable, _BaseGenericAlias, _root=True):
    """An unsubscripted alias such as List or Dict (deprecated aliases)."""

    def __init__(self, origin, nparams, *, inst=True, name=None, defaults=()):
        if name is None:
            name = origin.__name__
        super().__init__(origin, inst=inst, name=name)
        self._nparams = nparams
        self._defaults = defaults
        if origin.__module__ == 'builtins':
            self.__doc__ = f'Deprecated alias to {origin.__qualname__}.'
        else:
            self.__doc__ = f'Deprecated alias to {origin.__module__}.{origin.__qualname__}.'

    @_tp_cache
    def __getitem__(self, params):
        if not isinstance(params, tuple):
            params = (params,)
        msg = "Parameters to generic types must be types."
        params = tuple(_type_check(p, msg) for p in params)
        if (self._defaults
                and len(params) < self._nparams
                and len(params) + len(self._defaults) >= self._nparams):
            params = (*params, *self._defaults[len(params) - (self._nparams - len(self._defaults)):])
        actual_len = len(params)

        if actual_len != self._nparams:
            if self._defaults:
                expected = f"at least {self._nparams - len(self._defaults)}"
            else:
                expected = str(self._nparams)
            if not self._nparams:
                raise TypeError(f"{self} is not a generic class")
            raise TypeError(f"Too {'many' if actual_len > self._nparams else 'few'} arguments for {self};"
                            f" actual {actual_len}, expected {expected}")
        return self.copy_with(params)

    def copy_with(self, params):
        return _GenericAlias(self.__origin__, params, name=self._name, inst=self._inst)

    def __repr__(self):
        return 'typing.' + self._name

    def __subclasscheck__(self, cls):
        if isinstance(cls, _SpecialGenericAlias):
            return issubclass(cls.__origin__, self.__origin__)
        if not isinstance(cls, _GenericAlias):
            return issubclass(cls, self.__origin__)
        return super().__subclasscheck__(cls)

    def __reduce__(self):
        return self._name

    def __or__(self, right):
        return Union[self, right]

    def __ror__(self, left):
        return Union[left, self]


class _DeprecatedGenericAlias(_SpecialGenericAlias, _root=True):
    def __init__(self, origin, nparams, *, removal_version, inst=True, name=None):
        super().__init__(origin, nparams, inst=inst, name=name)
        self._removal_version = removal_version

    def __instancecheck__(self, inst):
        import warnings
        warnings._deprecated(f"{self.__module__}.{self._name}", remove=self._removal_version)
        return super().__instancecheck__(inst)


class _CallableGenericAlias(_NotIterable, _GenericAlias, _root=True):
    def __repr__(self):
        assert self._name == 'Callable'
        args = self.__args__
        if len(args) == 2 and _is_param_expr(args[0]):
            return super().__repr__()
        return (f'typing.Callable'
                f'[[{", ".join([_type_repr(a) for a in args[:-1]])}], '
                f'{_type_repr(args[-1])}]')

    def __reduce__(self):
        args = self.__args__
        if not (len(args) == 2 and _is_param_expr(args[0])):
            args = list(args[:-1]), args[-1]
        return operator.getitem, (Callable, args)


class _CallableType(_SpecialGenericAlias, _root=True):
    def copy_with(self, params):
        return _CallableGenericAlias(self.__origin__, params, name=self._name, inst=self._inst)

    def __getitem__(self, params):
        if not isinstance(params, tuple) or len(params) != 2:
            raise TypeError("Callable must be used as "
                            "Callable[[arg, ...], result].")
        args, result = params
        # Anything is accepted as the argument part (lists, ..., ParamSpec,
        # Concatenate); checking it is the job of static type checkers.
        if isinstance(args, list):
            params = (tuple(args), result)
        else:
            params = (args, result)
        return self.__getitem_inner__(params)

    @_tp_cache
    def __getitem_inner__(self, params):
        args, result = params
        msg = "Callable[args, result]: result must be a type."
        result = _type_check(result, msg)
        if args is Ellipsis:
            return self.copy_with((_TypingEllipsis, result))
        if not isinstance(args, tuple):
            args = (args,)
        args = tuple(_type_convert(arg) for arg in args)
        params = args + (result,)
        return self.copy_with(params)


class _TupleType(_SpecialGenericAlias, _root=True):
    @_tp_cache
    def __getitem__(self, params):
        if not isinstance(params, tuple):
            params = (params,)
        if len(params) >= 2 and params[-1] is ...:
            msg = "Tuple[t, ...]: t must be a type."
            params = tuple(_type_check(p, msg) for p in params[:-1])
            return self.copy_with((*params, _TypingEllipsis))
        msg = "Tuple[t0, t1, ...]: each t must be a type."
        params = tuple(_type_check(p, msg) for p in params)
        return self.copy_with(params)


class _UnionGenericAliasMeta(type):
    def __instancecheck__(self, inst: object) -> bool:
        import warnings
        warnings._deprecated("_UnionGenericAlias", remove=(3, 17))
        return isinstance(inst, Union)

    def __subclasscheck__(self, inst: type) -> bool:
        import warnings
        warnings._deprecated("_UnionGenericAlias", remove=(3, 17))
        return issubclass(inst, Union)

    def __eq__(self, other):
        import warnings
        warnings._deprecated("_UnionGenericAlias", remove=(3, 17))
        if other is _UnionGenericAlias or other is Union:
            return True
        return NotImplemented

    def __hash__(self):
        return hash(Union)


class _UnionGenericAlias(metaclass=_UnionGenericAliasMeta):
    """Backwards-compatibility shim: unions are types.UnionType (typing.Union) now."""
    def __new__(cls, self_cls, parameters, /, *, name=None):
        import warnings
        warnings._deprecated("_UnionGenericAlias", remove=(3, 17))
        return Union[parameters]


def _value_and_type_iter(parameters):
    return ((p, type(p)) for p in parameters)


class _LiteralGenericAlias(_GenericAlias, _root=True):
    def __eq__(self, other):
        if not isinstance(other, _LiteralGenericAlias):
            return NotImplemented
        return set(_value_and_type_iter(self.__args__)) == set(_value_and_type_iter(other.__args__))

    def __hash__(self):
        return hash(frozenset(_value_and_type_iter(self.__args__)))


class _ConcatenateGenericAlias(_GenericAlias, _root=True):
    def copy_with(self, params):
        if isinstance(params[-1], _ConcatenateGenericAlias):
            params = (*params[:-1], *params[-1].__args__)
        elif isinstance(params[-1], (list, tuple)):
            return (*params[:-1], *params[-1])
        elif (not (params[-1] is ... or isinstance(params[-1], ParamSpec))):
            raise TypeError("The last parameter to Concatenate should be a "
                            "ParamSpec variable or ellipsis.")
        return self.__class__(self.__origin__, params, name=self._name, inst=self._inst)


@_SpecialForm
def Unpack(self, parameters):
    """Unpacks a TypeVarTuple or tuple type: Unpack[Ts] is the same as *Ts."""
    item = _type_check(parameters, f'{self} accepts only single type.')
    return _UnpackGenericAlias(origin=self, args=(item,))


class _UnpackGenericAlias(_GenericAlias, _root=True):
    def __repr__(self):
        return f'typing.Unpack[{_type_repr(self.__args__[0])}]'

    def __getitem__(self, args):
        if self.__typing_is_unpacked_typevartuple__:
            return args
        return super().__getitem__(args)

    @property
    def __typing_unpacked_tuple_args__(self):
        assert self.__origin__ is Unpack
        assert len(self.__args__) == 1
        arg, = self.__args__
        if isinstance(arg, (_GenericAlias, GenericAlias)):
            if arg.__origin__ is not tuple:
                raise TypeError("Unpack[...] must be used with a tuple type")
            return arg.__args__
        return None

    @property
    def __typing_is_unpacked_typevartuple__(self):
        assert self.__origin__ is Unpack
        assert len(self.__args__) == 1
        return isinstance(self.__args__[0], TypeVarTuple)


class _TypingEllipsis:
    """Internal placeholder for ... in Tuple[int, ...] / Callable[..., T]."""


# ---------------------------------------------------------------- Protocol

_TYPING_INTERNALS = frozenset({
    '__parameters__', '__orig_bases__', '__orig_class__',
    '_is_protocol', '_is_runtime_protocol', '__protocol_attrs__',
    '__non_callable_proto_members__', '__type_params__',
})

_SPECIAL_NAMES = frozenset({
    '__abstractmethods__', '__annotations__', '__dict__', '__doc__',
    '__init__', '__module__', '__new__', '__slots__',
    '__subclasshook__', '__weakref__', '__class_getitem__',
    '__match_args__', '__static_attributes__', '__firstlineno__',
    '__annotate__', '__annotate_func__', '__annotations_cache__',
})

# attributes never collected as protocol members
EXCLUDED_ATTRIBUTES = frozenset([*_TYPING_INTERNALS, *_SPECIAL_NAMES, '_MutableMapping__marker'])


def _class_annotation_names(base):
    d = base.__dict__
    if '__annotations__' in d and isinstance(d['__annotations__'], dict):
        return d['__annotations__']
    if '__annotate__' in d or '__annotate_func__' in d:
        return _lazy_annotationlib.get_annotations(
            base, format=_lazy_annotationlib.Format.FORWARDREF)
    return {}


def _get_protocol_attrs(cls):
    """The protocol members of a protocol class (excluding object's)."""
    attrs = set()
    for base in cls.__mro__[:-1]:  # skip object
        if base.__name__ in {'Protocol', 'Generic'}:
            continue
        annotations = _class_annotation_names(base)
        for attr in (*base.__dict__, *annotations):
            if not attr.startswith('_abc_') and attr not in EXCLUDED_ATTRIBUTES:
                attrs.add(attr)
    return attrs


def _no_init_or_replace_init(self, *args, **kwargs):
    cls = type(self)

    if cls._is_protocol:
        raise TypeError('Protocols cannot be instantiated')

    # A custom __init__ is already installed (avoid infinite recursion).
    if cls.__init__ is not _no_init_or_replace_init:
        return

    # First instantiation of a concrete subclass of a protocol: find the real
    # __init__ along the MRO and install it on the class for later calls.
    for base in cls.__mro__:
        init = base.__dict__.get('__init__', _no_init_or_replace_init)
        if init is not _no_init_or_replace_init:
            cls.__init__ = init
            break
    else:
        cls.__init__ = object.__init__

    cls.__init__(self, *args, **kwargs)


def _caller(depth=1, default='__main__'):
    try:
        return sys._getframemodulename(depth + 1) or default
    except AttributeError:
        pass
    try:
        return sys._getframe(depth + 1).f_globals.get('__name__', default)
    except (AttributeError, ValueError):
        pass
    return None


def _allow_reckless_class_checks(depth=2):
    """abc and functools check every class of an MRO; let them pass for protocols."""
    return _caller(depth) in {'abc', '_py_abc', 'functools', None}


_PROTO_ALLOWLIST = {
    'collections.abc': [
        'Callable', 'Awaitable', 'Iterable', 'Iterator', 'AsyncIterable',
        'AsyncIterator', 'Hashable', 'Sized', 'Container', 'Collection',
        'Reversible', 'Buffer',
    ],
    'contextlib': ['AbstractContextManager', 'AbstractAsyncContextManager'],
    'io': ['Reader', 'Writer'],
    'os': ['PathLike'],
}


@functools.cache
def _lazy_load_getattr_static():
    from inspect import getattr_static
    return getattr_static


_cleanups.append(_lazy_load_getattr_static.cache_clear)


def _pickle_psargs(psargs):
    return ParamSpecArgs, (psargs.__origin__,)

copyreg.pickle(ParamSpecArgs, _pickle_psargs)


def _pickle_pskwargs(pskwargs):
    return ParamSpecKwargs, (pskwargs.__origin__,)

copyreg.pickle(ParamSpecKwargs, _pickle_pskwargs)

del _pickle_psargs, _pickle_pskwargs


_abc_instancecheck = ABCMeta.__instancecheck__
_abc_subclasscheck = ABCMeta.__subclasscheck__


def _type_check_issubclass_arg_1(arg):
    if not isinstance(arg, type):
        # same message as issubclass(1, int)
        raise TypeError('issubclass() arg 1 must be a class')


class _ProtocolMeta(ABCMeta):
    def __new__(mcls, name, bases, namespace, /, **kwargs):
        if name == "Protocol" and bases == (Generic,):
            pass
        elif Protocol in bases:
            for base in bases:
                if not (
                    base in {object, Generic}
                    or base.__name__ in _PROTO_ALLOWLIST.get(base.__module__, [])
                    or (
                        issubclass(base, Generic)
                        and getattr(base, "_is_protocol", False)
                    )
                ):
                    raise TypeError(
                        f"Protocols can only inherit from other protocols, "
                        f"got {base!r}"
                    )
        return super().__new__(mcls, name, bases, namespace, **kwargs)

    def __init__(cls, *args, **kwargs):
        super().__init__(*args, **kwargs)
        if getattr(cls, "_is_protocol", False):
            cls.__protocol_attrs__ = _get_protocol_attrs(cls)

    def __subclasscheck__(cls, other):
        if cls is Protocol:
            return type.__subclasscheck__(cls, other)
        if (
            getattr(cls, '_is_protocol', False)
            and not _allow_reckless_class_checks()
        ):
            if not getattr(cls, '_is_runtime_protocol', False):
                _type_check_issubclass_arg_1(other)
                raise TypeError(
                    "Instance and class checks can only be used with "
                    "@runtime_checkable protocols"
                )
            _warn_if_runtime_checkable_inherited(cls)
            if (
                cls.__non_callable_proto_members__
                and cls.__dict__.get("__subclasshook__") is _proto_hook
            ):
                _type_check_issubclass_arg_1(other)
                non_method_attrs = sorted(cls.__non_callable_proto_members__)
                raise TypeError(
                    "Protocols with non-method members don't support issubclass()."
                    f" Non-method members: {str(non_method_attrs)[1:-1]}."
                )
        return _abc_subclasscheck(cls, other)

    def __instancecheck__(cls, instance):
        if cls is Protocol:
            return type.__instancecheck__(cls, instance)
        if not getattr(cls, "_is_protocol", False):
            # a concrete subclass of a protocol
            return _abc_instancecheck(cls, instance)

        if (
            not getattr(cls, '_is_runtime_protocol', False) and
            not _allow_reckless_class_checks()
        ):
            raise TypeError("Instance and class checks can only be used with"
                            " @runtime_checkable protocols")
        if not _allow_reckless_class_checks():
            _warn_if_runtime_checkable_inherited(cls)

        if _abc_instancecheck(cls, instance):
            return True

        getattr_static = _lazy_load_getattr_static()
        for attr in cls.__protocol_attrs__:
            try:
                val = getattr_static(instance, attr)
            except AttributeError:
                break
            # a member set to None explicitly opts out (only for methods)
            if val is None and attr not in cls.__non_callable_proto_members__:
                break
        else:
            return True

        return False


def _warn_if_runtime_checkable_inherited(cls):
    # A protocol is only runtime-checkable if @runtime_checkable was applied
    # to it; inheriting the flag from a base protocol is deprecated.
    if (cls.__dict__.get('_is_runtime_protocol') is None
            and getattr(cls, '_is_runtime_protocol', False)):
        import warnings
        warnings.warn(
            f"{cls!r} isn't explicitly decorated with @runtime_checkable but "
            "it is used in issubclass() or isinstance(). Instance and class "
            "checks can only be used with @runtime_checkable protocols. "
            "This will raise a TypeError in Python 3.20.",
            DeprecationWarning, stacklevel=3)


@classmethod
def _proto_hook(cls, other):
    if not cls.__dict__.get('_is_protocol', False):
        return NotImplemented

    for attr in cls.__protocol_attrs__:
        for base in other.__mro__:
            # defined in a class namespace?
            if attr in base.__dict__:
                if base.__dict__[attr] is None:
                    return NotImplemented
                break

            # or annotated in a sub-protocol?
            if issubclass(other, Generic) and getattr(other, "_is_protocol", False):
                try:
                    annos = base.__annotations__
                except Exception:
                    annos = _lazy_annotationlib.get_annotations(
                        base, format=_lazy_annotationlib.Format.FORWARDREF)
                if attr in annos:
                    break
        else:
            return NotImplemented
    return True


class Protocol(Generic, metaclass=_ProtocolMeta):
    """Base class for structural (duck-typed) protocol classes (PEP 544).

    Classes deriving from Protocol directly are protocols; with
    @runtime_checkable they also support isinstance()/issubclass() checks
    that only look for the presence of the protocol members.
    """

    __slots__ = ()
    _is_protocol = True
    _is_runtime_protocol = False

    def __init_subclass__(cls, *args, **kwargs):
        super().__init_subclass__(*args, **kwargs)

        # protocol or concrete implementation?
        if not cls.__dict__.get('_is_protocol', False):
            cls._is_protocol = any(b is Protocol for b in cls.__bases__)

        if '__subclasshook__' not in cls.__dict__:
            cls.__subclasshook__ = _proto_hook

        # protocols themselves cannot be instantiated
        if cls._is_protocol and cls.__init__ is Protocol.__init__:
            cls.__init__ = _no_init_or_replace_init


# ---------------------------------------------------------------- Annotated

class _AnnotatedAlias(_NotIterable, _GenericAlias, _root=True):
    """Annotated[T, *metadata]; nested Annotated forms are flattened."""

    def __init__(self, origin, metadata):
        if isinstance(origin, _AnnotatedAlias):
            metadata = origin.__metadata__ + metadata
            origin = origin.__origin__
        super().__init__(origin, origin, name='Annotated')
        self.__metadata__ = metadata

    def copy_with(self, params):
        assert len(params) == 1
        new_type = params[0]
        return _AnnotatedAlias(new_type, self.__metadata__)

    def __repr__(self):
        return "typing.Annotated[{}, {}]".format(
            _type_repr(self.__origin__),
            ", ".join(repr(a) for a in self.__metadata__)
        )

    def __reduce__(self):
        return operator.getitem, (Annotated, (self.__origin__,) + self.__metadata__)

    def __eq__(self, other):
        if not isinstance(other, _AnnotatedAlias):
            return NotImplemented
        return (self.__origin__ == other.__origin__
                and self.__metadata__ == other.__metadata__)

    def __hash__(self):
        return hash((self.__origin__, self.__metadata__))

    def __getattr__(self, attr):
        if attr in {'__name__', '__qualname__'}:
            return 'Annotated'
        return super().__getattr__(attr)

    def __mro_entries__(self, bases):
        return (self.__origin__,)


@_TypedCacheSpecialForm
@_tp_cache(typed=True)
def Annotated(self, *params):
    """Adds context-specific metadata to a type: Annotated[int, ValueRange(0, 9)]."""
    if len(params) < 2:
        raise TypeError("Annotated[...] should be used "
                        "with at least two arguments (a type and an "
                        "annotation).")
    if _is_unpacked_typevartuple(params[0]):
        raise TypeError("Annotated[...] should not be used with an "
                        "unpacked TypeVarTuple")
    msg = "Annotated[t, ...]: t must be a type."
    origin = _type_check(params[0], msg, allow_special_forms=True)
    metadata = tuple(params[1:])
    return _AnnotatedAlias(origin, metadata)


def runtime_checkable(cls):
    """Mark a protocol class as usable with isinstance() and issubclass()."""
    if not issubclass(cls, Generic) or not getattr(cls, '_is_protocol', False):
        raise TypeError('@runtime_checkable can be only applied to protocol classes,'
                        ' got %r' % cls)
    cls._is_runtime_protocol = True
    # issubclass() is not supported for protocols with data members; record them.
    cls.__non_callable_proto_members__ = set()
    for attr in cls.__protocol_attrs__:
        try:
            is_callable = callable(getattr(cls, attr, None))
        except Exception as e:
            raise TypeError(
                f"Failed to determine whether protocol member {attr!r} "
                "is a method member"
            ) from e
        else:
            if not is_callable:
                cls.__non_callable_proto_members__.add(attr)
    return cls


def cast(typ, val):
    """Tell type checkers that val has type typ; returns val unchanged."""
    return val


def assert_type(val, typ, /):
    """Ask a type checker to verify the inferred type of val; returns val."""
    return val


# ---------------------------------------------------------------- get_type_hints & co

def get_type_hints(obj, globalns=None, localns=None, include_extras=False,
                   *, format=None):
    """Return the evaluated type hints of a module, class, method or function.

    Forward references (strings) are evaluated in globalns/localns (defaulting
    to the object's namespaces). For classes the hints of all classes of the
    MRO are merged. Annotated, Required, NotRequired and ReadOnly wrappers are
    stripped unless include_extras is true.
    """
    if getattr(obj, '__no_type_check__', None):
        return {}
    Format = _lazy_annotationlib.Format
    if format is None:
        format = Format.VALUE
    if isinstance(obj, type):
        hints = {}
        for base in reversed(obj.__mro__):
            ann = _lazy_annotationlib.get_annotations(base, format=format)
            if format == Format.STRING:
                hints.update(ann)
                continue
            if globalns is None:
                base_globals = getattr(sys.modules.get(base.__module__, None), '__dict__', {})
            else:
                base_globals = globalns
            base_locals = dict(vars(base)) if localns is None else localns
            if localns is None and globalns is None:
                # Historical behaviour: names are looked up in the class
                # namespace first, then in the module.
                base_globals, base_locals = base_locals, base_globals
            type_params = base.__type_params__
            base_globals, base_locals = _add_type_params_to_scope(
                type_params, base_globals, base_locals, True)
            for name, value in ann.items():
                if isinstance(value, str):
                    value = _make_forward_ref(value, is_argument=False, is_class=True)
                # A ForwardRef bound to a module is evaluated in that module's
                # namespace instead of the scope built above, so it needs the
                # type parameters passed explicitly.
                bound_to_module = (isinstance(value, _lazy_annotationlib.ForwardRef)
                                   and value.__forward_module__ is not None)
                value = _eval_type(value, base_globals, base_locals,
                                   type_params if bound_to_module else (),
                                   format=format, owner=obj)
                if value is None:
                    value = type(None)
                hints[name] = value
        if include_extras or format == Format.STRING:
            return hints
        return {k: _strip_annotations(t) for k, t in hints.items()}

    hints = _lazy_annotationlib.get_annotations(obj, format=format)
    if (
        not hints
        and not isinstance(obj, types.ModuleType)
        and not callable(obj)
        and not hasattr(obj, '__annotations__')
        and not hasattr(obj, '__annotate__')
    ):
        raise TypeError(f"{obj!r} is not a module, class, or callable.")
    if format == Format.STRING:
        return hints

    if globalns is None:
        if isinstance(obj, types.ModuleType):
            globalns = obj.__dict__
        else:
            nsobj = obj
            visited = {id(nsobj)}
            while hasattr(nsobj, '__wrapped__'):
                nsobj = nsobj.__wrapped__
                if id(nsobj) in visited:
                    raise ValueError(f'wrapper loop when unwrapping {obj!r}')
                visited.add(id(nsobj))
            globalns = getattr(nsobj, '__globals__', {})
        if localns is None:
            localns = globalns
    elif localns is None:
        localns = globalns
    type_params = getattr(obj, "__type_params__", ())
    globalns, localns = _add_type_params_to_scope(type_params, globalns, localns, False)
    for name, value in hints.items():
        if isinstance(value, str):
            value = _make_forward_ref(
                value,
                is_argument=not isinstance(obj, types.ModuleType),
                is_class=False,
            )
        value = _eval_type(value, globalns, localns, (), format=format, owner=obj)
        if value is None:
            value = type(None)
        hints[name] = value
    return hints if include_extras else {k: _strip_annotations(t) for k, t in hints.items()}


def _add_type_params_to_scope(type_params, globalns, localns, is_class):
    if not type_params:
        return globalns, localns
    globalns = dict(globalns)
    localns = dict(localns)
    for param in type_params:
        if not is_class or param.__name__ not in globalns:
            globalns[param.__name__] = param
            localns.pop(param.__name__, None)
    return globalns, localns


def _strip_annotations(t):
    """Remove Annotated/Required/NotRequired/ReadOnly wrappers (recursively)."""
    if isinstance(t, _AnnotatedAlias):
        return _strip_annotations(t.__origin__)
    if hasattr(t, "__origin__") and t.__origin__ in (Required, NotRequired, ReadOnly):
        return _strip_annotations(t.__args__[0])
    if isinstance(t, _GenericAlias):
        stripped_args = tuple(_strip_annotations(a) for a in t.__args__)
        if stripped_args == t.__args__:
            return t
        return t.copy_with(stripped_args)
    if isinstance(t, GenericAlias):
        stripped_args = tuple(_strip_annotations(a) for a in t.__args__)
        if stripped_args == t.__args__:
            return t
        new = type(t)(t.__origin__, stripped_args)
        return next(iter(new)) if t.__unpacked__ else new
    if isinstance(t, Union):
        stripped_args = tuple(_strip_annotations(a) for a in t.__args__)
        if stripped_args == t.__args__:
            return t
        return functools.reduce(operator.or_, stripped_args)
    return t


def get_origin(tp):
    """The unsubscripted form of a generic type (List[int] -> list), else None."""
    if isinstance(tp, _AnnotatedAlias):
        return Annotated
    if isinstance(tp, (_BaseGenericAlias, GenericAlias, ParamSpecArgs, ParamSpecKwargs)):
        return tp.__origin__
    if tp is Generic:
        return Generic
    if isinstance(tp, Union):
        return Union
    return None


def get_args(tp):
    """The type arguments of a subscripted type (Dict[int, str] -> (int, str))."""
    if isinstance(tp, _AnnotatedAlias):
        return (tp.__origin__,) + tp.__metadata__
    if isinstance(tp, (_GenericAlias, GenericAlias)):
        res = tp.__args__
        if _should_unflatten_callable_args(tp, res):
            res = (list(res[:-1]), res[-1])
        return res
    if isinstance(tp, Union):
        return tp.__args__
    return ()


def is_typeddict(tp):
    """True for classes created with TypedDict."""
    return isinstance(tp, _TypedDictMeta)


_ASSERT_NEVER_REPR_MAX_LENGTH = 100


def assert_never(arg: Never, /) -> Never:
    """Statically assert that a line is unreachable; raises at runtime."""
    value = repr(arg)
    if len(value) > _ASSERT_NEVER_REPR_MAX_LENGTH:
        value = value[:_ASSERT_NEVER_REPR_MAX_LENGTH] + '...'
    raise AssertionError(f"Expected code to be unreachable, but got: {value}")


def no_type_check(arg):
    """Decorator: annotations of the decorated function/class are not type hints."""
    if isinstance(arg, type):
        for key in dir(arg):
            obj = getattr(arg, key)
            if (
                not hasattr(obj, '__qualname__')
                or obj.__qualname__ != f'{arg.__qualname__}.{obj.__name__}'
                or getattr(obj, '__module__', None) != arg.__module__
            ):
                # only objects defined directly in this class
                continue
            if isinstance(obj, types.FunctionType):
                obj.__no_type_check__ = True
            if isinstance(obj, types.MethodType):
                obj.__func__.__no_type_check__ = True
            if isinstance(obj, type):
                no_type_check(obj)
    try:
        arg.__no_type_check__ = True
    except TypeError:  # builtin classes
        pass
    return arg


def _overload_dummy(*args, **kwds):
    raise NotImplementedError(
        "You should not call an overloaded function. "
        "A series of @overload-decorated functions "
        "outside a stub module should always be followed "
        "by an implementation that is not @overload-ed.")


# {module: {qualname: {firstlineno: func}}}
_overload_registry = defaultdict(functools.partial(defaultdict, dict))


def overload(func):
    """Decorator for overloaded signatures; the implementation follows undecorated."""
    f = getattr(func, "__func__", func)  # classmethod / staticmethod
    try:
        _overload_registry[f.__module__][f.__qualname__][f.__code__.co_firstlineno] = func
    except AttributeError:
        # not a Python function
        pass
    return _overload_dummy


def get_overloads(func):
    """The overloads registered for func (a list, possibly empty)."""
    f = getattr(func, "__func__", func)
    if f.__module__ not in _overload_registry:
        return []
    mod_dict = _overload_registry[f.__module__]
    if f.__qualname__ not in mod_dict:
        return []
    return list(mod_dict[f.__qualname__].values())


def clear_overloads():
    """Forget all registered overloads."""
    _overload_registry.clear()


def final(f):
    """Decorator marking a method or class as final (sets __final__ when possible)."""
    try:
        f.__final__ = True
    except (AttributeError, TypeError):
        # read-only attribute or builtin type
        pass
    return f


# Some unconstrained type variables, used by the container aliases.
T = TypeVar('T')
KT = TypeVar('KT')
VT = TypeVar('VT')
T_co = TypeVar('T_co', covariant=True)
V_co = TypeVar('V_co', covariant=True)
VT_co = TypeVar('VT_co', covariant=True)
T_contra = TypeVar('T_contra', contravariant=True)
CT_co = TypeVar('CT_co', covariant=True, bound=type)

# A constrained type variable for str/bytes functions.
AnyStr = TypeVar('AnyStr', bytes, str)


# ---------------------------------------------------------------- deprecated aliases

_alias = _SpecialGenericAlias

Hashable = _alias(collections.abc.Hashable, 0)
Awaitable = _alias(collections.abc.Awaitable, 1)
Coroutine = _alias(collections.abc.Coroutine, 3)
AsyncIterable = _alias(collections.abc.AsyncIterable, 1)
AsyncIterator = _alias(collections.abc.AsyncIterator, 1)
Iterable = _alias(collections.abc.Iterable, 1)
Iterator = _alias(collections.abc.Iterator, 1)
Reversible = _alias(collections.abc.Reversible, 1)
Sized = _alias(collections.abc.Sized, 0)
Container = _alias(collections.abc.Container, 1)
Collection = _alias(collections.abc.Collection, 1)
Callable = _CallableType(collections.abc.Callable, 2)
Callable.__doc__ = """Deprecated alias to collections.abc.Callable.

    Callable[[int], str] is a function taking an int and returning a str;
    Callable[..., str] accepts any arguments.
    """
AbstractSet = _alias(collections.abc.Set, 1, name='AbstractSet')
MutableSet = _alias(collections.abc.MutableSet, 1)
Mapping = _alias(collections.abc.Mapping, 2)
MutableMapping = _alias(collections.abc.MutableMapping, 2)
Sequence = _alias(collections.abc.Sequence, 1)
MutableSequence = _alias(collections.abc.MutableSequence, 1)
Tuple = _TupleType(tuple, -1, inst=False, name='Tuple')
Tuple.__doc__ = """Deprecated alias to builtins.tuple.

    Tuple[X, Y] is a pair; Tuple[int, ...] is a variable-length tuple of ints.
    """
List = _alias(list, 1, inst=False, name='List')
Deque = _alias(collections.deque, 1, name='Deque')
Set = _alias(set, 1, inst=False, name='Set')
FrozenSet = _alias(frozenset, 1, inst=False, name='FrozenSet')
MappingView = _alias(collections.abc.MappingView, 1)
KeysView = _alias(collections.abc.KeysView, 1)
ItemsView = _alias(collections.abc.ItemsView, 2)
ValuesView = _alias(collections.abc.ValuesView, 1)
Dict = _alias(dict, 2, inst=False, name='Dict')
DefaultDict = _alias(collections.defaultdict, 2, name='DefaultDict')
OrderedDict = _alias(collections.OrderedDict, 2)
Counter = _alias(collections.Counter, 1)
ChainMap = _alias(collections.ChainMap, 2)
Generator = _alias(collections.abc.Generator, 3, defaults=(types.NoneType, types.NoneType))
AsyncGenerator = _alias(collections.abc.AsyncGenerator, 2, defaults=(types.NoneType,))
Type = _alias(type, 1, inst=False, name='Type')
Type.__doc__ = """Deprecated alias to builtins.type.

    Type[C] is the class C itself or one of its subclasses (not an instance).
    """


# ---------------------------------------------------------------- protocols

@runtime_checkable
class SupportsInt(Protocol):
    """An ABC with one abstract method __int__."""
    __slots__ = ()

    @abstractmethod
    def __int__(self) -> int:
        pass


@runtime_checkable
class SupportsFloat(Protocol):
    """An ABC with one abstract method __float__."""
    __slots__ = ()

    @abstractmethod
    def __float__(self) -> float:
        pass


@runtime_checkable
class SupportsComplex(Protocol):
    """An ABC with one abstract method __complex__."""
    __slots__ = ()

    @abstractmethod
    def __complex__(self) -> complex:
        pass


@runtime_checkable
class SupportsBytes(Protocol):
    """An ABC with one abstract method __bytes__."""
    __slots__ = ()

    @abstractmethod
    def __bytes__(self) -> bytes:
        pass


@runtime_checkable
class SupportsIndex(Protocol):
    """An ABC with one abstract method __index__."""
    __slots__ = ()

    @abstractmethod
    def __index__(self) -> int:
        pass


@runtime_checkable
class SupportsAbs[T](Protocol):
    """An ABC with one abstract method __abs__ that is covariant in its return type."""
    __slots__ = ()

    @abstractmethod
    def __abs__(self) -> T:
        pass


@runtime_checkable
class SupportsRound[T](Protocol):
    """An ABC with one abstract method __round__ that is covariant in its return type."""
    __slots__ = ()

    @abstractmethod
    def __round__(self, ndigits: int = 0) -> T:
        pass


# ---------------------------------------------------------------- NamedTuple

def _make_nmtuple(name, fields, annotate_func, module, defaults=()):
    nm_tpl = collections.namedtuple(name, fields, defaults=defaults, module=module)
    nm_tpl.__annotate__ = nm_tpl.__new__.__annotate__ = annotate_func
    return nm_tpl


def _make_eager_annotate(types):
    checked_types = {key: _type_check(val, f"field {key} annotation must be a type")
                     for key, val in types.items()}

    def annotate(format):
        Format = _lazy_annotationlib.Format
        if format in (Format.VALUE, Format.FORWARDREF):
            return checked_types
        if format == Format.STRING:
            return _lazy_annotationlib.annotations_to_string(types)
        raise NotImplementedError(format)
    return annotate


# attributes that cannot be set in a NamedTuple class body
_prohibited = frozenset({'__new__', '__init__', '__slots__', '__getnewargs__',
                         '_fields', '_field_defaults',
                         '_make', '_replace', '_asdict', '_source'})

_special = frozenset({'__module__', '__name__', '__annotations__', '__annotate__',
                      '__annotate_func__', '__annotations_cache__'})


class NamedTupleMeta(type):
    def __new__(cls, typename, bases, ns):
        assert _NamedTuple in bases
        if "__classcell__" in ns:
            raise TypeError(
                "uses of super() and __class__ are unsupported in methods of NamedTuple subclasses")
        for base in bases:
            if base is not _NamedTuple and base is not Generic:
                raise TypeError('can only inherit from a NamedTuple type and Generic')
        bases = tuple(tuple if base is _NamedTuple else base for base in bases)
        if "__annotations__" in ns:
            types = ns["__annotations__"]
            field_names = list(types)
            annotate = _make_eager_annotate(types)
        elif (original_annotate := _lazy_annotationlib.get_annotate_from_class_namespace(ns)) is not None:
            types = _lazy_annotationlib.call_annotate_function(
                original_annotate, _lazy_annotationlib.Format.FORWARDREF)
            field_names = list(types)

            # validate the field types at class creation time, as before PEP 649
            for typ in types.values():
                _type_check(typ, "field annotation must be a type")

            def annotate(format):
                annos = _lazy_annotationlib.call_annotate_function(original_annotate, format)
                if format != _lazy_annotationlib.Format.STRING:
                    return {key: _type_check(val, f"field {key} annotation must be a type")
                            for key, val in annos.items()}
                return annos
        else:
            field_names = []
            annotate = lambda format: {}
        default_names = []
        for field_name in field_names:
            if field_name in ns:
                default_names.append(field_name)
            elif default_names:
                raise TypeError(f"Non-default namedtuple field {field_name} "
                                f"cannot follow default field"
                                f"{'s' if len(default_names) > 1 else ''} "
                                f"{', '.join(default_names)}")
        nm_tpl = _make_nmtuple(typename, field_names, annotate,
                               defaults=[ns[n] for n in default_names],
                               module=ns['__module__'])
        nm_tpl.__bases__ = bases
        if Generic in bases:
            nm_tpl.__class_getitem__ = classmethod(_generic_class_getitem)
        # copy the user namespace without clobbering the namedtuple machinery
        for key, val in ns.items():
            if key in _prohibited:
                raise AttributeError("Cannot overwrite NamedTuple attribute " + key)
            elif key not in _special:
                if key not in nm_tpl._fields:
                    setattr(nm_tpl, key, val)
                try:
                    set_name = type(val).__set_name__
                except AttributeError:
                    pass
                else:
                    try:
                        set_name(val, nm_tpl, key)
                    except BaseException as e:
                        e.add_note(
                            f"Error calling __set_name__ on {type(val).__name__!r} "
                            f"instance {key!r} in {typename!r}"
                        )
                        raise

        if Generic in bases:
            nm_tpl.__init_subclass__()
        return nm_tpl


def NamedTuple(typename, fields, /):
    """Typed version of collections.namedtuple.

    Usage::

        class Employee(NamedTuple):
            name: str
            id: int

    or, equivalently, Employee = NamedTuple('Employee', [('name', str), ('id', int)]).
    """
    fields = list(fields)
    types = {n: _type_check(t, f"field {n} annotation must be a type")
             for n, t in fields}
    field_names = [n for n, _ in fields]
    nt = _make_nmtuple(typename, field_names, _make_eager_annotate(types), module=_caller())
    nt.__orig_bases__ = (NamedTuple,)
    return nt

_NamedTuple = type.__new__(NamedTupleMeta, 'NamedTuple', (), {})


def _namedtuple_mro_entries(bases):
    assert NamedTuple in bases
    return (_NamedTuple,)

NamedTuple.__mro_entries__ = _namedtuple_mro_entries


# ---------------------------------------------------------------- TypedDict

def _get_typeddict_qualifiers(annotation_type):
    while True:
        annotation_origin = get_origin(annotation_type)
        if annotation_origin is Annotated:
            annotation_args = get_args(annotation_type)
            if annotation_args:
                annotation_type = annotation_args[0]
            else:
                break
        elif annotation_origin is Required:
            yield Required
            (annotation_type,) = get_args(annotation_type)
        elif annotation_origin is NotRequired:
            yield NotRequired
            (annotation_type,) = get_args(annotation_type)
        elif annotation_origin is ReadOnly:
            yield ReadOnly
            (annotation_type,) = get_args(annotation_type)
        else:
            break


class _NoExtraItemsType:
    """Type of the NoExtraItems sentinel (PEP 728)."""
    __slots__ = ()
    _instance = None

    def __new__(cls):
        if cls._instance is None:
            cls._instance = object.__new__(cls)
        return cls._instance

    def __repr__(self):
        return 'typing.NoExtraItems'

    def __reduce__(self):
        return 'NoExtraItems'

NoExtraItems = _NoExtraItemsType()


class _TypedDictMeta(type):
    def __new__(cls, name, bases, ns, total=True, closed=None,
                extra_items=NoExtraItems):
        """Create a TypedDict class: a plain dict subclass whose metadata
        (__required_keys__, __optional_keys__, ...) is computed from the
        annotations of the class and its TypedDict bases.
        """
        for base in bases:
            if type(base) is not _TypedDictMeta and base is not Generic:
                raise TypeError('cannot inherit from both a TypedDict type '
                                'and a non-TypedDict base class')
        if closed is not None and extra_items is not NoExtraItems:
            raise TypeError(f"Cannot combine closed={closed!r} and extra_items")

        if any(issubclass(b, Generic) for b in bases):
            generic_base = (Generic,)
        else:
            generic_base = ()

        ns_annotations = ns.pop('__annotations__', None)

        tp_dict = type.__new__(_TypedDictMeta, name, (*generic_base, dict), ns)

        if not hasattr(tp_dict, '__orig_bases__'):
            tp_dict.__orig_bases__ = bases

        if ns_annotations is not None:
            own_annotate = None
            own_annotations = ns_annotations
        elif (own_annotate := _lazy_annotationlib.get_annotate_from_class_namespace(ns)) is not None:
            own_annotations = _lazy_annotationlib.call_annotate_function(
                own_annotate, _lazy_annotationlib.Format.FORWARDREF, owner=tp_dict
            )
        else:
            own_annotate = None
            own_annotations = {}
        msg = "TypedDict('Name', {f0: t0, f1: t1, ...}); each t must be a type"
        own_checked_annotations = {
            n: _type_check(tp, msg, owner=tp_dict, module=tp_dict.__module__)
            for n, tp in own_annotations.items()
        }
        required_keys = set()
        optional_keys = set()
        readonly_keys = set()
        mutable_keys = set()

        for base in bases:
            base_required = base.__dict__.get('__required_keys__', set())
            required_keys |= base_required
            optional_keys -= base_required

            base_optional = base.__dict__.get('__optional_keys__', set())
            required_keys -= base_optional
            optional_keys |= base_optional

            readonly_keys.update(base.__dict__.get('__readonly_keys__', ()))
            mutable_keys.update(base.__dict__.get('__mutable_keys__', ()))

        for annotation_key, annotation_type in own_checked_annotations.items():
            qualifiers = set(_get_typeddict_qualifiers(annotation_type))
            if Required in qualifiers:
                is_required = True
            elif NotRequired in qualifiers:
                is_required = False
            else:
                is_required = total

            if is_required:
                required_keys.add(annotation_key)
                optional_keys.discard(annotation_key)
            else:
                optional_keys.add(annotation_key)
                required_keys.discard(annotation_key)

            if ReadOnly in qualifiers:
                if annotation_key in mutable_keys:
                    raise TypeError(
                        f"Cannot override mutable key {annotation_key!r}"
                        " with read-only key"
                    )
                readonly_keys.add(annotation_key)
            else:
                mutable_keys.add(annotation_key)
                readonly_keys.discard(annotation_key)

        # the annotations of the bases (in reverse MRO order) followed by our own
        def __annotate__(format):
            annos = {}
            for base in bases:
                if base is Generic:
                    continue
                base_annotate = base.__annotate__
                if base_annotate is None:
                    continue
                base_annos = _lazy_annotationlib.call_annotate_function(
                    base_annotate, format, owner=base)
                annos.update(base_annos)
            if own_annotate is not None:
                own = _lazy_annotationlib.call_annotate_function(
                    own_annotate, format, owner=tp_dict)
                if format != _lazy_annotationlib.Format.STRING:
                    own = {
                        n: _type_check(tp, msg, module=tp_dict.__module__)
                        for n, tp in own.items()
                    }
            elif format == _lazy_annotationlib.Format.STRING:
                own = _lazy_annotationlib.annotations_to_string(own_annotations)
            elif format in (_lazy_annotationlib.Format.FORWARDREF,
                            _lazy_annotationlib.Format.VALUE):
                own = own_checked_annotations
            else:
                raise NotImplementedError(format)
            annos.update(own)
            return annos

        tp_dict.__annotate__ = __annotate__
        tp_dict.__required_keys__ = frozenset(required_keys)
        tp_dict.__optional_keys__ = frozenset(optional_keys)
        tp_dict.__readonly_keys__ = frozenset(readonly_keys)
        tp_dict.__mutable_keys__ = frozenset(mutable_keys)
        tp_dict.__total__ = total
        tp_dict.__closed__ = closed
        tp_dict.__extra_items__ = extra_items
        return tp_dict

    __call__ = dict  # static method

    def __subclasscheck__(cls, other):
        # TypedDict does not support class checks
        raise TypeError('TypedDict does not support instance and class checks')

    __instancecheck__ = __subclasscheck__


def TypedDict(typename, fields, /, *, total=True, closed=None,
              extra_items=NoExtraItems):
    """A simple typed namespace; at runtime equivalent to a plain dict.

    Usage::

        class Point2D(TypedDict):
            x: int
            y: int
            label: str

    or Point2D = TypedDict('Point2D', {'x': int, 'y': int, 'label': str}).
    total=False makes all keys optional; Required[]/NotRequired[] override
    this per key and ReadOnly[] marks read-only keys. closed and extra_items
    describe the allowed extra keys (PEP 728).
    """
    ns = {'__annotations__': dict(fields)}
    module = _caller()
    if module is not None:
        # set correct module for pickling
        ns['__module__'] = module

    td = _TypedDictMeta(typename, (), ns, total=total, closed=closed,
                        extra_items=extra_items)
    td.__orig_bases__ = (TypedDict,)
    return td

_TypedDict = type.__new__(_TypedDictMeta, 'TypedDict', (), {})
TypedDict.__mro_entries__ = lambda bases: (_TypedDict,)


@_SpecialForm
def Required(self, parameters):
    """Marks a TypedDict key as required (in a total=False TypedDict)."""
    item = _type_check(parameters, f'{self._name} accepts only a single type.')
    return _GenericAlias(self, (item,))


@_SpecialForm
def NotRequired(self, parameters):
    """Marks a TypedDict key as potentially missing."""
    item = _type_check(parameters, f'{self._name} accepts only a single type.')
    return _GenericAlias(self, (item,))


@_SpecialForm
def ReadOnly(self, parameters):
    """Marks a TypedDict key as read-only."""
    item = _type_check(parameters, f'{self._name} accepts only a single type.')
    return _GenericAlias(self, (item,))



class NewType:
    """NewType('UserId', int) creates a distinct type for type checkers.

    At runtime it is a callable returning its argument unchanged.
    """

    __call__ = _idfunc

    def __init__(self, name, tp):
        self.__qualname__ = name
        if '.' in name:
            name = name.rpartition('.')[-1]
        self.__name__ = name
        self.__supertype__ = tp
        def_mod = _caller()
        if def_mod != 'typing':
            self.__module__ = def_mod

    def __mro_entries__(self, bases):
        # Give a helpful error when someone tries to subclass a NewType.
        superclass_name = self.__name__

        class Dummy:
            def __init_subclass__(cls):
                subclass_name = cls.__name__
                raise TypeError(
                    f"Cannot subclass an instance of NewType. Perhaps you were looking for: "
                    f"`{subclass_name} = NewType({subclass_name!r}, {superclass_name})`"
                )

        return (Dummy,)

    def __repr__(self):
        return f'{self.__module__}.{self.__qualname__}'

    def __reduce__(self):
        return self.__qualname__

    def __or__(self, other):
        return Union[self, other]

    def __ror__(self, other):
        return Union[other, self]


# Python-version-independent alias for str.
Text = str

# Always False at runtime; True for static type checkers.
TYPE_CHECKING = False


class IO(Generic[AnyStr]):
    """Generic base of the TextIO and BinaryIO stub classes (not io's classes)."""

    __slots__ = ()

    @property
    @abstractmethod
    def mode(self) -> str:
        pass

    @property
    @abstractmethod
    def name(self) -> str:
        pass

    @abstractmethod
    def close(self) -> None:
        pass

    @property
    @abstractmethod
    def closed(self) -> bool:
        pass

    @abstractmethod
    def fileno(self) -> int:
        pass

    @abstractmethod
    def flush(self) -> None:
        pass

    @abstractmethod
    def isatty(self) -> bool:
        pass

    @abstractmethod
    def read(self, n: int = -1) -> AnyStr:
        pass

    @abstractmethod
    def readable(self) -> bool:
        pass

    @abstractmethod
    def readline(self, limit: int = -1) -> AnyStr:
        pass

    @abstractmethod
    def readlines(self, hint: int = -1) -> list[AnyStr]:
        pass

    @abstractmethod
    def seek(self, offset: int, whence: int = 0) -> int:
        pass

    @abstractmethod
    def seekable(self) -> bool:
        pass

    @abstractmethod
    def tell(self) -> int:
        pass

    @abstractmethod
    def truncate(self, size: int | None = None) -> int:
        pass

    @abstractmethod
    def writable(self) -> bool:
        pass

    @abstractmethod
    def write(self, s: AnyStr) -> int:
        pass

    @abstractmethod
    def writelines(self, lines: list[AnyStr]) -> None:
        pass

    @abstractmethod
    def __enter__(self) -> IO[AnyStr]:
        pass

    @abstractmethod
    def __exit__(self, type, value, traceback) -> None:
        pass


class BinaryIO(IO[bytes]):
    """Typed version of the return of open() in binary mode."""

    __slots__ = ()

    @abstractmethod
    def write(self, s: bytes | bytearray) -> int:
        pass

    @abstractmethod
    def __enter__(self) -> BinaryIO:
        pass


class TextIO(IO[str]):
    """Typed version of the return of open() in text mode."""

    __slots__ = ()

    @property
    @abstractmethod
    def buffer(self) -> BinaryIO:
        pass

    @property
    @abstractmethod
    def encoding(self) -> str:
        pass

    @property
    @abstractmethod
    def errors(self) -> str | None:
        pass

    @property
    @abstractmethod
    def line_buffering(self) -> bool:
        pass

    @property
    @abstractmethod
    def newlines(self) -> Any:
        pass

    @abstractmethod
    def __enter__(self) -> TextIO:
        pass


def reveal_type[T](obj: T, /) -> T:
    """Print the runtime type of obj to stderr (type checkers show the static type)."""
    print(f"Runtime type is {type(obj).__name__!r}", file=sys.stderr)
    return obj


class _IdentityCallable(Protocol):
    def __call__[T](self, arg: T, /) -> T:
        ...


def dataclass_transform(
    *,
    eq_default: bool = True,
    order_default: bool = False,
    kw_only_default: bool = False,
    frozen_default: bool = False,
    field_specifiers: tuple[type[Any] | Callable[..., Any], ...] = (),
    **kwargs: Any,
) -> _IdentityCallable:
    """Mark a decorator, base class or metaclass as providing dataclass-like behaviour.

    The decorated object gets a __dataclass_transform__ dict recording the arguments.
    """
    def decorator(cls_or_fn):
        cls_or_fn.__dataclass_transform__ = {
            "eq_default": eq_default,
            "order_default": order_default,
            "kw_only_default": kw_only_default,
            "frozen_default": frozen_default,
            "field_specifiers": field_specifiers,
            "kwargs": kwargs,
        }
        return cls_or_fn
    return decorator


type _Func = Callable[..., Any]


def override[F: _Func](method: F, /) -> F:
    """Mark a method as overriding a base class method (sets __override__)."""
    try:
        method.__override__ = True
    except (AttributeError, TypeError):
        pass
    return method


def disjoint_base[T](cls: type[T], /) -> type[T]:
    """Mark a class as a disjoint base (PEP 800); sets __disjoint_base__."""
    cls.__disjoint_base__ = True
    return cls


def is_protocol(tp: type, /) -> bool:
    """True if tp is a Protocol class."""
    return (
        isinstance(tp, type)
        and getattr(tp, '_is_protocol', False)
        and tp != Protocol
    )


def get_protocol_members(tp: type, /) -> frozenset[str]:
    """The members of a Protocol class as a frozenset of names."""
    if not is_protocol(tp):
        raise TypeError(f'{tp!r} is not a Protocol')
    return frozenset(tp.__protocol_attrs__)


_byte_string_alias = None


def _warn_byte_string():
    import warnings
    warnings._deprecated("typing.ByteString", remove=(3, 17))


class _DeprecatedSpecialAlias(_SpecialGenericAlias, _root=True):
    # typing.ByteString: every use in a class or instance check warns
    def __instancecheck__(self, obj):
        _warn_byte_string()
        return super().__instancecheck__(obj)

    def __subclasscheck__(self, cls):
        _warn_byte_string()
        return super().__subclasscheck__(cls)


def __getattr__(attr):
    """Objects that are expensive to create are made on first access."""
    if attr == "ForwardRef":
        obj = _lazy_annotationlib.ForwardRef
    elif attr in {"Pattern", "Match"}:
        import re
        obj = _alias(getattr(re, attr), 1)
    elif attr in {"ContextManager", "AsyncContextManager"}:
        import contextlib
        obj = _alias(getattr(contextlib, f"Abstract{attr}"), 2, name=attr, defaults=(bool | None,))
    elif attr == "ByteString":
        # deprecated: warn on every access, so it is not stored in globals()
        global _byte_string_alias
        _warn_byte_string()
        if _byte_string_alias is None:
            import warnings
            with warnings.catch_warnings(action="ignore", category=DeprecationWarning):
                from collections.abc import ByteString as origin
            _byte_string_alias = _DeprecatedSpecialAlias(origin, 0, name="ByteString")
        return _byte_string_alias
    elif attr == "_collect_parameters":
        import warnings

        depr_message = (
            "The private _collect_parameters function is deprecated and will be"
            " removed in a future version of Python. Any use of private functions"
            " is discouraged and may break in the future."
        )
        warnings.warn(depr_message, category=DeprecationWarning, stacklevel=2)
        obj = _collect_type_parameters
    else:
        raise AttributeError(f"module {__name__!r} has no attribute {attr!r}")
    globals()[attr] = obj
    return obj
