"""Names for type hints and the runtime objects behind them.

This module provides:

* the special forms used inside annotations (Any, Optional, Literal,
  ClassVar, Final, Annotated, Concatenate, Unpack, TypeGuard, TypeIs, ...);
* subscriptable aliases of the standard containers and ABCs (List, Dict,
  Callable, Tuple, Iterable, Mapping, ...);
* the generic-class machinery (Generic, TypeVar, ParamSpec, TypeVarTuple)
  and structural typing (Protocol, runtime_checkable);
* class factories for NamedTuple, TypedDict and NewType;
* introspection helpers: get_type_hints, get_origin, get_args, ...

None of this affects how Python runs a program; type checkers read the
annotations, and this module makes them valid, introspectable objects.
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

# Exported names.  The test suite checks that every public module-level
# object defined here is listed (see test_all_exported_names).
__all__ = [
    # special primitives
    'Annotated', 'Any', 'Callable', 'ClassVar', 'Concatenate', 'Final',
    'ForwardRef', 'Generic', 'Literal', 'Optional', 'ParamSpec', 'Protocol',
    'Tuple', 'Type', 'TypeForm', 'TypeVar', 'TypeVarTuple', 'Union',
    # aliases of collections.abc
    'AbstractSet', 'Container', 'ContextManager', 'Hashable', 'ItemsView',
    'Iterable', 'Iterator', 'KeysView', 'Mapping', 'MappingView',
    'MutableMapping', 'MutableSequence', 'MutableSet', 'Sequence', 'Sized',
    'ValuesView', 'Awaitable', 'AsyncIterator', 'AsyncIterable', 'Coroutine',
    'Collection', 'AsyncGenerator', 'AsyncContextManager',
    # structural protocols
    'Reversible', 'SupportsAbs', 'SupportsBytes', 'SupportsComplex',
    'SupportsFloat', 'SupportsIndex', 'SupportsInt', 'SupportsRound',
    # concrete containers
    'ChainMap', 'Counter', 'Deque', 'Dict', 'DefaultDict', 'List',
    'OrderedDict', 'Set', 'FrozenSet', 'NamedTuple', 'TypedDict', 'Generator',
    # other concrete types
    'BinaryIO', 'IO', 'Match', 'Pattern', 'TextIO',
    # functions, decorators and markers
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

# Values of annotationlib.Format, so that the common paths need not import
# annotationlib (it is imported lazily, see test_lazy_import).
_FMT_VALUE = 1
_FMT_VALUE_WITH_FAKE_GLOBALS = 2
_FMT_FORWARDREF = 3
_FMT_STRING = 4

# Marks "argument not given" where None is a meaningful value.
_NOT_GIVEN = object()


def _alib():
    """Return the annotationlib module, importing it on first use."""
    import annotationlib
    return annotationlib


def _is_dunder(name):
    return len(name) > 4 and name.startswith('__') and name.endswith('__')


# ---------------------------------------------------------------------------
# Checking and converting the arguments of subscriptions
# ---------------------------------------------------------------------------

def _type_convert(arg, module=None, *, allow_special_forms=False, owner=None):
    """None stands for NoneType and a string for a forward reference."""
    if arg is None:
        return types.NoneType
    if isinstance(arg, str):
        return _make_forward_ref(arg, module=module, is_class=allow_special_forms,
                                 owner=owner)
    return arg


def _type_check(arg, msg, is_argument=True, module=None, *,
                allow_special_forms=False, owner=None):
    """Validate *arg* as something that may appear where a type is expected.

    Strings become forward references and None becomes NoneType.  Plain
    special forms (``Optional`` without brackets, ...) and tuples are
    rejected; *msg* starts the error message for the latter.  ClassVar and
    Final are accepted only when *allow_special_forms* is true (class-level
    annotations), Final also when *is_argument* is false.
    """
    arg = _type_convert(arg, module=module, allow_special_forms=allow_special_forms,
                        owner=owner)
    if isinstance(arg, _GenericAlias):
        origin = arg.__origin__
        if (origin is Generic or origin is Protocol
                or (not allow_special_forms
                    and (origin is ClassVar or (is_argument and origin is Final)))):
            raise TypeError(f"{arg} is not valid as type argument")
        return arg
    for ok in _BARE_FORMS_ALLOWED:
        if arg is ok:
            return arg
    if allow_special_forms and (arg is ClassVar or arg is Final):
        return arg
    if isinstance(arg, _SpecialForm) or arg is Generic or arg is Protocol:
        raise TypeError(f"Plain {arg} is not valid as type argument")
    if type(arg) is tuple:
        raise TypeError(f"{msg} Got {arg!r:.100}.")
    return arg


def _is_param_expr(arg):
    """True for what may stand in the parameter list of a Callable."""
    return (arg is ... or isinstance(arg, (tuple, list, ParamSpec))
            or isinstance(arg, _ConcatenateGenericAlias))


def _should_unflatten_callable_args(typ, args):
    """collections.abc.Callable[[int, str], bool] stores (int, str, bool):
    tell whether *args* of such an alias must be regrouped into
    ([int, str], bool)."""
    return (typ.__origin__ is collections.abc.Callable
            and not (len(args) == 2 and _is_param_expr(args[0])))


def _type_repr(obj):
    """The text of *obj* as it appears inside an alias repr."""
    if isinstance(obj, tuple):
        # a substituted ParamSpec: Z[[int, str]]
        return '[' + ', '.join(map(_type_repr, obj)) + ']'
    if isinstance(obj, type) and not isinstance(obj, GenericAlias):
        mod = obj.__module__
        if mod == 'builtins':
            return obj.__qualname__
        return f'{mod}.{obj.__qualname__}'
    if obj is ...:
        return '...'
    if isinstance(obj, (TypeVar, ParamSpec, TypeVarTuple, _BaseGenericAlias, Union)):
        return repr(obj)
    return _alib().type_repr(obj)


def _is_unpacked_typevartuple(x):
    return (not isinstance(x, type)
            and getattr(x, '__typing_is_unpacked_typevartuple__', False) is True)


def _is_typevar_like(x):
    return isinstance(x, (TypeVar, ParamSpec)) or _is_unpacked_typevartuple(x)


def _collect_type_parameters(args, *, enforce_default_ordering=True):
    """The type parameters occurring in *args*, in order of first appearance.

    With *enforce_default_ordering* the parameters given directly in *args*
    are validated: one without a default may not follow one with a default,
    and none with a default may follow a TypeVarTuple.
    """
    found = []
    seen_default = False
    seen_tuple_unpack = False
    for item in args:
        if isinstance(item, type):
            # a plain class: its __parameters__ (if any) is a descriptor
            continue
        if isinstance(item, tuple):
            # a ParamSpec replaced by a list of types, e.g. [T, int]
            for p in _collect_type_parameters(item, enforce_default_ordering=False):
                if p not in found:
                    found.append(p)
            continue
        if hasattr(item, '__typing_subst__'):
            if item in found:
                continue
            if enforce_default_ordering:
                has_default = item.has_default()
                if has_default and seen_tuple_unpack:
                    raise TypeError('Type parameter with a default'
                                    ' follows TypeVarTuple')
                if has_default:
                    seen_default = True
                elif seen_default:
                    raise TypeError(f'Type parameter {item!r} without a default'
                                    ' follows type parameter with a default')
            found.append(item)
            continue
        if _is_unpacked_typevartuple(item):
            seen_tuple_unpack = True
        for p in getattr(item, '__parameters__', ()):
            if p not in found:
                found.append(p)
    return tuple(found)


def _check_arg_count(cls, arguments):
    """Raise unless *arguments* fits the parameters of generic class *cls*."""
    params = cls.__parameters__
    if not params:
        raise TypeError(f"{cls} is not a generic class")
    given = len(arguments)
    wanted = len(params)
    if given == wanted:
        return
    if given > wanted:
        raise TypeError(f"Too many arguments for {cls}; actual {given}, expected {wanted}")
    # Too few: fine when every missing parameter has a default (defaults
    # only ever trail, see _collect_type_parameters).
    if params[given].has_default():
        return
    required = wanted - sum(1 for p in params if p.has_default())
    raise TypeError(f"Too few arguments for {cls}; actual {given}, "
                    f"expected at least {required}")


def _unpack_args(*args):
    """Splice the members of unpacked fixed-length tuples (*tuple[int, str])
    into the argument list; arbitrary-length ones stay as they are."""
    out = []
    for a in args:
        inner = getattr(a, '__typing_unpacked_tuple_args__', None)
        if isinstance(inner, tuple) and not (inner and inner[-1] is ...):
            out.extend(inner)
        else:
            out.append(a)
    return out


def _deduplicate(params, *, unhashable_fallback=False):
    """Drop repeated entries, keeping the first occurrence of each."""
    try:
        return list(dict.fromkeys(params))
    except TypeError:
        if not unhashable_fallback:
            raise
    kept = []
    for p in params:
        if p not in kept:
            kept.append(p)
    return kept


def _flatten_literal_params(parameters):
    """Literal[Literal[1, 2], 3] means Literal[1, 2, 3]."""
    flat = []
    for p in parameters:
        if isinstance(p, _LiteralGenericAlias):
            flat.extend(p.__args__)
        else:
            flat.append(p)
    return tuple(flat)


def _value_and_type_iter(parameters):
    # Literal[1] and Literal[True] differ although 1 == True.
    return ((p, type(p)) for p in parameters)


# ---------------------------------------------------------------------------
# Caching of subscriptions
# ---------------------------------------------------------------------------

# cache_clear functions of every subscription cache; regrtest and the tests
# call them to reset state between runs.
_cleanups = []


def _tp_cache(func=None, /, *, typed=False):
    """Memoize a subscription function.

    Calls with unhashable arguments (Literal[[]], Z[[int, str]], ...) are
    passed through to the uncached function.
    """
    def decorate(f):
        cached = functools.lru_cache(typed=typed)(f)
        _cleanups.append(cached.cache_clear)

        @functools.wraps(f)
        def lookup(*args, **kwds):
            try:
                if not kwds:
                    return cached(*args)
                return cached(*args, **kwds)
            except TypeError:
                try:
                    hash(args)
                    hash(tuple(kwds.values()))
                except TypeError:
                    return f(*args, **kwds)
                raise
        return lookup

    if func is not None:
        return decorate(func)
    return decorate


def _deprecation_warning_for_no_type_params_passed(funcname: str) -> None:
    import warnings
    warnings.warn(
        f"Failing to pass a value to the 'type_params' parameter of {funcname!r}"
        f" is deprecated: without it, {funcname} cannot resolve a stringified"
        " annotation that names a PEP 695 type parameter. Passing nothing will"
        " be an error in a future version of Python.",
        category=DeprecationWarning, stacklevel=3)


# ---------------------------------------------------------------------------
# Forward references and evaluation
# ---------------------------------------------------------------------------

def _make_forward_ref(code, *, parent_fwdref=None, **kwargs):
    """A ForwardRef for *code*; inside the evaluation of *parent_fwdref* it
    inherits the names the parent was given by the stringifier."""
    if parent_fwdref is not None and kwargs.get('module') is None:
        kwargs['module'] = parent_fwdref.__forward_module__
    ref = _alib().ForwardRef(code, **kwargs)
    # compile now, so that a malformed string fails where it is written
    ref.__forward_code__
    if parent_fwdref is not None:
        extra = parent_fwdref.__extra_names__
        if extra:
            ref.__extra_names__ = dict(extra)
    return ref


def evaluate_forward_ref(forward_ref, *, owner=None, globals=None, locals=None,
                         type_params=None, format=None,
                         _recursive_guard=frozenset()):
    """Evaluate a forward reference and the forward references inside it.

    Unlike ForwardRef.evaluate(), strings and forward references found in
    the result (for example in ``list["int"]``) are evaluated too.
    *owner* supplies the namespaces and type parameters when *globals*,
    *locals* or *type_params* are not given.  *format* is an
    annotationlib.Format: VALUE (the default) raises for undefined names,
    FORWARDREF leaves them as ForwardRef objects, STRING returns the source.
    """
    alib = _alib()
    if format == alib.Format.STRING:
        return forward_ref.evaluate(format=format)
    if forward_ref.__forward_arg__ in _recursive_guard:
        return forward_ref
    if format is None:
        format = alib.Format.VALUE
    if forward_ref.__forward_module__ is not None:
        # a reference created for a module can always see that module's
        # names; names in the namespaces passed in take precedence
        module = sys.modules.get(forward_ref.__forward_module__, None)
        if module is not None:
            if globals is None:
                globals = module.__dict__
            elif globals is not module.__dict__:
                globals = {**module.__dict__, **globals}
    value = forward_ref.evaluate(globals=globals, locals=locals,
                                 type_params=type_params, owner=owner, format=format)
    if isinstance(value, alib.ForwardRef) and format == alib.Format.FORWARDREF:
        return value
    if owner is None:
        owner = forward_ref.__owner__
    if isinstance(value, str):
        value = _make_forward_ref(
            value,
            module=forward_ref.__forward_module__,
            owner=owner,
            is_argument=forward_ref.__forward_is_argument__,
            is_class=forward_ref.__forward_is_class__,
        )
    return _eval_type(value, globals, locals, type_params,
                      recursive_guard=_recursive_guard | {forward_ref.__forward_arg__},
                      format=format, owner=owner, parent_fwdref=forward_ref)


def _respecialize(t, args):
    """A types.GenericAlias like *t* (same class, starred or not) but with
    *args*; Callable arguments are regrouped for collections.abc.Callable."""
    if _should_unflatten_callable_args(t, args):
        new = t.__origin__[(list(args[:-1]), args[-1])]
    else:
        new = type(t)(t.__origin__, args)
    if t.__unpacked__:
        new = next(iter(new))
    return new


def _rebuild_builtin_alias(t, parent_fwdref):
    """Turn string arguments of a types.GenericAlias into ForwardRefs."""
    old = t.__args__
    if not any(isinstance(a, str) for a in old):
        return t
    return _respecialize(t, tuple(_make_forward_ref(a, parent_fwdref=parent_fwdref)
                                  if isinstance(a, str) else a for a in old))


def _eval_type(t, globalns, localns, type_params=_NOT_GIVEN, *,
               recursive_guard=frozenset(), format=None, owner=None,
               parent_fwdref=None):
    """Evaluate the forward references in *t*, recursively.

    Aliases are rebuilt only when one of their arguments changed.
    """
    if type_params is _NOT_GIVEN:
        _deprecation_warning_for_no_type_params_passed("typing._eval_type")
        type_params = ()
    if isinstance(t, (_GenericAlias, GenericAlias, Union)):
        if isinstance(t, GenericAlias):
            t = _rebuild_builtin_alias(t, parent_fwdref)
        old = t.__args__
        new = tuple(_eval_type(a, globalns, localns, type_params,
                               recursive_guard=recursive_guard, format=format,
                               owner=owner, parent_fwdref=parent_fwdref)
                    for a in old)
        if new == old:
            return t
        if isinstance(t, GenericAlias):
            return _respecialize(t, new)
        if isinstance(t, Union):
            return functools.reduce(operator.or_, new)
        return t.copy_with(new)
    if type(t).__name__ == 'ForwardRef' and isinstance(t, _alib().ForwardRef):
        return evaluate_forward_ref(t, globals=globalns, locals=localns,
                                    type_params=type_params, owner=owner,
                                    format=format, _recursive_guard=recursive_guard)
    return t


# ---------------------------------------------------------------------------
# Base classes of the typing objects
# ---------------------------------------------------------------------------

class _Final:
    """Base of the typing objects; subclassing needs the _root keyword."""

    __slots__ = ('__weakref__',)

    def __init_subclass__(cls, /, *args, **kwds):
        if '_root' not in kwds:
            raise TypeError("Cannot subclass special typing classes")


class _NotIterable:
    """Mixin: iter() fails and the object is not a collections.abc.Iterable.

    Generic aliases define __iter__ so that ``*Tuple[int]`` works; this
    switches that off where unpacking makes no sense.
    """

    __slots__ = ()
    __iter__ = None


class _SpecialForm(_Final, _NotIterable, _root=True):
    # A named, subscriptable typing construct such as Optional or ClassVar.
    # _SpecialForm(getitem) takes its name and docstring from *getitem*,
    # which is called as getitem(form, parameters) on subscription.  (No
    # class docstring: each instance carries its own __doc__ in a slot.)

    __slots__ = ('_name', '__doc__', '_getitem', '_call')

    def __init__(self, getitem):
        self._getitem = getitem
        self._name = getitem.__name__
        self.__doc__ = getitem.__doc__
        # forms that may be called (TypeForm) store the behaviour here
        self._call = None

    def __getattr__(self, item):
        if item == '__name__' or item == '__qualname__':
            return self._name
        raise AttributeError(item)

    def __mro_entries__(self, bases):
        raise TypeError(f"Cannot subclass {self!r}")

    def __repr__(self):
        return 'typing.' + self._name

    def __reduce__(self):
        return self._name

    def __call__(self, *args, **kwds):
        if self._call is not None:
            return self._call(*args, **kwds)
        raise TypeError(f"Cannot instantiate {self!r}")

    def __or__(self, other):
        return Union[self, other]

    def __ror__(self, other):
        return Union[other, self]

    def __instancecheck__(self, obj):
        raise TypeError(f"{self} cannot be used with isinstance()")

    def __subclasscheck__(self, cls):
        raise TypeError(f"{self} cannot be used with issubclass()")

    def __getitem__(self, parameters):
        return self._getitem(self, parameters)


def _form(name, doc, getitem):
    getitem.__name__ = getitem.__qualname__ = name
    getitem.__doc__ = doc
    return _SpecialForm(getitem)


def _plain_form(name, doc):
    """A special form that cannot be subscripted."""
    def refuse(self, parameters):
        raise TypeError(f"{self} is not subscriptable")
    return _form(name, doc, refuse)


def _wrapper_form(name, doc, *, class_level=False):
    """A special form taking exactly one type: X[t]."""
    @_tp_cache
    def wrap(self, parameters):
        item = _type_check(parameters, f'{self} accepts only a single type.',
                           allow_special_forms=class_level)
        return _GenericAlias(self, (item,))
    return _form(name, doc, wrap)


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
    """The type that is compatible with every type, in both directions.

    A value of type Any may be used where any type is expected, and any
    value may be assigned to something of type Any.  It cannot be used with
    isinstance(); classes may inherit from it to tell a type checker that
    they are compatible with everything.
    """

    def __new__(cls, *args, **kwargs):
        if cls is Any:
            raise TypeError("Any cannot be instantiated")
        return super().__new__(cls)


NoReturn = _plain_form('NoReturn', """Annotation for functions that never return normally.

    Such a function always raises or loops forever.  NoReturn is the same
    bottom type as Never; it is conventionally used for return annotations.
    """)

Never = _plain_form('Never', """The bottom type: no value has this type.

    Use it for code that can never be reached, or as the return type of a
    function that never returns (see also NoReturn and assert_never()).
    """)

Self = _plain_form('Self', """The type of the enclosing class's instance.

    Inside a class body, ``def copy(self) -> Self`` says that the method
    returns an instance of whatever (sub)class it was called on.
    """)

LiteralString = _plain_form('LiteralString', """The type of strings built only from literals.

    Literal strings, and strings combined from them, are LiteralString;
    a parameter annotated with it rejects strings derived from user input.
    At runtime it is equivalent to str.
    """)

TypeAlias = _plain_form('TypeAlias', """Marks an explicit type alias definition.

    ``Vector: TypeAlias = list[float]`` tells a type checker that the
    assignment defines an alias rather than a variable.  New code may use
    the ``type`` statement instead.
    """)

ClassVar = _wrapper_form('ClassVar', """Marks an attribute as belonging to the class.

    ``count: ClassVar[int] = 0`` in a class body declares a class variable
    that instances should not set.  ClassVar takes exactly one type and may
    not be nested inside other types.
    """, class_level=True)

Final = _wrapper_form('Final', """Marks a name that must not be reassigned or overridden.

    ``MAX: Final = 9000`` declares a constant; in a class body it also
    forbids redefinition in subclasses.  There is no runtime enforcement.
    """, class_level=True)


@_tp_cache
def _optional(self, parameters):
    arg = _type_check(parameters, f"{self} requires a single type.")
    return Union[arg, types.NoneType]


Optional = _form('Optional', """Optional[X] means X | None.

    It is not about optional arguments: a parameter with a default value
    does not need Optional unless None is a valid value for it.
    """, _optional)


@_tp_cache(typed=True)
def _literal_alias(*values):
    values = _flatten_literal_params(values)
    try:
        values = tuple(v for v, _ in _deduplicate(list(_value_and_type_iter(values)),
                                                  unhashable_fallback=True))
    except TypeError:
        # values that cannot even be compared are kept as given
        pass
    return _LiteralGenericAlias(Literal, values)


def _literal(self, parameters):
    if not isinstance(parameters, tuple):
        parameters = (parameters,)
    return _literal_alias(*parameters)


Literal = _form('Literal', """The type of exactly the listed values.

    ``Literal['r', 'w']`` accepts only those two strings.  The values may be
    ints, strings, bytes, bools, enum members and None; nested Literals are
    flattened and repeated values dropped.  Equality of Literal types does
    not depend on the order of the values.
    """, _literal)


@_tp_cache
def _concatenate_alias(parameters):
    *head, tail = parameters
    msg = "Concatenate[arg, ...]: each arg must be a type."
    return _ConcatenateGenericAlias(
        Concatenate, (*(_type_check(p, msg) for p in head), tail))


def _concatenate(self, parameters):
    if parameters == ():
        raise TypeError("Cannot take a Concatenate of no types.")
    if not isinstance(parameters, tuple):
        parameters = (parameters,)
    tail = parameters[-1]
    if not (tail is ... or isinstance(tail, ParamSpec)):
        raise TypeError("The last parameter to Concatenate should be a "
                        "ParamSpec variable or ellipsis.")
    return _concatenate_alias(parameters)


Concatenate = _form('Concatenate', """Prepends positional parameters to a ParamSpec.

    ``Callable[Concatenate[int, P], R]`` is a callable taking an int and
    then whatever parameters P stands for.  The last argument must be a
    ParamSpec or an ellipsis.
    """, _concatenate)

TypeGuard = _wrapper_form('TypeGuard', """Return annotation of a user-defined type guard.

    A function ``def is_str_list(v: list[object]) -> TypeGuard[list[str]]``
    tells a type checker that when it returns True its first argument is a
    list[str].  Unlike TypeIs, nothing is narrowed when it returns False.
    """)

TypeIs = _wrapper_form('TypeIs', """Return annotation of a narrowing predicate.

    ``def is_int(v: object) -> TypeIs[int]`` narrows its argument to int
    when it returns True and excludes int when it returns False.  The
    narrowed type must be consistent with the parameter's type.
    """)

TypeForm = _wrapper_form('TypeForm', """The type of type expressions.

    ``TypeForm[int | str]`` describes values such as ``int``, ``str`` or
    ``int | None`` used as objects.  Calling TypeForm returns its argument
    unchanged, which lets code mark an expression as a type form.
    """)


def _identity(value, /):
    return value


TypeForm._call = _identity


# ---------------------------------------------------------------------------
# Substitution hooks of TypeVar, ParamSpec and TypeVarTuple
# (the _typing objects call these through __typing_subst__ and
# __typing_prepare_subst__)
# ---------------------------------------------------------------------------

def _typevar_subst(self, arg):
    arg = _type_check(arg, "Parameters to generic types must be types.",
                      is_argument=True)
    if ((isinstance(arg, _GenericAlias) and arg.__origin__ is Unpack)
            or (isinstance(arg, GenericAlias) and getattr(arg, '__unpacked__', False))):
        raise TypeError(f"{arg} is not valid as type argument")
    return arg


def _typevartuple_prepare_subst(self, alias, args):
    """Group the arguments so that the TypeVarTuple *self* receives a tuple.

    The parameters before and after *self* take one argument each; *self*
    takes the rest.  An unpacked arbitrary-length tuple ``*tuple[X, ...]``
    among the arguments can also supply X for the fixed parameters around
    it.
    """
    params = alias.__parameters__
    where = params.index(self)
    if any(isinstance(p, TypeVarTuple) for p in params[where + 1:]):
        raise TypeError(f"More than one TypeVarTuple parameter in {alias}")
    nargs = len(args)
    before = where
    after = len(params) - where - 1
    star_at = None
    filler = None
    for i, a in enumerate(args):
        if isinstance(a, type):
            continue
        inner = getattr(a, '__typing_unpacked_tuple_args__', None)
        if isinstance(inner, tuple) and len(inner) == 2 and inner[-1] is ...:
            if star_at is not None:
                raise TypeError("More than one unpacked arbitrary-length tuple argument")
            star_at = i
            filler = inner[0]
    if star_at is not None:
        # the fixed parameters overlapping the variadic argument get its
        # element type
        take_before = min(before, star_at)
        take_after = min(after, nargs - star_at - 1)
    elif before + after > nargs:
        raise TypeError(f"Too few arguments for {alias}; actual {nargs}, "
                        f"expected at least {len(params) - 1}")
    else:
        take_before, take_after = before, after
    end = nargs - take_after
    if take_before == end and self.has_default():
        middle = tuple(_unpack_args(self.__default__))
    else:
        middle = tuple(args[take_before:end])
    return (*args[:take_before],
            *[filler] * (before - take_before),
            middle,
            *[filler] * (after - take_after),
            *args[end:])


def _paramspec_subst(self, arg):
    if isinstance(arg, (list, tuple)):
        return tuple(_type_check(a, "Expected a type.") for a in arg)
    if not _is_param_expr(arg):
        raise TypeError(f"Expected a list of types, an ellipsis, "
                        f"ParamSpec, or Concatenate. Got {arg}")
    return arg


def _paramspec_prepare_subst(self, alias, args):
    params = alias.__parameters__
    where = params.index(self)
    if where == len(args) and self.has_default():
        args = (*args, self.__default__)
    if where >= len(args):
        raise TypeError(f"Too few arguments for {alias}")
    if len(params) == 1 and not _is_param_expr(args[0]):
        # Z[int, str] is accepted as Z[[int, str]] when P is the only
        # parameter (PEP 612)
        return (tuple(args),)
    if isinstance(args[where], list):
        # a tuple keeps the result hashable (and cacheable)
        return (*args[:where], tuple(args[where]), *args[where + 1:])
    return args


# ---------------------------------------------------------------------------
# Generic: subscription and subclassing (called by _typing.Generic)
# ---------------------------------------------------------------------------

@_tp_cache
def _generic_class_getitem(cls, args):
    """Generic[T, ...], Protocol[T, ...] and C[int, ...] for generic C."""
    if not isinstance(args, tuple):
        args = (args,)
    args = tuple(map(_type_convert, args))
    if cls is Generic or cls is Protocol:
        if not args:
            raise TypeError(f"Parameter list to {cls.__qualname__}[...] cannot be empty")
        if not all(map(_is_typevar_like, args)):
            raise TypeError(f"Parameters to {cls.__name__}[...] must all be type "
                            "variables or parameter specification variables.")
        if len(set(args)) != len(args):
            raise TypeError(f"Parameters to {cls.__name__}[...] must all be unique")
        return _GenericAlias(cls, args)
    try:
        params = cls.__parameters__
    except AttributeError as exc:
        _note_missing_init_subclass(cls, exc)
        raise
    for p in params:
        prepare = getattr(p, '__typing_prepare_subst__', None)
        if prepare is not None:
            args = prepare(cls, args)
    _check_arg_count(cls, args)
    flat = []
    for p, a in zip(params, args):
        if isinstance(p, TypeVarTuple):
            flat.extend(a)
        else:
            flat.append(a)
    return _GenericAlias(cls, tuple(flat))


def _note_missing_init_subclass(cls, exc):
    """__parameters__ is missing when an __init_subclass__ in the MRO did not
    chain up to Generic's; point at the likely culprit."""
    for klass in cls.__mro__:
        if '__init_subclass__' in klass.__dict__:
            if klass is not Generic and klass is not object:
                culprit = getattr(cls, '__init_subclass__')
                exc.add_note(
                    f"Note: this exception may have been caused by "
                    f"{culprit.__qualname__!r} (or the '__init_subclass__' "
                    f"method on a superclass) not calling "
                    f"'super().__init_subclass__()'")
            return


def _generic_init_subclass(cls, *args, **kwargs):
    """Compute __parameters__ of a new subclass of Generic."""
    super(Generic, cls).__init_subclass__(*args, **kwargs)
    own = cls.__dict__
    if '__orig_bases__' in own:
        orig_bases = own['__orig_bases__']
        if Generic in orig_bases:
            raise TypeError("Cannot inherit from plain Generic")
    else:
        orig_bases = None
        if (Generic in cls.__bases__ and cls.__name__ != 'Protocol'
                and type(cls) is not _TypedDictMeta):
            raise TypeError("Cannot inherit from plain Generic")
    if orig_bases is None:
        cls.__parameters__ = ()
        return
    found = _collect_type_parameters(orig_bases)
    declared = None
    declared_by = None
    for base in orig_bases:
        if (isinstance(base, _GenericAlias)
                and (base.__origin__ is Generic or base.__origin__ is Protocol)):
            if declared is not None:
                raise TypeError("Cannot inherit from Generic[...] multiple times.")
            declared = base.__parameters__
            declared_by = base.__origin__
    if declared is not None:
        missing = [t for t in found if t not in declared]
        if missing:
            raise TypeError(
                f"Some type variables ({', '.join(map(str, missing))}) are not "
                f"listed in {declared_by.__name__}"
                f"[{', '.join(map(str, declared))}]")
        found = declared
    cls.__parameters__ = tuple(found)


# ---------------------------------------------------------------------------
# Generic aliases
# ---------------------------------------------------------------------------

# attributes kept on the alias itself instead of being forwarded to the origin
_ALIAS_OWN_ATTRS = frozenset({'_name', '_inst', '_nparams', '_defaults'})


def _brings_generic(base, bases):
    """Will *base*, listed in a class statement with *bases*, put Generic
    (or a subclass of it) into the final bases?"""
    if isinstance(base, _BaseGenericAlias):
        return True
    if isinstance(base, type):
        return issubclass(base, Generic)
    entries = getattr(base, '__mro_entries__', None)
    if entries is None:
        return False
    produced = entries(bases)
    return isinstance(produced, tuple) and any(
        isinstance(e, type) and issubclass(e, Generic) for e in produced)


class _BaseGenericAlias(_Final, _root=True):
    """Common behaviour of all typing aliases.

    Calling an alias instantiates its origin (and records the alias in
    ``__orig_class__`` when possible); other non-dunder attributes are read
    from and written to the origin.  ``_inst`` is false for aliases of
    builtin containers, which cannot be instantiated through the alias.
    """

    def __init__(self, origin, *, inst=True, name=None):
        self._inst = inst
        self._name = name
        self.__origin__ = origin

    def __call__(self, *args, **kwargs):
        if not self._inst:
            raise TypeError(f"Type {self._name} cannot be instantiated; "
                            f"use {self.__origin__.__name__}() instead")
        result = self.__origin__(*args, **kwargs)
        try:
            result.__orig_class__ = self
        except Exception:
            # objects without a __dict__, or rejecting the attribute
            pass
        return result

    def __mro_entries__(self, bases):
        entries = []
        if self.__origin__ not in bases:
            entries.append(self.__origin__)
        # Generic is added unless a later base will bring it anyway (two
        # copies of it would make the MRO inconsistent)
        later = bases[bases.index(self) + 1:]
        if not any(_brings_generic(b, bases) for b in later):
            entries.append(Generic)
        return tuple(entries)

    def __getattr__(self, attr):
        if attr in ('__name__', '__qualname__'):
            return self._name or self.__origin__.__name__
        if attr in ('__origin__', '__args__'):
            raise AttributeError(attr)
        # dunders are never forwarded, which keeps copy and pickle sane
        if '__origin__' in self.__dict__ and not _is_dunder(attr):
            return getattr(self.__origin__, attr)
        raise AttributeError(attr)

    def __setattr__(self, attr, val):
        if _is_dunder(attr) or attr in _ALIAS_OWN_ATTRS:
            object.__setattr__(self, attr, val)
        else:
            setattr(self.__origin__, attr, val)

    def __instancecheck__(self, obj):
        return self.__subclasscheck__(type(obj))

    def __subclasscheck__(self, cls):
        raise TypeError("Subscripted generics cannot be used with"
                        " class and instance checks")

    def __dir__(self):
        names = set(object.__dir__(self))
        names.update(n for n in dir(self.__origin__) if not _is_dunder(n))
        return sorted(names)


class _GenericAlias(_BaseGenericAlias, _root=True):
    """A subscripted typing object: List[int], Callable[[int], str], C[T]...

    ``__origin__`` is the unsubscripted class or special form, ``__args__``
    the arguments and ``__parameters__`` the type parameters still free in
    them.  Subscripting an alias that has parameters substitutes them::

        T = TypeVar('T')
        Pair = Tuple[T, T]
        Pair[int]            # Tuple[int, int]
    """

    def __init__(self, origin, args, *, inst=True, name=None):
        super().__init__(origin, inst=inst, name=name)
        if not isinstance(args, tuple):
            args = (args,)
        self.__args__ = args
        self.__parameters__ = _collect_type_parameters(
            args, enforce_default_ordering=(origin is Generic or origin is Protocol))
        if not name:
            self.__module__ = origin.__module__

    def __eq__(self, other):
        if not isinstance(other, _GenericAlias):
            return NotImplemented
        return self.__origin__ == other.__origin__ and self.__args__ == other.__args__

    def __hash__(self):
        return hash((self.__origin__, self.__args__))

    def __or__(self, right):
        return Union[self, right]

    def __ror__(self, left):
        return Union[left, self]

    @_tp_cache
    def __getitem__(self, args):
        origin = self.__origin__
        if origin is Generic or origin is Protocol:
            raise TypeError(f"Cannot subscript already-subscripted {self}")
        if not self.__parameters__:
            raise TypeError(f"{self} is not a generic class")
        if not isinstance(args, tuple):
            args = (args,)
        args = _unpack_args(*map(_type_convert, args))
        return self.copy_with(self._determine_new_args(args))

    def _determine_new_args(self, args):
        """The arguments of the alias after substituting *args* for its
        parameters."""
        params = self.__parameters__
        for p in params:
            prepare = getattr(p, '__typing_prepare_subst__', None)
            if prepare is not None:
                args = prepare(self, args)
        if len(args) != len(params):
            raise TypeError(
                f"Too {'many' if len(args) > len(params) else 'few'} arguments "
                f"for {self}; actual {len(args)}, expected {len(params)}")
        return tuple(self._make_substitution(self.__args__, dict(zip(params, args))))

    def _make_substitution(self, args, mapping):
        out = []
        callable_origin = self.__origin__ is collections.abc.Callable
        for old in args:
            if isinstance(old, type):
                out.append(old)
                continue
            if isinstance(old, tuple):
                # a ParamSpec already replaced by a list: substitute inside
                out.append(tuple(self._make_substitution(old, mapping)))
                continue
            subst = getattr(old, '__typing_subst__', None)
            if subst:
                new = subst(mapping[old])
            else:
                inner = getattr(old, '__parameters__', ())
                if inner:
                    replacement = []
                    for p in inner:
                        if isinstance(p, TypeVarTuple):
                            replacement.extend(mapping[p])
                        else:
                            replacement.append(mapping[p])
                    new = old[tuple(replacement)]
                else:
                    new = old
            if (callable_origin and isinstance(new, tuple)) or _is_unpacked_typevartuple(old):
                # parameter lists of a Callable and the members bound to a
                # TypeVarTuple are spliced into the argument list
                out.extend(new)
            else:
                out.append(new)
        return out

    def copy_with(self, args):
        return self.__class__(self.__origin__, args, name=self._name, inst=self._inst)

    def __repr__(self):
        head = 'typing.' + self._name if self._name else _type_repr(self.__origin__)
        if self.__args__:
            body = ', '.join(map(_type_repr, self.__args__))
        else:
            body = '()'
        return f'{head}[{body}]'

    def __reduce__(self):
        origin = globals()[self._name] if self._name else self.__origin__
        args = tuple(self.__args__)
        if len(args) == 1 and not isinstance(args[0], tuple):
            args = args[0]
        return operator.getitem, (origin, args)

    def __mro_entries__(self, bases):
        origin = self.__origin__
        if isinstance(origin, _SpecialForm):
            raise TypeError(f"Cannot subclass {self!r}")
        if self._name:
            # alias of a container or ABC
            return super().__mro_entries__(bases)
        if origin is Generic:
            if Protocol in bases:
                return ()
            for b in bases[bases.index(self) + 1:]:
                if isinstance(b, _BaseGenericAlias) and b is not self:
                    return ()
        return (origin,)

    def __iter__(self):
        yield Unpack[self]


class _SpecialGenericAlias(_NotIterable, _BaseGenericAlias, _root=True):
    """An unsubscripted alias of a standard class: List, Iterable, ...

    *nparams* is the number of arguments it takes (-1: any number);
    *defaults* fill in missing trailing arguments.
    """

    def __init__(self, origin, nparams, *, inst=True, name=None, defaults=()):
        if name is None:
            name = origin.__name__
        super().__init__(origin, inst=inst, name=name)
        self._nparams = nparams
        self._defaults = defaults
        if origin.__module__ == 'builtins':
            where = origin.__qualname__
        else:
            where = f'{origin.__module__}.{origin.__qualname__}'
        self.__doc__ = f'Subscriptable typing alias of {where}.'

    @_tp_cache
    def __getitem__(self, params):
        if not isinstance(params, tuple):
            params = (params,)
        msg = "Parameters to generic types must be types."
        params = tuple(_type_check(p, msg) for p in params)
        wanted = self._nparams
        defaults = self._defaults
        given = len(params)
        if defaults and given < wanted <= given + len(defaults):
            params = (*params, *defaults[given - wanted:])
            given = len(params)
        if given != wanted:
            if not wanted:
                raise TypeError(f"{self} is not a generic class")
            expected = (f"at least {wanted - len(defaults)}" if defaults else str(wanted))
            raise TypeError(f"Too {'many' if given > wanted else 'few'} arguments "
                            f"for {self}; actual {given}, expected {expected}")
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
    """An alias whose every use in a class or instance check warns."""

    def __init__(self, origin, nparams, *, removal_version, inst=True, name=None):
        super().__init__(origin, nparams, inst=inst, name=name)
        object.__setattr__(self, '_removal_version', removal_version)

    def _warn(self):
        import warnings
        warnings._deprecated(f"{self.__module__}.{self._name}",
                             remove=self._removal_version)

    def __instancecheck__(self, obj):
        self._warn()
        return super().__instancecheck__(obj)

    def __subclasscheck__(self, cls):
        self._warn()
        return super().__subclasscheck__(cls)

    def __mro_entries__(self, bases):
        self._warn()
        return super().__mro_entries__(bases)


class _CallableGenericAlias(_NotIterable, _GenericAlias, _root=True):
    def __repr__(self):
        args = self.__args__
        if len(args) == 2 and _is_param_expr(args[0]):
            return super().__repr__()
        params = ', '.join(map(_type_repr, args[:-1]))
        return f'typing.Callable[[{params}], {_type_repr(args[-1])}]'

    def __reduce__(self):
        args = self.__args__
        if not (len(args) == 2 and _is_param_expr(args[0])):
            args = list(args[:-1]), args[-1]
        return operator.getitem, (Callable, args)


class _CallableType(_SpecialGenericAlias, _root=True):
    def copy_with(self, params):
        return _CallableGenericAlias(self.__origin__, params,
                                     name=self._name, inst=self._inst)

    def __getitem__(self, params):
        if not isinstance(params, tuple) or len(params) != 2:
            raise TypeError("Callable must be used as "
                            "Callable[[arg, ...], result].")
        args, result = params
        if isinstance(args, list):
            # lists are unhashable; the cached step needs a tuple
            args = tuple(args)
        return self._subscript(args, result)

    @_tp_cache
    def _subscript(self, args, result):
        result = _type_check(result, "Callable[args, result]: result must be a type.")
        if args is ...:
            return self.copy_with((..., result))
        if not isinstance(args, tuple):
            args = (args,)
        return self.copy_with((*map(_type_convert, args), result))


class _TupleType(_SpecialGenericAlias, _root=True):
    @_tp_cache
    def __getitem__(self, params):
        if not isinstance(params, tuple):
            params = (params,)
        if len(params) >= 2 and params[-1] is ...:
            msg = "Tuple[t, ...]: t must be a type."
            return self.copy_with((*(_type_check(p, msg) for p in params[:-1]), ...))
        msg = "Tuple[t0, t1, ...]: each t must be a type."
        return self.copy_with(tuple(_type_check(p, msg) for p in params))


def _union_deprecated():
    import warnings
    warnings._deprecated("_UnionGenericAlias", remove=(3, 17))


class _UnionGenericAliasMeta(type):
    # isinstance/issubclass/== against the old class keep working, with a
    # warning, for code written against Python < 3.14
    def __instancecheck__(self, inst):
        _union_deprecated()
        return isinstance(inst, Union)

    def __subclasscheck__(self, inst):
        _union_deprecated()
        return issubclass(inst, Union)

    def __eq__(self, other):
        _union_deprecated()
        if other is _UnionGenericAlias or other is Union:
            return True
        return NotImplemented

    def __hash__(self):
        return hash(Union)


class _UnionGenericAlias(metaclass=_UnionGenericAliasMeta):
    """Deprecated: unions are typing.Union (types.UnionType) objects now."""

    def __new__(cls, self_cls, parameters, /, *, name=None):
        _union_deprecated()
        return Union[parameters]


class _LiteralGenericAlias(_GenericAlias, _root=True):
    def __eq__(self, other):
        if not isinstance(other, _LiteralGenericAlias):
            return NotImplemented
        return (set(_value_and_type_iter(self.__args__))
                == set(_value_and_type_iter(other.__args__)))

    def __hash__(self):
        return hash(frozenset(_value_and_type_iter(self.__args__)))


class _ConcatenateGenericAlias(_GenericAlias, _root=True):
    def copy_with(self, params):
        last = params[-1]
        if isinstance(last, (list, tuple)):
            # the ParamSpec was replaced by a parameter list
            return (*params[:-1], *last)
        if isinstance(last, _ConcatenateGenericAlias):
            params = (*params[:-1], *last.__args__)
        return super().copy_with(params)


@_tp_cache
def _unpack(self, parameters):
    item = _type_check(parameters, f'{self} accepts only a single type.')
    return _UnpackGenericAlias(origin=self, args=(item,))


Unpack = _form('Unpack', """Unpacks a TypeVarTuple or a tuple type.

    ``Unpack[Ts]`` is the same as ``*Ts``: in ``tuple[int, *Ts]`` or a
    ``Generic[*Ts]`` base it stands for any number of types.  Applied to
    a TypedDict in ``**kwargs: Unpack[Options]`` it types keyword arguments.
    """, _unpack)


class _UnpackGenericAlias(_GenericAlias, _root=True):
    def __getitem__(self, args):
        if self.__typing_is_unpacked_typevartuple__:
            return args
        return super().__getitem__(args)

    @property
    def __typing_unpacked_tuple_args__(self):
        assert self.__origin__ is Unpack
        assert len(self.__args__) == 1
        target = self.__args__[0]
        if isinstance(target, (_GenericAlias, GenericAlias)):
            if target.__origin__ is not tuple:
                raise TypeError("Unpack[...] must be used with a tuple type")
            return target.__args__
        return None

    @property
    def __typing_is_unpacked_typevartuple__(self):
        return isinstance(self.__args__[0], TypeVarTuple)


# The special forms that may appear bare in an annotation.
_BARE_FORMS_ALLOWED = (Any, LiteralString, NoReturn, Never, Self, TypeAlias)


# ---------------------------------------------------------------------------
# Protocols
# ---------------------------------------------------------------------------

# Attributes of a protocol class that are machinery, not members.
_TYPING_INTERNALS = frozenset({
    '__parameters__', '__orig_bases__', '__orig_class__',
    '_is_protocol', '_is_runtime_protocol', '__protocol_attrs__',
    '__non_callable_proto_members__', '__type_params__',
    '__typing_is_deprecated_inherited_runtime_protocol__',
})

_SPECIAL_NAMES = frozenset({
    '__abstractmethods__', '__annotations__', '__dict__', '__doc__',
    '__init__', '__module__', '__new__', '__slots__',
    '__subclasshook__', '__weakref__', '__class_getitem__',
    '__match_args__', '__static_attributes__', '__firstlineno__',
    '__annotate__', '__annotate_func__', '__annotations_cache__',
})

# Never treated as protocol members.
EXCLUDED_ATTRIBUTES = _TYPING_INTERNALS | _SPECIAL_NAMES | {'_MutableMapping__marker'}

_ANNOTATION_KEYS = ('__annotations__', '__annotate__', '__annotate_func__',
                    '__annotations_cache__')


def _annotation_names(klass):
    """The names *klass* itself annotates, without evaluating anything."""
    own = klass.__dict__
    if not any(k in own for k in _ANNOTATION_KEYS):
        return ()
    alib = _alib()
    try:
        annos = alib.get_annotations(klass, format=alib.Format.FORWARDREF)
    except Exception:
        return ()
    if not isinstance(annos, collections.abc.Mapping):
        return ()
    return tuple(annos)


def _get_protocol_attrs(cls):
    """The names a protocol class requires: everything defined or annotated
    in its MRO below Protocol/Generic, minus typing's own machinery."""
    members = set()
    for klass in cls.__mro__[:-1]:  # object contributes nothing
        if klass.__name__ in ('Protocol', 'Generic'):
            continue
        for name in (*klass.__dict__, *_annotation_names(klass)):
            if not name.startswith('_abc_') and name not in EXCLUDED_ATTRIBUTES:
                members.add(name)
    return members


def _no_init_or_replace_init(self, *args, **kwargs):
    """__init__ of protocol classes.

    Protocols cannot be instantiated.  A concrete subclass that inherits
    this __init__ gets, on its first instantiation, the next real __init__
    from its MRO installed in its place, and that one is called.
    """
    cls = type(self)
    if cls._is_protocol:
        raise TypeError('Protocols cannot be instantiated')
    if cls.__init__ is not _no_init_or_replace_init:
        # reached through super().__init__() from a custom __init__
        return
    for klass in cls.__mro__:
        found = klass.__dict__.get('__init__', _no_init_or_replace_init)
        if found is not _no_init_or_replace_init:
            cls.__init__ = found
            break
    else:
        cls.__init__ = object.__init__
    cls.__init__(self, *args, **kwargs)


def _caller(depth=1, default='__main__'):
    """The module name of the frame *depth* levels above our caller."""
    try:
        return sys._getframemodulename(depth + 1) or default
    except AttributeError:
        pass
    try:
        return sys._getframe(depth + 1).f_globals.get('__name__', default)
    except (AttributeError, ValueError):
        return None


def _allow_reckless_class_checks(depth=2):
    """abc and functools run isinstance()/issubclass() over whole MROs that
    may contain non-runtime protocols; let those calls through."""
    return _caller(depth) in ('abc', 'functools', None)


# Classes outside typing that protocols may inherit from.
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

_getattr_static = None


def _static_lookup():
    global _getattr_static
    if _getattr_static is None:
        from inspect import getattr_static
        _getattr_static = getattr_static
    return _getattr_static


def _pickle_psargs(psargs):
    return ParamSpecArgs, (psargs.__origin__,)


def _pickle_pskwargs(pskwargs):
    return ParamSpecKwargs, (pskwargs.__origin__,)


copyreg.pickle(ParamSpecArgs, _pickle_psargs)
copyreg.pickle(ParamSpecKwargs, _pickle_pskwargs)

_abc_instancecheck = ABCMeta.__instancecheck__
_abc_subclasscheck = ABCMeta.__subclasscheck__


def _type_check_issubclass_arg_1(arg):
    if not isinstance(arg, type):
        raise TypeError('issubclass() arg 1 must be a class')


def _non_callable_members(cls):
    names = set()
    for attr in cls.__protocol_attrs__:
        try:
            is_method = callable(getattr(cls, attr, None))
        except Exception as exc:
            raise TypeError(f"Failed to determine whether protocol member "
                            f"{attr!r} is a method member") from exc
        if not is_method:
            names.add(attr)
    return names


_NOT_RUNTIME_MSG = ("Instance and class checks can only be used with "
                    "@runtime_checkable protocols")


def _warn_inherited_runtime(cls):
    """*cls* is runtime-checkable only because a base protocol is."""
    import warnings
    warnings.warn(
        f"{cls!r} isn't explicitly decorated with @runtime_checkable but "
        "it is used in issubclass() or isinstance(). Instance and class "
        "checks can only be used with @runtime_checkable protocols. This "
        "will raise a TypeError in Python 3.20.",
        DeprecationWarning, stacklevel=3)
    if '__non_callable_proto_members__' not in cls.__dict__:
        cls.__non_callable_proto_members__ = _non_callable_members(cls)


class _ProtocolMeta(ABCMeta):
    # Protocol classes get structural isinstance()/issubclass() checks;
    # their concrete subclasses behave like ordinary ABCs.
    # (_allow_reckless_class_checks() must be called directly from the two
    # check methods: it looks a fixed number of frames up.)

    def __new__(mcls, name, bases, namespace, /, **kwargs):
        if name == "Protocol" and bases == (Generic,):
            pass
        elif Protocol in bases:
            for base in bases:
                if not (base is object or base is Generic
                        or base.__name__ in _PROTO_ALLOWLIST.get(base.__module__, ())
                        or (issubclass(base, Generic)
                            and getattr(base, '_is_protocol', False))):
                    raise TypeError("Protocols can only inherit from other "
                                    f"protocols, got {base!r}")
        return super().__new__(mcls, name, bases, namespace, **kwargs)

    def __init__(cls, *args, **kwargs):
        super().__init__(*args, **kwargs)
        if getattr(cls, '_is_protocol', False):
            cls.__protocol_attrs__ = _get_protocol_attrs(cls)
            if (getattr(cls, '_is_runtime_protocol', False)
                    and cls.__name__ != 'Protocol'):
                # runtime-checkable only by inheritance (deprecated)
                cls.__typing_is_deprecated_inherited_runtime_protocol__ = True

    def __subclasscheck__(cls, other):
        if cls is Protocol:
            return type.__subclasscheck__(cls, other)
        if getattr(cls, '_is_protocol', False) and not _allow_reckless_class_checks():
            if not getattr(cls, '_is_runtime_protocol', False):
                _type_check_issubclass_arg_1(other)
                raise TypeError(_NOT_RUNTIME_MSG)
            if cls.__dict__.get('__typing_is_deprecated_inherited_runtime_protocol__'):
                _warn_inherited_runtime(cls)
            if (cls.__non_callable_proto_members__
                    and cls.__dict__.get('__subclasshook__') is _proto_hook):
                _type_check_issubclass_arg_1(other)
                listed = str(sorted(cls.__non_callable_proto_members__))[1:-1]
                raise TypeError("Protocols with non-method members don't "
                                f"support issubclass(). Non-method members: {listed}.")
        return _abc_subclasscheck(cls, other)

    def __instancecheck__(cls, instance):
        if cls is Protocol:
            return type.__instancecheck__(cls, instance)
        if not getattr(cls, '_is_protocol', False):
            # a concrete class that merely inherits from a protocol
            return _abc_instancecheck(cls, instance)
        if not getattr(cls, '_is_runtime_protocol', False):
            if not _allow_reckless_class_checks():
                raise TypeError(_NOT_RUNTIME_MSG)
        elif (cls.__dict__.get('__typing_is_deprecated_inherited_runtime_protocol__')
                and not _allow_reckless_class_checks()):
            _warn_inherited_runtime(cls)
        if _abc_instancecheck(cls, instance):
            return True
        lookup = _getattr_static or _static_lookup()
        callables_ok = getattr(cls, '__non_callable_proto_members__', None)
        if callables_ok is None:
            callables_ok = _non_callable_members(cls)
        for attr in cls.__protocol_attrs__:
            try:
                value = lookup(instance, attr)
            except AttributeError:
                return False
            # a method member explicitly set to None opts out
            if value is None and attr not in callables_ok:
                return False
        return True
        lookup = _static_lookup()
        callables_ok = cls.__dict__.get('__non_callable_proto_members__')
        if callables_ok is None:
            callables_ok = getattr(cls, '__non_callable_proto_members__', None)
            if callables_ok is None:
                callables_ok = _non_callable_members(cls)
        for attr in cls.__protocol_attrs__:
            try:
                value = lookup(instance, attr)
            except AttributeError:
                return False
            # a method member explicitly set to None opts out
            if value is None and attr not in callables_ok:
                return False
        return True


@classmethod
def _proto_hook(cls, other):
    """__subclasshook__ of protocol classes: structural subclass check."""
    if not cls.__dict__.get('_is_protocol', False):
        return NotImplemented
    other_is_protocol = issubclass(other, Generic) and getattr(other, '_is_protocol', False)
    for attr in cls.__protocol_attrs__:
        for klass in other.__mro__:
            if attr in klass.__dict__:
                if klass.__dict__[attr] is None:
                    return NotImplemented
                break
            # a sub-protocol may merely annotate the member
            if other_is_protocol and attr in _annotation_names(klass):
                break
        else:
            return NotImplemented
    return True


class Protocol(Generic, metaclass=_ProtocolMeta):
    """Base class for structural ("duck") types.

    A protocol lists the methods and attributes a type must provide::

        class Closable(Protocol):
            def close(self) -> None: ...

    Any class with a matching close() method is then a Closable for a
    static type checker, without inheriting from it.  Decorate a protocol
    with @runtime_checkable to allow isinstance() and issubclass() checks
    (which only test that the members exist).  Protocols may be generic:
    ``class Box[T](Protocol)``.
    """

    __slots__ = ()
    _is_protocol = True
    _is_runtime_protocol = False

    def __init_subclass__(cls, *args, **kwargs):
        super().__init_subclass__(*args, **kwargs)
        # a direct subclass naming Protocol as a base is itself a protocol
        if not cls.__dict__.get('_is_protocol', False):
            cls._is_protocol = any(b is Protocol for b in cls.__bases__)
        if '__subclasshook__' not in cls.__dict__:
            cls.__subclasshook__ = _proto_hook
        if cls._is_protocol and cls.__init__ is Protocol.__init__:
            cls.__init__ = _no_init_or_replace_init


def runtime_checkable(cls):
    """Allow isinstance() and issubclass() with a protocol class.

    The checks only test that the protocol's members are present, not their
    signatures or types.  issubclass() is refused for protocols with data
    members.
    """
    if (not isinstance(cls, type) or not issubclass(cls, Generic)
            or not getattr(cls, '_is_protocol', False)):
        raise TypeError('@runtime_checkable can be only applied to protocol '
                        f'classes, got {cls!r}')
    cls._is_runtime_protocol = True
    cls.__typing_is_deprecated_inherited_runtime_protocol__ = False
    cls.__non_callable_proto_members__ = _non_callable_members(cls)
    return cls


def is_protocol(tp: type, /) -> bool:
    """Tell whether *tp* is a protocol class (Protocol itself excluded)."""
    return (isinstance(tp, type) and getattr(tp, '_is_protocol', False)
            and tp is not Protocol)


def get_protocol_members(tp: type, /) -> frozenset[str]:
    """The members of a Protocol class as a frozenset of names."""
    if not is_protocol(tp):
        raise TypeError(f'{tp!r} is not a Protocol')
    return frozenset(tp.__protocol_attrs__)


# ---------------------------------------------------------------------------
# Annotated
# ---------------------------------------------------------------------------

class _AnnotatedAlias(_NotIterable, _GenericAlias, _root=True):
    """Annotated[T, x, ...]: T with metadata attached.

    Nested Annotated forms merge: the inner metadata comes first.
    """

    def __init__(self, origin, metadata):
        if isinstance(origin, _AnnotatedAlias):
            metadata = origin.__metadata__ + metadata
            origin = origin.__origin__
        super().__init__(origin, origin, name='Annotated')
        self.__metadata__ = metadata

    def copy_with(self, params):
        assert len(params) == 1
        return _AnnotatedAlias(params[0], self.__metadata__)

    def __repr__(self):
        meta = ', '.join(map(repr, self.__metadata__))
        return f"typing.Annotated[{_type_repr(self.__origin__)}, {meta}]"

    def __reduce__(self):
        return operator.getitem, (Annotated, (self.__origin__, *self.__metadata__))

    def __eq__(self, other):
        if not isinstance(other, _AnnotatedAlias):
            return NotImplemented
        return (self.__origin__ == other.__origin__
                and self.__metadata__ == other.__metadata__)

    def __hash__(self):
        return hash((self.__origin__, self.__metadata__))

    def __getattr__(self, attr):
        if attr in ('__name__', '__qualname__'):
            return 'Annotated'
        return super().__getattr__(attr)

    def __mro_entries__(self, bases):
        return (self.__origin__,)


@_tp_cache(typed=True)
def _annotated_alias(*params):
    if len(params) < 2:
        raise TypeError("Annotated[...] should be used with at least two "
                        "arguments (a type and an annotation).")
    if _is_unpacked_typevartuple(params[0]):
        raise TypeError("Annotated[...] should not be used with an "
                        "unpacked TypeVarTuple")
    origin = _type_check(params[0], "Annotated[t, ...]: t must be a type.",
                         allow_special_forms=True)
    return _AnnotatedAlias(origin, tuple(params[1:]))


def _annotated(self, params):
    if not isinstance(params, tuple):
        params = (params,)
    return _annotated_alias(*params)


Annotated = _form('Annotated', """Attaches metadata to a type.

    ``Annotated[int, ValueRange(0, 10)]`` is the type int for a type
    checker; the extra arguments are stored, in order, in ``__metadata__``
    for tools that understand them.  The first argument must be a type and
    at least one annotation must follow.  get_type_hints() drops the
    metadata unless include_extras=True.
    """, _annotated)


# ---------------------------------------------------------------------------
# Introspection helpers
# ---------------------------------------------------------------------------

def cast(typ, val):
    """Tell a type checker that *val* has type *typ*; returns *val* as is."""
    return val


def assert_type(val, typ, /):
    """Ask a type checker to confirm that *val* has type *typ*.

    At runtime this does nothing and returns *val*.
    """
    return val


def _add_type_params_to_scope(type_params, globalns, localns, is_class):
    """Make PEP 695 type parameters visible to the evaluation of string
    annotations (as copies of the namespaces)."""
    if not type_params:
        return globalns, localns
    globalns = dict(globalns)
    localns = dict(localns)
    for param in type_params:
        name = param.__name__
        if is_class and name in globalns:
            # a class attribute of the same name shadows the parameter
            continue
        globalns[name] = param
        localns.pop(name, None)
    return globalns, localns


def _evaluated_hints(annotations, globalns, localns, owner, *, is_argument, is_class,
                     format):
    hints = {}
    for name, value in annotations.items():
        if isinstance(value, str):
            value = _make_forward_ref(value, is_argument=is_argument, is_class=is_class)
        value = _eval_type(value, globalns, localns, (), format=format, owner=owner)
        hints[name] = types.NoneType if value is None else value
    return hints


def get_type_hints(obj, globalns=None, localns=None, include_extras=False,
                   *, format=None):
    """The annotations of a module, class or callable, evaluated.

    Unlike ``__annotations__``, string annotations and forward references
    are evaluated (in *globalns* and *localns*, by default the namespaces
    the object was defined in), None becomes NoneType, and for classes the
    annotations of all base classes are merged, most derived last.
    Annotated, Required, NotRequired and ReadOnly wrappers are removed
    unless *include_extras* is true.  *format* is an annotationlib.Format;
    with FORWARDREF unresolvable names stay ForwardRef objects, with STRING
    the source strings are returned.
    """
    if getattr(obj, '__no_type_check__', None):
        return {}
    alib = _alib()
    if format is None:
        format = alib.Format.VALUE

    if isinstance(obj, type):
        hints = {}
        for base in reversed(obj.__mro__):
            ann = alib.get_annotations(base, format=format)
            if format == alib.Format.STRING:
                hints.update(ann)
                continue
            if globalns is None:
                module = sys.modules.get(base.__module__, None)
                base_globals = getattr(module, '__dict__', {})
            else:
                base_globals = globalns
            base_locals = dict(vars(base)) if localns is None else localns
            if localns is None and globalns is None:
                # Historically only the module namespace was searched for
                # class annotations; to keep that precedence the module
                # namespace is passed as the locals.
                base_globals, base_locals = base_locals, base_globals
            base_globals, base_locals = _add_type_params_to_scope(
                base.__type_params__, base_globals, base_locals, True)
            hints.update(_evaluated_hints(ann, base_globals, base_locals, obj,
                                          is_argument=False, is_class=True,
                                          format=format))
        if include_extras or format == alib.Format.STRING:
            return hints
        return {k: _strip_annotations(t) for k, t in hints.items()}

    hints = alib.get_annotations(obj, format=format)
    if (not hints and not isinstance(obj, types.ModuleType) and not callable(obj)
            and not hasattr(obj, '__annotations__')
            and not hasattr(obj, '__annotate__')):
        raise TypeError(f"{obj!r} is not a module, class, or callable.")
    if format == alib.Format.STRING:
        return hints
    if globalns is None:
        if isinstance(obj, types.ModuleType):
            globalns = obj.__dict__
        else:
            target = obj
            visited = {id(target)}
            while hasattr(target, '__wrapped__'):
                target = target.__wrapped__
                if id(target) in visited:
                    raise ValueError(f'wrapper loop when unwrapping {obj!r}')
                visited.add(id(target))
            globalns = getattr(target, '__globals__', {})
    if localns is None:
        localns = globalns
    globalns, localns = _add_type_params_to_scope(
        getattr(obj, '__type_params__', ()), globalns, localns, False)
    hints = _evaluated_hints(hints, globalns, localns, obj,
                             is_argument=not isinstance(obj, types.ModuleType),
                             is_class=False, format=format)
    if include_extras:
        return hints
    return {k: _strip_annotations(t) for k, t in hints.items()}


def _strip_annotations(t):
    """Remove Annotated, Required, NotRequired and ReadOnly wrappers."""
    if isinstance(t, _AnnotatedAlias):
        return _strip_annotations(t.__origin__)
    if isinstance(t, _GenericAlias) and t.__origin__ in (Required, NotRequired, ReadOnly):
        return _strip_annotations(t.__args__[0])
    if isinstance(t, (_GenericAlias, GenericAlias, Union)):
        old = t.__args__
        new = tuple(map(_strip_annotations, old))
        if new == old:
            return t
        if isinstance(t, GenericAlias):
            return _respecialize(t, new)
        if isinstance(t, Union):
            return functools.reduce(operator.or_, new)
        return t.copy_with(new)
    return t


def get_origin(tp):
    """The unsubscripted form of a typing alias, or None.

    >>> get_origin(Dict[str, int])
    <class 'dict'>
    >>> get_origin(Annotated[int, 'unit'])
    typing.Annotated
    >>> get_origin(int | None) is Union
    True
    >>> get_origin(int) is None
    True
    """
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
    """The arguments of a typing alias, with Callable arguments regrouped.

    >>> get_args(Dict[str, int])
    (<class 'str'>, <class 'int'>)
    >>> get_args(Callable[[int], str])
    ([<class 'int'>], <class 'str'>)
    >>> get_args(int)
    ()
    """
    if isinstance(tp, _AnnotatedAlias):
        return (tp.__origin__, *tp.__metadata__)
    if isinstance(tp, (_GenericAlias, GenericAlias)):
        args = tp.__args__
        if _should_unflatten_callable_args(tp, args):
            return (list(args[:-1]), args[-1])
        return args
    if isinstance(tp, Union):
        return tp.__args__
    return ()


def is_typeddict(tp):
    """Tell whether *tp* is a TypedDict class (not an instance or alias)."""
    return isinstance(tp, _TypedDictMeta)


# assert_never() shortens the repr of the offending value to this length
_ASSERT_NEVER_REPR_MAX_LENGTH = 100


def assert_never(arg: Never, /) -> Never:
    """Mark code a type checker should prove unreachable.

    Called at runtime it raises AssertionError naming the value.
    """
    text = repr(arg)
    if len(text) > _ASSERT_NEVER_REPR_MAX_LENGTH:
        text = text[:_ASSERT_NEVER_REPR_MAX_LENGTH] + '...'
    raise AssertionError(f"Expected code to be unreachable, but got: {text}")


def _defined_directly_in(owner, obj):
    """True if *obj* (found as an attribute of class *owner*) was defined in
    the body of *owner* itself."""
    name = getattr(obj, '__name__', None)
    qualname = getattr(obj, '__qualname__', None)
    if not isinstance(name, str) or qualname != f'{owner.__qualname__}.{name}':
        return False
    return getattr(obj, '__module__', None) == owner.__module__


def no_type_check(arg):
    """Tell type checkers to ignore the annotations of a function or class.

    For a class, the functions and nested classes defined in its body are
    marked too.  get_type_hints() returns {} for marked objects.
    """
    if isinstance(arg, type):
        for name in dir(arg):
            member = getattr(arg, name)
            if not _defined_directly_in(arg, member):
                continue
            if isinstance(member, types.FunctionType):
                member.__no_type_check__ = True
            elif isinstance(member, types.MethodType):
                member.__func__.__no_type_check__ = True
            elif isinstance(member, type):
                no_type_check(member)
    try:
        arg.__no_type_check__ = True
    except TypeError:
        # builtin and extension types
        pass
    return arg


def _overload_dummy(*args, **kwds):
    """Stand-in for a function decorated with @overload."""
    raise NotImplementedError(
        "You should not call an overloaded function. A series of @overload-"
        "decorated functions outside a stub module should always be followed "
        "by an implementation that is not @overload-ed.")


# module name -> qualified name -> first line number -> overload
_overload_registry = defaultdict(functools.partial(defaultdict, dict))


def overload(func):
    """Decorator for the signature variants of an overloaded function.

    The variants are only for type checkers; the undecorated definition
    that follows them is the implementation.  get_overloads() returns the
    variants recorded for a function.
    """
    target = getattr(func, '__func__', func)
    try:
        key = target.__module__, target.__qualname__, target.__code__.co_firstlineno
    except AttributeError:
        # builtins and other objects without code: nothing to record
        return _overload_dummy
    module, qualname, line = key
    _overload_registry[module][qualname][line] = func
    return _overload_dummy


def get_overloads(func):
    """The @overload variants recorded for *func*, in definition order."""
    target = getattr(func, '__func__', func)
    by_module = _overload_registry.get(target.__module__)
    if not by_module:
        return []
    variants = by_module.get(target.__qualname__)
    if not variants:
        return []
    return list(variants.values())


def clear_overloads():
    """Forget every recorded overload."""
    _overload_registry.clear()


def final(f):
    """Mark a method as not to be overridden, or a class as not to be
    subclassed.  Sets ``__final__ = True`` on the object when possible."""
    try:
        f.__final__ = True
    except (AttributeError, TypeError):
        # read-only or slotted objects simply stay unmarked
        pass
    return f


# Type variables used in the aliases and protocols below.
T = TypeVar('T')
KT = TypeVar('KT')
VT = TypeVar('VT')
T_co = TypeVar('T_co', covariant=True)
V_co = TypeVar('V_co', covariant=True)
VT_co = TypeVar('VT_co', covariant=True)
T_contra = TypeVar('T_contra', contravariant=True)
CT_co = TypeVar('CT_co', covariant=True, bound=type)

# str or bytes, consistently within one use
AnyStr = TypeVar('AnyStr', bytes, str)


# ---------------------------------------------------------------------------
# Aliases of standard classes
# ---------------------------------------------------------------------------

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
Callable.__doc__ = """Callable[[A1, A2], R] is a callable taking A1 and A2 and returning R.

    The parameter list may also be an ellipsis (any parameters), a
    ParamSpec or a Concatenate[...] form.  Prefer collections.abc.Callable
    in new code.
    """
AbstractSet = _alias(collections.abc.Set, 1, name='AbstractSet')
MutableSet = _alias(collections.abc.MutableSet, 1)
Mapping = _alias(collections.abc.Mapping, 2)
MutableMapping = _alias(collections.abc.MutableMapping, 2)
Sequence = _alias(collections.abc.Sequence, 1)
MutableSequence = _alias(collections.abc.MutableSequence, 1)
Tuple = _TupleType(tuple, -1, inst=False, name='Tuple')
Tuple.__doc__ = """Tuple[X, Y] is a pair of an X and a Y; Tuple[int, ...] any number of ints.

    Tuple[()] is the empty tuple.  Prefer the builtin tuple in new code.
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
Generator = _alias(collections.abc.Generator, 3,
                   defaults=(types.NoneType, types.NoneType))
AsyncGenerator = _alias(collections.abc.AsyncGenerator, 2, defaults=(types.NoneType,))
Type = _alias(type, 1, inst=False, name='Type')
Type.__doc__ = """Type[C] is the class C or one of its subclasses (not an instance).

    Prefer the builtin type in new code: ``type[C]``.
    """


# ---------------------------------------------------------------------------
# One-method protocols
# ---------------------------------------------------------------------------

@runtime_checkable
class SupportsInt(Protocol):
    """Has an __int__ method."""
    __slots__ = ()

    @abstractmethod
    def __int__(self) -> int:
        pass


@runtime_checkable
class SupportsFloat(Protocol):
    """Has a __float__ method."""
    __slots__ = ()

    @abstractmethod
    def __float__(self) -> float:
        pass


@runtime_checkable
class SupportsComplex(Protocol):
    """Has a __complex__ method."""
    __slots__ = ()

    @abstractmethod
    def __complex__(self) -> complex:
        pass


@runtime_checkable
class SupportsBytes(Protocol):
    """Has a __bytes__ method."""
    __slots__ = ()

    @abstractmethod
    def __bytes__(self) -> bytes:
        pass


@runtime_checkable
class SupportsIndex(Protocol):
    """Has an __index__ method."""
    __slots__ = ()

    @abstractmethod
    def __index__(self) -> int:
        pass


@runtime_checkable
class SupportsAbs[T](Protocol):
    """Has an __abs__ method returning T."""
    __slots__ = ()

    @abstractmethod
    def __abs__(self) -> T:
        pass


@runtime_checkable
class SupportsRound[T](Protocol):
    """Has a __round__ method returning T."""
    __slots__ = ()

    @abstractmethod
    def __round__(self, ndigits: int = 0) -> T:
        pass


# ---------------------------------------------------------------------------
# NamedTuple
# ---------------------------------------------------------------------------

def _make_nmtuple(name, fields, annotate_func, module, defaults=()):
    cls = collections.namedtuple(name, fields, defaults=defaults, module=module)
    cls.__annotate__ = annotate_func
    cls.__new__.__annotate__ = annotate_func
    return cls


def _make_eager_annotate(types_by_name):
    """An __annotate__ function for annotations known up front."""
    checked = {name: _type_check(tp, f"field {name} annotation must be a type")
               for name, tp in types_by_name.items()}

    def annotate(format):
        if format == _FMT_VALUE or format == _FMT_FORWARDREF:
            return checked
        if format == _FMT_STRING:
            return _alib().annotations_to_string(types_by_name)
        raise NotImplementedError(format)
    return annotate


def _make_lazy_annotate(original):
    """Wrap a compiler-generated __annotate__ so that every evaluation is
    type checked like the eager case."""
    alib = _alib()

    def annotate(format):
        annos = alib.call_annotate_function(original, format)
        if format == _FMT_STRING:
            return annos
        return {name: _type_check(tp, f"field {name} annotation must be a type")
                for name, tp in annos.items()}
    return annotate


# Names a NamedTuple class body may not define, and names it defines that
# are not copied to the generated class.
_prohibited = frozenset({'__new__', '__init__', '__slots__', '__getnewargs__',
                         '_fields', '_field_defaults',
                         '_make', '_replace', '_asdict', '_source'})

_special = frozenset({'__module__', '__name__', '__annotations__', '__annotate__',
                      '__annotate_func__', '__annotations_cache__'})


def _namedtuple_fields(ns):
    """Field names and the __annotate__ function of a NamedTuple body."""
    if '__annotations__' in ns:
        annos = ns['__annotations__']
        return list(annos), _make_eager_annotate(annos)
    if not any(k in ns for k in _ANNOTATION_KEYS):
        return [], lambda format: {}
    alib = _alib()
    original = alib.get_annotate_from_class_namespace(ns)
    if original is None:
        return [], lambda format: {}
    annos = alib.call_annotate_function(original, alib.Format.FORWARDREF)
    # checked now as well, like annotations known up front
    for tp in annos.values():
        _type_check(tp, "field annotation must be a type")
    return list(annos), _make_lazy_annotate(original)


class NamedTupleMeta(type):
    def __new__(cls, typename, bases, ns):
        assert _NamedTuple in bases
        if "__classcell__" in ns:
            raise TypeError("uses of super() and __class__ are unsupported "
                            "in methods of NamedTuple subclasses")
        for base in bases:
            if base is not _NamedTuple and base is not Generic:
                raise TypeError('can only inherit from a NamedTuple type and Generic')
        bases = tuple(tuple if b is _NamedTuple else b for b in bases)
        names, annotate = _namedtuple_fields(ns)
        with_default = []
        for name in names:
            if name in ns:
                with_default.append(name)
            elif with_default:
                plural = 's' if len(with_default) > 1 else ''
                raise TypeError(f"Non-default namedtuple field {name} cannot "
                                f"follow default field{plural} "
                                f"{', '.join(with_default)}")
        nm_tpl = _make_nmtuple(typename, names, annotate,
                               defaults=[ns[n] for n in with_default],
                               module=ns['__module__'])
        nm_tpl.__bases__ = bases
        generic = Generic in bases
        if generic:
            nm_tpl.__class_getitem__ = classmethod(_generic_class_getitem)
        fields = nm_tpl._fields
        for key, val in ns.items():
            if key in _prohibited:
                raise AttributeError("Cannot overwrite NamedTuple attribute " + key)
            if key in _special:
                continue
            if key not in fields:
                setattr(nm_tpl, key, val)
            try:
                set_name = type(val).__set_name__
            except AttributeError:
                continue
            try:
                set_name(val, nm_tpl, key)
            except BaseException as exc:
                exc.add_note(f"Error calling __set_name__ on {type(val).__name__!r} "
                             f"instance {key!r} in {typename!r}")
                raise
        if generic:
            # the class was not created with these bases, so Generic's
            # hook has not run yet
            nm_tpl.__init_subclass__()
        return nm_tpl


def NamedTuple(typename, fields, /):
    """Typed version of collections.namedtuple().

    Class form::

        class Employee(NamedTuple):
            name: str
            id: int = 3

    Functional form: ``Employee = NamedTuple('Employee', [('name', str),
    ('id', int)])``.  The field types are available from
    ``__annotations__``; defaults, methods and docstrings may be given in the
    class form, which may also be generic.
    """
    checked = {name: _type_check(tp, f"field {name} annotation must be a type")
               for name, tp in fields}
    nt = _make_nmtuple(typename, checked.keys(), _make_eager_annotate(checked),
                       module=_caller())
    nt.__orig_bases__ = (NamedTuple,)
    return nt


_NamedTuple = type.__new__(NamedTupleMeta, 'NamedTuple', (), {})


def _namedtuple_mro_entries(bases):
    assert NamedTuple in bases
    return (_NamedTuple,)


NamedTuple.__mro_entries__ = _namedtuple_mro_entries


# ---------------------------------------------------------------------------
# TypedDict
# ---------------------------------------------------------------------------

def _get_typeddict_qualifiers(annotation_type):
    """Yield Required/NotRequired/ReadOnly for each wrapper around the type
    (Annotated layers are looked through)."""
    t = annotation_type
    while True:
        if isinstance(t, _AnnotatedAlias):
            t = t.__origin__
            continue
        if isinstance(t, _GenericAlias) and t.__origin__ in (Required, NotRequired, ReadOnly):
            yield t.__origin__
            t = t.__args__[0]
            continue
        return


class _NoExtraItemsType:
    """Type of NoExtraItems, the marker for a TypedDict that declares no
    extra_items (a singleton)."""

    __slots__ = ()
    _instance = None

    def __new__(cls):
        inst = cls._instance
        if inst is None:
            inst = cls._instance = super().__new__(cls)
        return inst

    def __init_subclass__(cls, /, *args, **kwargs):
        raise TypeError(f"type '{__name__}.NoExtraItems' is not an acceptable base type")

    def __repr__(self):
        return 'typing.NoExtraItems'

    def __reduce__(self):
        return 'NoExtraItems'


NoExtraItems = _NoExtraItemsType()

_TYPEDDICT_MSG = "TypedDict('Name', {f0: t0, f1: t1, ...}); each t must be a type"


class _TypedDictMeta(type):
    def __new__(cls, name, bases, ns, total=True, closed=None,
                extra_items=NoExtraItems):
        """Create a TypedDict class: a plain dict subclass carrying the key
        sets (__required_keys__, __optional_keys__, __readonly_keys__,
        __mutable_keys__) computed from its own and its bases' annotations."""
        for base in bases:
            if type(base) is not _TypedDictMeta and base is not Generic:
                raise TypeError('cannot inherit from both a TypedDict type '
                                'and a non-TypedDict base class')
        if closed is not None and extra_items is not NoExtraItems:
            raise TypeError(f"Cannot combine closed={closed!r} and extra_items")

        generic = any(issubclass(b, Generic) for b in bases)
        explicit = ns.pop('__annotations__', None)
        tp_dict = type.__new__(_TypedDictMeta, name,
                               (Generic, dict) if generic else (dict,), ns)
        if not hasattr(tp_dict, '__orig_bases__'):
            tp_dict.__orig_bases__ = bases
        module = tp_dict.__module__

        own_annotate = None
        if explicit is not None:
            own = explicit
        elif any(k in ns for k in _ANNOTATION_KEYS):
            alib = _alib()
            own_annotate = alib.get_annotate_from_class_namespace(ns)
            if own_annotate is None:
                own = {}
            else:
                own = alib.call_annotate_function(own_annotate, alib.Format.FORWARDREF,
                                                  owner=tp_dict)
        else:
            own = {}
        checked = {k: _type_check(v, _TYPEDDICT_MSG, owner=tp_dict, module=module)
                   for k, v in own.items()}

        required = set()
        optional = set()
        readonly = set()
        mutable = set()
        for base in bases:
            vars_ = base.__dict__
            base_required = vars_.get('__required_keys__', ())
            base_optional = vars_.get('__optional_keys__', ())
            required.update(base_required)
            optional.difference_update(base_required)
            optional.update(base_optional)
            required.difference_update(base_optional)
            readonly.update(vars_.get('__readonly_keys__', ()))
            mutable.update(vars_.get('__mutable_keys__', ()))
        for key, tp in checked.items():
            quals = set(_get_typeddict_qualifiers(tp))
            if Required in quals:
                is_required = True
            elif NotRequired in quals:
                is_required = False
            else:
                is_required = total
            (required if is_required else optional).add(key)
            (optional if is_required else required).discard(key)
            if ReadOnly in quals:
                if key in mutable:
                    raise TypeError(f"Cannot override mutable key {key!r} "
                                    "with read-only key")
                readonly.add(key)
            else:
                mutable.add(key)
                readonly.discard(key)

        def __annotate__(format):
            alib = _alib()
            merged = {}
            for base in bases:
                if base is Generic:
                    continue
                base_annotate = base.__annotate__
                if base_annotate is None:
                    continue
                merged.update(alib.call_annotate_function(base_annotate, format,
                                                          owner=base))
            if own_annotate is not None:
                mine = alib.call_annotate_function(own_annotate, format, owner=tp_dict)
                if format != alib.Format.STRING:
                    mine = {k: _type_check(v, _TYPEDDICT_MSG, module=module)
                            for k, v in mine.items()}
            elif format == alib.Format.STRING:
                mine = alib.annotations_to_string(own)
            elif format in (alib.Format.FORWARDREF, alib.Format.VALUE):
                mine = checked
            else:
                raise NotImplementedError(format)
            merged.update(mine)
            return merged

        tp_dict.__annotate__ = __annotate__
        tp_dict.__required_keys__ = frozenset(required)
        tp_dict.__optional_keys__ = frozenset(optional)
        tp_dict.__readonly_keys__ = frozenset(readonly)
        tp_dict.__mutable_keys__ = frozenset(mutable)
        tp_dict.__total__ = total
        tp_dict.__closed__ = closed
        tp_dict.__extra_items__ = extra_items
        return tp_dict

    # Calling a TypedDict class makes a plain dict.
    __call__ = dict

    def __subclasscheck__(cls, other):
        raise TypeError('TypedDict does not support instance and class checks')

    __instancecheck__ = __subclasscheck__


def TypedDict(typename, fields, /, *, total=True, closed=None,
              extra_items=NoExtraItems):
    """A dict type with a fixed set of string keys, each with its own type.

    Class form::

        class Point2D(TypedDict):
            x: int
            y: int
            label: NotRequired[str]

    Functional form: ``Point2D = TypedDict('Point2D', {'x': int, 'y': int})``.
    Instances are plain dicts; the class only describes them for type
    checkers.  ``total=False`` makes keys optional by default (Required and
    NotRequired override that per key), ReadOnly marks keys that may not
    be changed, and *closed* / *extra_items* describe keys beyond those
    declared.  isinstance() and issubclass() are not supported.
    """
    ns = {'__annotations__': dict(fields)}
    module = _caller()
    if module is not None:
        # needed for pickling the class by reference
        ns['__module__'] = module
    td = _TypedDictMeta(typename, (), ns, total=total, closed=closed,
                        extra_items=extra_items)
    td.__orig_bases__ = (TypedDict,)
    return td


_TypedDict = type.__new__(_TypedDictMeta, 'TypedDict', (), {})
TypedDict.__mro_entries__ = lambda bases: (_TypedDict,)


def _typeddict_key_form(name, doc):
    @_tp_cache
    def wrap(self, parameters):
        item = _type_check(parameters, f'{self._name} accepts only a single type.')
        return _GenericAlias(self, (item,))
    return _form(name, doc, wrap)


Required = _typeddict_key_form('Required', """Marks a TypedDict key as required.

    Useful in a TypedDict declared with total=False.
    """)

NotRequired = _typeddict_key_form('NotRequired', """Marks a TypedDict key as possibly missing.

    The opposite of Required; useful in a total TypedDict.
    """)

ReadOnly = _typeddict_key_form('ReadOnly', """Marks a TypedDict key as read-only.

    Type checkers reject assignments to such keys; subclasses may narrow
    their types.
    """)


# ---------------------------------------------------------------------------
# NewType
# ---------------------------------------------------------------------------

class NewType:
    """A distinct name for an existing type, for the type checker only.

    ``UserId = NewType('UserId', int)`` creates a callable that returns its
    argument unchanged, while a type checker treats UserId as a subtype of
    int that plain ints are not compatible with.
    """

    __call__ = _idfunc

    def __init__(self, name, tp):
        self.__qualname__ = name
        self.__name__ = name.rpartition('.')[2]
        self.__supertype__ = tp
        module = _caller()
        if module != 'typing':
            self.__module__ = module

    def __mro_entries__(self, bases):
        # subclassing a NewType is an error; this produces a helpful message
        base_name = self.__name__

        class _Refuse:
            def __init_subclass__(cls):
                sub = cls.__name__
                raise TypeError(
                    f"Cannot subclass an instance of NewType. Perhaps you were "
                    f"looking for: `{sub} = NewType({sub!r}, {base_name})`")
        return (_Refuse,)

    def __repr__(self):
        return f'{self.__module__}.{self.__qualname__}'

    def __reduce__(self):
        return self.__qualname__

    def __or__(self, other):
        return Union[self, other]

    def __ror__(self, other):
        return Union[other, self]


# Python 2 compatibility name.
Text = str

# True only while a static type checker analyses the code.
TYPE_CHECKING = False


# ---------------------------------------------------------------------------
# I/O
# ---------------------------------------------------------------------------

class IO(Generic[AnyStr]):
    """The interface of file objects, generic over str or bytes.

    TextIO and BinaryIO are the two concrete variants.  Annotate parameters
    with these when any file-like object is acceptable.
    """

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
    def truncate(self, size: int = None) -> int:
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
    """A file object for bytes."""

    __slots__ = ()

    @abstractmethod
    def write(self, s: bytes | bytearray) -> int:
        pass

    @abstractmethod
    def __enter__(self) -> BinaryIO:
        pass


class TextIO(IO[str]):
    """A file object for text."""

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
    def newlines(self) -> object:
        pass

    @abstractmethod
    def __enter__(self) -> TextIO:
        pass


# ---------------------------------------------------------------------------
# Decorators and functions for type checkers
# ---------------------------------------------------------------------------

def reveal_type[T](obj: T, /) -> T:
    """Ask a type checker to report the inferred type of *obj*.

    At runtime the runtime type is printed to stderr and *obj* returned.
    """
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
    """Mark a decorator, base class or metaclass as creating dataclass-like
    classes (PEP 681).

    The arguments describe the defaults of the generated classes for a
    type checker; at runtime they are recorded in the
    ``__dataclass_transform__`` attribute of the decorated object.
    """
    def mark(cls_or_fn):
        cls_or_fn.__dataclass_transform__ = {
            "eq_default": eq_default,
            "order_default": order_default,
            "kw_only_default": kw_only_default,
            "frozen_default": frozen_default,
            "field_specifiers": field_specifiers,
            "kwargs": kwargs,
        }
        return cls_or_fn
    return mark


type _Func = Callable[..., Any]


def override[F: _Func](method: F, /) -> F:
    """Mark a method as overriding a method of a base class.

    A type checker reports an error when no base class has such a method.
    At runtime ``__override__ = True`` is set on the method when possible.
    """
    try:
        method.__override__ = True
    except (AttributeError, TypeError):
        pass
    return method


def disjoint_base[T](cls: type[T], /) -> type[T]:
    """Mark a class as a disjoint base (PEP 800): no class can inherit from
    it and from another disjoint base unrelated to it.

    At runtime ``__disjoint_base__ = True`` is set on the class.
    """
    cls.__disjoint_base__ = True
    return cls


# ---------------------------------------------------------------------------
# Names created on first access
# ---------------------------------------------------------------------------

_byte_string_alias = None


def _lazy_byte_string():
    global _byte_string_alias
    import warnings
    warnings._deprecated("typing.ByteString", remove=(3, 17))
    if _byte_string_alias is None:
        with warnings.catch_warnings(action="ignore", category=DeprecationWarning):
            from collections.abc import ByteString as origin
        _byte_string_alias = _DeprecatedGenericAlias(
            origin, 0, removal_version=(3, 17), name="ByteString")
    return _byte_string_alias


def _lazy_re_alias(name):
    import re
    return _alias(getattr(re, name), 1)


def _lazy_context_manager(name):
    import contextlib
    return _alias(getattr(contextlib, 'Abstract' + name), 2, name=name,
                  defaults=(bool | None,))


def _lazy_forward_ref(name):
    return _alib().ForwardRef


def _lazy_collect_parameters(name):
    import warnings
    warnings.warn(
        "The private _collect_parameters function is deprecated and will be"
        " removed in a future version of Python. Private functions may change"
        " or disappear without notice.",
        category=DeprecationWarning, stacklevel=3)
    return _collect_type_parameters


_LAZY = {
    'ForwardRef': _lazy_forward_ref,
    'Pattern': _lazy_re_alias,
    'Match': _lazy_re_alias,
    'ContextManager': _lazy_context_manager,
    'AsyncContextManager': _lazy_context_manager,
    '_collect_parameters': _lazy_collect_parameters,
}


def __getattr__(attr):
    """Module attributes that need an import, or a warning, on first use."""
    if attr == 'ByteString':
        # deprecated: warn on every access, so never stored in globals()
        return _lazy_byte_string()
    make = _LAZY.get(attr)
    if make is None:
        raise AttributeError(f"module {__name__!r} has no attribute {attr!r}")
    value = make(attr)
    globals()[attr] = value
    return value
