"""Mock objects for tests.

This module offers configurable stand-in objects (Mock, MagicMock,
AsyncMock, ThreadingMock and their non-callable variants), the ``patch``
family of decorators/context managers that swap such objects into place
for the duration of a test, and assorted helpers (``create_autospec``,
``call``, ``ANY``, ``sentinel``, ``mock_open``, ``seal`` ...).

Written for this project from the documented behaviour of
``unittest.mock``.
"""

__all__ = (
    'Mock',
    'MagicMock',
    'patch',
    'sentinel',
    'DEFAULT',
    'ANY',
    'call',
    'create_autospec',
    'AsyncMock',
    'ThreadingMock',
    'FILTER_DIR',
    'NonCallableMock',
    'NonCallableMagicMock',
    'mock_open',
    'PropertyMock',
    'seal',
)

import annotationlib as _annotationlib
import builtins
import contextlib
import inspect
import io
import pprint
import sys
import threading
from dataclasses import fields as _dc_fields, is_dataclass as _is_dataclass
from functools import partial as _partial, wraps as _wraps
from types import (BuiltinFunctionType, CodeType, FunctionType,
                   MethodType, ModuleType)
from unittest.util import safe_repr

# Both of these are spellings of "plain Python function"; builtin
# functions/methods are not, they have no introspectable signature.
FunctionTypes = (
    type(lambda: None),
    type(object.__init__),
)

FILTER_DIR = True

# Names of builtins: patching these on a module is always allowed even
# though the module has no such global (it would normally see the builtin).
_builtins = frozenset(name for name in dir(builtins)
                      if not name.startswith('_'))


class InvalidSpecError(Exception):
    """Raised when a mock is used as the spec of another mock."""


# ---------------------------------------------------------------------------
# Sentinels
# ---------------------------------------------------------------------------

class _SentinelObject:
    """A unique, named marker object."""

    def __init__(self, name):
        self.name = name

    def __repr__(self):
        return 'sentinel.' + self.name

    def __reduce__(self):
        # Re-created by looking the attribute up on the module-level
        # ``sentinel`` registry, which preserves identity.
        return 'sentinel.' + self.name


class _SentinelRegistry:
    """Attribute access creates (once) and returns a named sentinel."""

    def __init__(self):
        self._registry = {}

    def __getattr__(self, name):
        if name == '__bases__':
            # keep help()/pydoc from treating this as a class
            raise AttributeError(name)
        try:
            return self._registry[name]
        except KeyError:
            obj = _SentinelObject(name)
            return self._registry.setdefault(name, obj)

    def __reduce__(self):
        return 'sentinel'


sentinel = _SentinelRegistry()

DEFAULT = sentinel.DEFAULT
_missing = sentinel.MISSING
_deleted = sentinel.DELETED

_timeout_unset = sentinel.TIMEOUT_UNSET


# ---------------------------------------------------------------------------
# Small predicates
# ---------------------------------------------------------------------------

def _is_instance_mock(obj):
    # Deliberately looks at type(), not obj.__class__ (which a spec'd mock
    # fakes).
    return issubclass(type(obj), NonCallableMock)


def _is_exception(obj):
    if isinstance(obj, BaseException):
        return True
    return isinstance(obj, type) and issubclass(obj, BaseException)


def _extract_mock(obj):
    """Autospecced functions carry their mock in a ``mock`` attribute."""
    if isinstance(obj, FunctionTypes) and hasattr(obj, 'mock'):
        return obj.mock
    return obj


def _callable(obj):
    if isinstance(obj, type):
        return True
    if isinstance(obj, (staticmethod, classmethod, MethodType)):
        return _callable(obj.__func__)
    return getattr(obj, '__call__', None) is not None


def _is_list(obj):
    # exact types only: subclasses may be valid specs in their own right
    return type(obj) in (list, tuple)


def _instance_callable(obj):
    """Would instances of *obj* (a class) be callable?"""
    if not isinstance(obj, type):
        return getattr(obj, '__call__', None) is not None
    for klass in (obj,) + obj.__mro__:
        if klass.__dict__.get('__call__') is not None:
            return True
    return False


def _unwrap_callable(obj):
    while isinstance(obj, (classmethod, staticmethod)):
        obj = obj.__func__
    return obj


def _is_async_func(obj):
    if getattr(obj, '__code__', None) is None:
        return False
    return inspect.iscoroutinefunction(obj)


def _is_async_obj(obj):
    if _is_instance_mock(obj) and not isinstance(obj, AsyncMock):
        return False
    if hasattr(obj, '__func__'):
        obj = getattr(obj, '__func__')
    return inspect.iscoroutinefunction(obj) or inspect.isawaitable(obj)


def _static_lookup(obj, name):
    """Find attribute *name* of *obj* without invoking descriptors or
    __getattr__ hooks; returns _missing if not found that way."""
    try:
        own = object.__getattribute__(obj, '__dict__')
    except AttributeError:
        own = None
    if isinstance(obj, type):
        search = obj.__mro__
    else:
        if isinstance(own, dict) and name in own:
            return own[name]
        search = type(obj).__mro__
    for klass in search:
        namespace = klass.__dict__
        if name in namespace:
            return namespace[name]
    return _missing


def _is_async_attribute(value):
    if value is _missing:
        return False
    if _is_instance_mock(value):
        return isinstance(value, AsyncMock)
    value = _unwrap_callable(value)
    if isinstance(value, (FunctionType, _partial)):
        return inspect.iscoroutinefunction(value)
    return False


def _try_iter(obj):
    """Turn an iterable side effect into an iterator; leave others alone."""
    if obj is None or _is_exception(obj) or _callable(obj):
        return obj
    try:
        return iter(obj)
    except TypeError:
        # not iterable either: complain only when it gets used
        return obj


# ---------------------------------------------------------------------------
# Signature support (used by spec/autospec)
# ---------------------------------------------------------------------------

def _get_signature_object(func, as_instance, eat_self):
    """Return (callable, Signature) usable to validate calls to *func*.

    *as_instance* means "the spec describes an instance of the class", in
    which case calls go to ``__call__``.  *eat_self* drops the first
    parameter (an unbound method being called through an instance).
    Returns None when no signature can be determined.
    """
    if isinstance(func, type) and not as_instance:
        # calling the class runs __init__ (whose self is not passed)
        try:
            func = func.__init__
        except AttributeError:
            return None
        eat_self = True
    elif isinstance(func, (classmethod, staticmethod)):
        if isinstance(func, classmethod):
            eat_self = True
        func = func.__func__
    elif isinstance(func, MethodType):
        # already bound: self is not part of the signature
        eat_self = False
    elif not isinstance(func, FunctionTypes + (_partial,)):
        try:
            func = func.__call__
        except AttributeError:
            return None
        if isinstance(func, MethodType):
            eat_self = False
    try:
        sig = inspect.signature(
            func, annotation_format=_annotationlib.Format.FORWARDREF)
    except (ValueError, TypeError):
        return None
    if eat_self:
        sig = _drop_first_parameter(sig)
    return func, sig


def _drop_first_parameter(sig):
    params = list(sig.parameters.values())
    if params and params[0].kind in (inspect.Parameter.POSITIONAL_ONLY,
                                     inspect.Parameter.POSITIONAL_OR_KEYWORD):
        params = params[1:]
    return sig.replace(parameters=params)


def _check_sig(spec, mock, skipfirst, instance=False):
    found = _get_signature_object(spec, instance, skipfirst)
    if found is None:
        return
    func, sig = found

    def checksig(self, /, *args, **kwargs):
        sig.bind(*args, **kwargs)
    _copy_func_details(func, checksig)
    type(mock)._mock_check_sig = checksig
    type(mock).__signature__ = sig


def _copy_func_details(func, funcopy):
    for name in ('__name__', '__doc__', '__text_signature__',
                 '__module__', '__defaults__', '__kwdefaults__'):
        try:
            setattr(funcopy, name, getattr(func, name))
        except (AttributeError, TypeError):
            pass


def _skip_first_param(spec, entry, is_type):
    """Should calls to attribute *entry* of *spec* drop their first arg?

    True for plain functions found on a class when the spec describes an
    instance (they become bound methods), False for static methods and
    class methods, and for anything found in an instance's own dict.
    """
    if not isinstance(spec, type):
        if entry in getattr(spec, '__dict__', {}):
            # instance attribute: no binding happens
            return False
        spec = type(spec)
        is_type = False
    for klass in spec.__mro__:
        found = klass.__dict__.get(entry, DEFAULT)
        if found is DEFAULT:
            continue
        if isinstance(found, (staticmethod, classmethod)):
            return False
        if isinstance(found, FunctionTypes):
            # functions on a class: the instance case binds self
            return is_type
        return False
    return is_type


_must_skip = _skip_first_param


# ---------------------------------------------------------------------------
# Formatting of calls
# ---------------------------------------------------------------------------

def _format_call_signature(name, args, kwargs):
    pieces = [repr(arg) for arg in args]
    pieces.extend('%s=%r' % item for item in kwargs.items())
    return '%s(%s)' % (name, ', '.join(pieces))


class _CallList(list):
    """List of calls whose ``in`` also matches contiguous sub-sequences."""

    def __contains__(self, value):
        if not isinstance(value, list):
            return list.__contains__(self, value)
        width = len(value)
        for start in range(len(self) - width + 1):
            if self[start:start + width] == value:
                return True
        return False

    def __repr__(self):
        return pprint.pformat(list(self))


class _AnyComparer(list):
    """Membership test that puts the *expected* object on the left of ==,
    so that ANY (and other permissive comparers) inside it get the first
    say."""

    def __contains__(self, item):
        for candidate in self:
            if len(item) != len(candidate):
                continue
            if all(want == have for want, have in zip(item, candidate)):
                return True
        return False


class _ANY:
    """Compares equal to everything."""

    def __eq__(self, other):
        return True

    def __ne__(self, other):
        return False

    __hash__ = object.__hash__

    def __repr__(self):
        return '<ANY>'


ANY = _ANY()


def _split_call_tuple(value):
    """Normalise one of the many accepted call spellings to
    (name, args, kwargs); name is None when not given."""
    size = len(value)
    if size == 0:
        return None, (), {}
    if size == 3:
        name, args, kwargs = value
        return name, args, kwargs
    if size == 1:
        (only,) = value
        if isinstance(only, str):
            return only, (), {}
        if isinstance(only, tuple):
            return None, only, {}
        return None, (), only
    if size == 2:
        first, second = value
        if isinstance(first, str):
            if isinstance(second, tuple):
                return first, second, {}
            return first, (), second
        return None, first, second
    raise ValueError('bad call tuple')


class _Call(tuple):
    """A recorded (or expected) call.

    Stored as a 2-tuple ``(args, kwargs)`` or a 3-tuple
    ``(name, args, kwargs)``.  Comparison is lenient about which form the
    other side uses.  Attribute access and calling build up chained calls,
    e.g. ``call.foo(1).bar()``.
    """

    def __new__(cls, value=(), name='', parent=None, two=False,
                from_kall=True):
        found_name, args, kwargs = _split_call_tuple(value)
        if found_name is None:
            found_name = name if isinstance(name, str) else ''
        if two:
            return tuple.__new__(cls, (args, kwargs))
        return tuple.__new__(cls, (found_name, args, kwargs))

    def __init__(self, value=(), name=None, parent=None, two=False,
                 from_kall=True):
        self._mock_name = name
        self._mock_parent = parent
        self._mock_from_kall = from_kall

    def _parts(self):
        if len(self) == 2:
            args, kwargs = self
            return '', args, kwargs
        return tuple(self)

    def __eq__(self, other):
        try:
            other_len = len(other)
        except TypeError:
            return NotImplemented
        mine_name, mine_args, mine_kwargs = self._parts()
        my_parent = getattr(self, '_mock_parent', None)
        their_parent = getattr(other, '_mock_parent', None)
        if (my_parent is not None and their_parent is not None
                and my_parent != their_parent):
            return False
        if other_len > 3:
            return False
        try:
            their_name, their_args, their_kwargs = _split_call_tuple(other)
        except ValueError:
            return False
        if their_name is None:
            their_name = ''
        if mine_name and their_name != mine_name:
            return False
        # the *other* side goes first so that ANY there wins
        return (their_args, their_kwargs) == (mine_args, mine_kwargs)

    def __ne__(self, other):
        result = _Call.__eq__(self, other)
        if result is NotImplemented:
            return result
        return not result

    __hash__ = None

    def __call__(self, /, *args, **kwargs):
        if self._mock_name is None:
            return _Call(('', args, kwargs), name='()')
        return _Call((self._mock_name, args, kwargs),
                     name=self._mock_name + '()', parent=self)

    def __getattr__(self, attr):
        if self._mock_name is None:
            return _Call(name=attr, from_kall=False)
        return _Call(name='%s.%s' % (self._mock_name, attr), parent=self,
                     from_kall=False)

    def __getattribute__(self, attr):
        # tuple methods (count, index, ...) are treated as call names
        if attr in tuple.__dict__:
            raise AttributeError
        return tuple.__getattribute__(self, attr)

    def count(self, /, *args, **kwargs):
        return self.__getattr__('count')(*args, **kwargs)

    def index(self, /, *args, **kwargs):
        return self.__getattr__('index')(*args, **kwargs)

    def _get_call_arguments(self):
        _, args, kwargs = self._parts()
        return args, kwargs

    @property
    def args(self):
        return self._get_call_arguments()[0]

    @property
    def kwargs(self):
        return self._get_call_arguments()[1]

    def __repr__(self):
        if not self._mock_from_kall:
            label = self._mock_name or 'call'
            if label.startswith('()'):
                label = 'call' + label
            return label
        name, args, kwargs = self._parts()
        if len(self) == 2 or not name:
            label = 'call'
        elif name.startswith('()'):
            label = 'call' + name
        else:
            label = 'call.' + name
        return _format_call_signature(label, args, kwargs)

    def call_list(self):
        """The chain of calls this call is the last of, as a list."""
        chain = []
        node = self
        while node is not None:
            if node._mock_from_kall:
                chain.append(node)
            node = node._mock_parent
        chain.reverse()
        return _CallList(chain)


call = _Call(from_kall=False)


# ---------------------------------------------------------------------------
# Magic method tables
# ---------------------------------------------------------------------------

def _dunder(names):
    return {'__%s__' % name for name in names.split()}


_plain_magic_names = (
    'lt le gt ge eq ne getitem setitem delitem len contains iter hash str '
    'sizeof enter exit divmod rdivmod neg pos abs invert complex int float '
    'index round trunc floor ceil bool next fspath aiter'
)
_numeric_names = 'add sub mul matmul truediv floordiv mod lshift rshift and xor or pow'

_magics = (_dunder(_plain_magic_names)
           | _dunder(_numeric_names)
           | _dunder(' '.join('i' + n for n in _numeric_names.split()))
           | _dunder(' '.join('r' + n for n in _numeric_names.split())))

# Supported, but not configured by default on MagicMock.
_non_defaults = {
    '__get__', '__set__', '__delete__', '__reversed__', '__missing__',
    '__reduce__', '__reduce_ex__', '__getinitargs__', '__getnewargs__',
    '__getstate__', '__setstate__', '__getformat__', '__repr__', '__dir__',
    '__subclasses__', '__format__', '__getnewargs_ex__',
}

# Never allowed to be set on a mock.
_unsupported_magics = {
    '__getattr__', '__setattr__', '__init__', '__new__', '__prepare__',
    '__instancecheck__', '__subclasscheck__', '__del__',
}

_async_method_magics = {'__aenter__', '__aexit__', '__anext__'}
_sync_async_magics = {'__aiter__'}
_async_magics = _async_method_magics | _sync_async_magics

_all_sync_magics = _magics | _non_defaults
_all_magics = _all_sync_magics | _async_magics


def _is_magic(name):
    return len(name) >= 4 and name[:2] == '__' and name[-2:] == '__'


def _get_method(name, func):
    """Wrap *func* so it can live on the per-mock class as *name*."""
    def method(self, /, *args, **kw):
        return func(self, *args, **kw)
    method.__name__ = name
    return method


# Fixed default return values of MagicMock's configured magic methods.
_return_values = {
    '__lt__': NotImplemented,
    '__gt__': NotImplemented,
    '__le__': NotImplemented,
    '__ge__': NotImplemented,
    '__int__': 1,
    '__contains__': False,
    '__len__': 0,
    '__exit__': False,
    '__complex__': 1j,
    '__float__': 1.0,
    '__bool__': True,
    '__index__': 1,
    '__aexit__': False,
}


def _mock_fspath(mock):
    return '%s/%s/%s' % (type(mock).__name__, mock._extract_mock_name(),
                         id(mock))


# Default return values computed from the owning mock.
_calculate_return_value = {
    '__hash__': lambda mock: object.__hash__(mock),
    '__str__': lambda mock: object.__str__(mock),
    '__sizeof__': lambda mock: object.__sizeof__(mock),
    '__fspath__': _mock_fspath,
}


def _eq_side_effect(mock):
    def eq(other):
        configured = mock.__eq__._mock_return_value
        if configured is not DEFAULT:
            return configured
        if mock is other:
            return True
        return NotImplemented
    return eq


def _ne_side_effect(mock):
    def ne(other):
        configured = mock.__ne__._mock_return_value
        if configured is not DEFAULT:
            return configured
        if mock is other:
            return False
        return NotImplemented
    return ne


def _iter_side_effect(mock):
    def iterate():
        configured = mock.__iter__._mock_return_value
        if configured is DEFAULT:
            return iter([])
        # a fresh iterator each time, over whatever was configured
        return iter(configured)
    return iterate


def _aiter_side_effect(mock):
    def aiterate():
        configured = mock.__aiter__._mock_return_value
        if configured is DEFAULT:
            return _AsyncIterator(iter([]))
        return _AsyncIterator(iter(configured))
    return aiterate


_side_effect_methods = {
    '__eq__': _eq_side_effect,
    '__ne__': _ne_side_effect,
    '__iter__': _iter_side_effect,
    '__aiter__': _aiter_side_effect,
}


def _set_return_value(mock, method, name):
    """Give a freshly created magic method mock its default behaviour."""
    fixed = _return_values.get(name, DEFAULT)
    if fixed is not DEFAULT:
        method.return_value = fixed
        return
    compute = _calculate_return_value.get(name)
    if compute is not None:
        method.return_value = compute(mock)
        return
    make_effect = _side_effect_methods.get(name)
    if make_effect is not None:
        method.side_effect = make_effect(mock)


class _AsyncIterator:
    """Async iterator over a plain iterator (used for ``__aiter__``)."""

    def __init__(self, iterator):
        self.iterator = iterator
        code = NonCallableMock(spec_set=CodeType)
        code.co_flags = inspect.CO_ITERABLE_COROUTINE
        self.__dict__['__code__'] = code

    async def __anext__(self):
        try:
            return next(self.iterator)
        except StopIteration:
            pass
        raise StopAsyncIteration


class MagicProxy:
    """Class-level placeholder for a magic method: the real child mock is
    created the first time the method is looked up."""

    def __init__(self, name, parent):
        self.name = name
        self.parent = parent

    def create_mock(self):
        owner = self.parent
        name = self.name
        child = owner._get_child_mock(name=name, _new_name=name,
                                      _new_parent=owner)
        setattr(owner, name, child)
        _set_return_value(owner, child, name)
        return child

    def __call__(self, /, *args, **kwargs):
        return self.create_mock()(*args, **kwargs)

    def __get__(self, obj, owner=None):
        return self.create_mock()


# ---------------------------------------------------------------------------
# The mock classes
# ---------------------------------------------------------------------------

class _SpecState:
    """Deferred autospec of a child attribute: built on first access."""

    def __init__(self, spec, spec_set=False, parent=None, name=None,
                 ids=None, instance=False):
        self.spec = spec
        self.ids = ids
        self.spec_set = spec_set
        self.parent = parent
        self.instance = instance
        self.name = name


def _delegated(name, doc=None):
    """Property for a piece of call bookkeeping.

    Normally the value lives in ``__dict__['_mock_' + name]``; when the mock
    sits behind an autospecced function (``_mock_delegate``), the function
    object holds the authoritative value instead.
    """
    slot = '_mock_' + name
    # assignments must reach the property even on spec_set mocks
    _allowed_names.add(name)

    def fget(self):
        delegate = self._mock_delegate
        if delegate is None:
            return self.__dict__[slot]
        return getattr(delegate, name)

    def fset(self, value):
        delegate = self._mock_delegate
        if delegate is None:
            self.__dict__[slot] = value
        else:
            setattr(delegate, name, value)

    return property(fget, fset, doc=doc)


# Prefixes that look like a misspelt assertion method: refusing them catches
# tests that silently assert nothing.
_ASSERTION_TYPOS = ('assert', 'assret', 'asert', 'aseert', 'assrt')
_ATTRIB_DENY_LIST = frozenset({
    name.removeprefix('assert_')
    for name in ('assert_called_once', 'assert_called_once_with',
                 'assert_called_with', 'assert_any_call', 'assert_has_calls',
                 'assert_not_called', 'assert_called', 'assert_awaited',
                 'assert_awaited_once', 'assert_awaited_with',
                 'assert_awaited_once_with', 'assert_any_await',
                 'assert_has_awaits', 'assert_not_awaited')
})

# Attribute names that bypass the custom __setattr__ logic.
_allowed_names = {
    'return_value', '_mock_return_value', 'side_effect',
    '_mock_side_effect', '_mock_parent', '_mock_new_parent',
    '_mock_name', '_mock_new_name',
}


class Base:
    """Root of the mock hierarchy (swallows constructor arguments)."""

    _mock_return_value = DEFAULT
    _mock_side_effect = None

    def __init__(self, /, *args, **kwargs):
        pass


class NonCallableMock(Base):
    """A mock that cannot be called.

    Every attribute read that is not already set creates (and remembers) a
    child mock.  ``spec``/``spec_set`` restrict the available attributes to
    those of an object or a list of names, ``wraps`` forwards calls to a
    real object, ``name`` is used in the repr, and further keyword
    arguments configure attributes (dotted names reach into children).
    """

    def __new__(cls, /, *args, **kw):
        # Each mock gets its own subclass so that magic methods (which must
        # live on the type) can be configured per instance.
        bases = (cls,)
        if not issubclass(cls, AsyncMockMixin):
            spec_obj = kw.get('spec_set', kw.get('spec'))
            if spec_obj is None and args:
                spec_obj = args[0]
            if spec_obj is not None and _is_async_obj(spec_obj):
                bases = (AsyncMockMixin, cls)
        per_instance = type(cls.__name__, bases, {'__doc__': cls.__doc__})
        return super(NonCallableMock, cls).__new__(per_instance)

    def __init__(self, /, spec=None, wraps=None, name=None, spec_set=None,
                 parent=None, _spec_state=None, _new_name='',
                 _new_parent=None, _spec_as_instance=False, _eat_self=None,
                 unsafe=False, **kwargs):
        if _new_parent is None:
            _new_parent = parent
        state = self.__dict__
        state['_mock_parent'] = parent
        state['_mock_name'] = name
        state['_mock_new_name'] = _new_name
        state['_mock_new_parent'] = _new_parent
        state['_mock_sealed'] = False

        if spec_set is not None:
            spec = spec_set
            spec_set = True
        if _eat_self is None:
            _eat_self = parent is not None
        self._mock_add_spec(spec, spec_set, _spec_as_instance, _eat_self)

        state['_mock_children'] = {}
        state['_mock_wraps'] = wraps
        state['_mock_delegate'] = None
        state['_mock_called'] = False
        state['_mock_call_args'] = None
        state['_mock_call_count'] = 0
        state['_mock_call_args_list'] = _CallList()
        state['_mock_mock_calls'] = _CallList()
        state['method_calls'] = _CallList()
        state['_mock_unsafe'] = unsafe

        if kwargs:
            self.configure_mock(**kwargs)
        super().__init__()

    # -- spec handling -----------------------------------------------------

    def attach_mock(self, mock, attribute):
        """Make *mock* the child *attribute* of this mock, so that its calls
        are recorded in this mock's ``method_calls``/``mock_calls``."""
        inner = _extract_mock(mock)
        for key in ('_mock_parent', '_mock_new_parent', '_mock_name',
                    '_mock_new_name'):
            inner.__dict__[key] = None
        on_type = _static_lookup(type(self), attribute)
        if (_is_instance_mock(on_type) and hasattr(type(on_type), '__set__')
                and attribute not in _all_magics):
            # e.g. a PropertyMock installed on the type: assigning would
            # call its setter, so only record the relationship
            if _check_and_set_parent(self, mock, attribute, attribute):
                self._mock_children[attribute] = mock
            return
        setattr(self, attribute, mock)

    def mock_add_spec(self, spec, spec_set=False):
        """Restrict an existing mock to the attributes of *spec*.

        Only attributes that exist on the spec may be read (and, with
        *spec_set*, written)."""
        self._mock_add_spec(spec, spec_set)

    def _mock_add_spec(self, spec, spec_set, _spec_as_instance=False,
                       _eat_self=False):
        if _is_instance_mock(spec):
            raise InvalidSpecError(
                'Cannot spec a Mock object. [object=%r]' % (spec,))
        spec_class = None
        signature = None
        asyncs = set()
        if spec is not None and not _is_list(spec):
            spec_class = spec if isinstance(spec, type) else type(spec)
            found = _get_signature_object(spec, _spec_as_instance, _eat_self)
            signature = found[1] if found else None
            names = dir(spec)
            # Which attributes are coroutine functions is decided now, from
            # the raw (undescribed) attribute values: properties must not
            # run and later changes to the spec object do not count.
            for attr in names:
                if _is_async_attribute(_static_lookup(spec, attr)):
                    asyncs.add(attr)
            spec = names
        state = self.__dict__
        state['_spec_class'] = spec_class
        state['_spec_set'] = spec_set
        state['_spec_signature'] = signature
        state['_mock_methods'] = spec
        state['_spec_asyncs'] = asyncs

    def _mock_extend_spec_methods(self, names):
        methods = self.__dict__['_mock_methods'] or []
        self.__dict__['_mock_methods'] = list(methods) + list(names)

    def _spec_attribute_is_async(self, name):
        return name in self.__dict__.get('_spec_asyncs', ())

    def __get_class(self):
        if self._spec_class is None:
            return type(self)
        return self._spec_class

    def __set_class(self, value):
        self.__dict__['_spec_class'] = value

    __class__ = property(__get_class, __set_class)

    # -- return value / side effect -----------------------------------------

    def __get_return_value(self):
        delegate = self._mock_delegate
        if delegate is not None:
            return delegate.return_value
        value = self._mock_return_value
        # a wrapping mock reports DEFAULT: calls go to the wrapped object
        if value is DEFAULT and self._mock_wraps is None:
            value = self._get_child_mock(_new_parent=self, _new_name='()')
            self.return_value = value
        return value

    def __set_return_value(self, value):
        delegate = self._mock_delegate
        if delegate is not None:
            delegate.return_value = value
            return
        self._mock_return_value = value
        _check_and_set_parent(self, value, None, '()')

    return_value = property(__get_return_value, __set_return_value,
                            doc='The value returned when the mock is called.')

    called = _delegated('called')
    call_count = _delegated('call_count')
    call_args = _delegated('call_args')
    call_args_list = _delegated('call_args_list')
    mock_calls = _delegated('mock_calls')

    def __get_side_effect(self):
        delegate = self._mock_delegate
        if delegate is None:
            return self._mock_side_effect
        effect = delegate.side_effect
        if (effect is not None and not _callable(effect)
                and not isinstance(effect, _MockIter)
                and not _is_exception(effect)):
            effect = _MockIter(effect)
            delegate.side_effect = effect
        return effect

    def __set_side_effect(self, value):
        value = _try_iter(value)
        delegate = self._mock_delegate
        if delegate is None:
            self._mock_side_effect = value
        else:
            delegate.side_effect = value

    side_effect = property(__get_side_effect, __set_side_effect)

    # -- resetting / configuring ---------------------------------------------

    def reset_mock(self, visited=None, *, return_value=False,
                   side_effect=False):
        """Forget all recorded calls (recursively through children).

        Configured return values and side effects are kept unless the
        corresponding keyword is true."""
        if visited is None:
            visited = []
        if id(self) in visited:
            return
        visited.append(id(self))

        self.called = False
        self.call_args = None
        self.call_count = 0
        self.mock_calls = _CallList()
        self.call_args_list = _CallList()
        self.method_calls = _CallList()

        if return_value:
            self._mock_return_value = DEFAULT
        if side_effect:
            self._mock_side_effect = None

        for name, child in self._mock_children.items():
            if isinstance(child, _SpecState) or child is _deleted:
                continue
            child.reset_mock(visited, return_value=return_value,
                             side_effect=side_effect)
            if return_value and name in _all_magics:
                # magic methods fall back to their defaults, not to mocks
                _set_return_value(self, child, name)

        current = self._mock_return_value
        if _is_instance_mock(current) and current is not self:
            current.reset_mock(visited)

    def configure_mock(self, /, **kwargs):
        """Set attributes from keyword arguments; dotted keys configure
        child mocks, e.g. ``configure_mock(**{'method.return_value': 3})``.
        """
        # parents before their descendants
        for key in sorted(kwargs, key=lambda k: k.count('.')):
            path = key.split('.')
            final = path.pop()
            target = self
            for step in path:
                target = getattr(target, step)
            setattr(target, final, kwargs[key])

    # -- attribute protocol ----------------------------------------------------

    def __getattr__(self, name):
        if name in {'_mock_methods', '_mock_unsafe', '_mock_check_sig'}:
            raise AttributeError(name)
        methods = self._mock_methods
        if methods is not None:
            if name not in methods or name in _all_magics:
                raise AttributeError('Mock object has no attribute %r' % name)
        elif _is_magic(name):
            raise AttributeError(name)
        if not self._mock_unsafe and (not methods or name not in methods):
            if name.startswith(_ASSERTION_TYPOS) or name in _ATTRIB_DENY_LIST:
                raise AttributeError(
                    f'{name!r} is not a valid assertion. Use a spec '
                    f'for the mock if {name!r} is meant to be an attribute.')

        children = self._mock_children
        result = children.get(name)
        if result is _deleted:
            raise AttributeError(name)
        if result is None:
            wrapped = None
            if self._mock_wraps is not None:
                # missing attributes on the wrapped object surface here
                wrapped = getattr(self._mock_wraps, name)
            result = self._get_child_mock(parent=self, name=name,
                                          wraps=wrapped, _new_name=name,
                                          _new_parent=self)
            children[name] = result
        elif isinstance(result, _SpecState):
            try:
                result = create_autospec(result.spec, result.spec_set,
                                         result.instance, result.parent,
                                         result.name)
            except InvalidSpecError:
                owner_name = self._mock_name or self
                raise InvalidSpecError(
                    f'Cannot autospec attr {result.name!r} from target '
                    f'{owner_name!r} as it has already been mocked out. '
                    f'[target={self!r}, attr={result.spec!r}]')
            children[name] = result
        return result

    def _extract_mock_name(self):
        """Dotted path of this mock from its root, e.g. ``mock.a().b``."""
        segments = []
        node = self
        while node._mock_new_parent is not None:
            segments.append(node._mock_new_name)
            node = node._mock_new_parent
        text = node._mock_name or 'mock'
        for segment in reversed(segments):
            if segment.startswith('()'):
                text += segment
            else:
                text += '.' + segment
        return text

    def __repr__(self):
        name = self._extract_mock_name()
        name_part = ''
        if name not in ('mock', 'mock.'):
            name_part = ' name=%r' % name
        spec_part = ''
        if self._spec_class is not None:
            label = ' spec_set=%r' if self._spec_set else ' spec=%r'
            spec_part = label % self._spec_class.__name__
        return "<%s%s%s id='%s'>" % (type(self).__name__, name_part,
                                     spec_part, id(self))

    def __dir__(self):
        """Public attributes only (unless FILTER_DIR is false), including
        spec attributes and created children."""
        if not FILTER_DIR:
            return object.__dir__(self)
        names = set(self._mock_methods or ())
        names.update(n for n in dir(type(self)) if not n.startswith('_'))
        names.update(n for n in self.__dict__
                     if not n.startswith('_') or _is_magic(n))
        names.update(n for n, child in self._mock_children.items()
                     if child is not _deleted)
        return sorted(names)

    def __setattr__(self, name, value):
        if name in _allowed_names:
            return object.__setattr__(self, name, value)
        methods = self._mock_methods
        if (self._spec_set and methods is not None
                and name not in methods and name not in self.__dict__):
            raise AttributeError("Mock object has no attribute '%s'" % name)
        if name in _unsupported_magics:
            raise AttributeError(
                'Attempting to set unsupported magic method %r.' % name)
        if name in _all_magics:
            if methods is not None and name not in methods:
                raise AttributeError(
                    "Mock object has no attribute '%s'" % name)
            if _is_instance_mock(value):
                # the child mock itself becomes the method
                _check_and_set_parent(self, value, None, name)
                setattr(type(self), name, value)
                self._mock_children[name] = value
            else:
                # a plain function: install on the type, bind self here
                setattr(type(self), name, _get_method(name, value))
                func = value
                value = lambda *args, **kw: func(self, *args, **kw)
        elif name == '__class__':
            self.__dict__['_spec_class'] = value
            return
        elif _check_and_set_parent(self, value, name, name):
            self._mock_children[name] = value

        if self._mock_sealed and not hasattr(self, name):
            full = f'{self._extract_mock_name()}.{name}'
            raise AttributeError(f'Cannot set {full}')
        return object.__setattr__(self, name, value)

    def __delattr__(self, name):
        if name in _all_magics and name in type(self).__dict__:
            delattr(type(self), name)
            if name not in self.__dict__:
                # only a type-level magic method: nothing else to undo
                return
        child = self._mock_children.get(name, _missing)
        if name in self.__dict__:
            object.__delattr__(self, name)
        elif child is _deleted:
            raise AttributeError(name)
        if child is not _missing:
            del self._mock_children[name]
        self._mock_children[name] = _deleted

    # -- assertion helpers -----------------------------------------------------

    def _format_mock_call_signature(self, args, kwargs):
        return _format_call_signature(self._mock_name or 'mock', args, kwargs)

    def _format_mock_failure_message(self, args, kwargs, action='call'):
        expected = self._format_mock_call_signature(args, kwargs)
        actual = self._format_mock_call_signature(*self.call_args)
        return 'expected %s not found.\nExpected: %s\n  Actual: %s' % (
            action, expected, actual)

    def _get_call_signature_from_name(self, name):
        """Signature to normalise calls of the (dotted) child *name*."""
        if not name:
            return self._spec_signature
        node = self
        for part in name.replace('()', '').split('.'):
            if not part:
                continue
            child = node._mock_children.get(part)
            if child is None or isinstance(child, _SpecState) \
                    or child is _deleted:
                return None
            node = _extract_mock(child)
        return node._spec_signature

    def _call_matcher(self, _call):
        """Normalise *_call* against the spec signature (if any) so that
        positional and keyword spellings compare equal.  Returns the binding
        error instead of raising it."""
        if isinstance(_call, tuple) and len(_call) > 2:
            signature = self._get_call_signature_from_name(_call[0])
        else:
            signature = self._spec_signature
        if signature is None:
            return _call
        if len(_call) == 2:
            name = ''
            args, kwargs = _call
        else:
            name, args, kwargs = _call
        try:
            bound = signature.bind(*args, **kwargs)
        except TypeError as exc:
            return exc.with_traceback(None)
        # the name becomes part of the arguments so that differently named
        # calls never compare equal once normalised
        return _Call(((name, bound.args, bound.kwargs), {}), two=True)

    def _calls_repr(self, prefix='Calls'):
        if not self.mock_calls:
            return ''
        return f'\n{prefix}: {safe_repr(self.mock_calls)}.'

    def assert_not_called(self):
        """Fail if the mock has been called."""
        if self.call_count != 0:
            raise AssertionError(
                "Expected '%s' to not have been called. Called %s times.%s"
                % (self._mock_name or 'mock', self.call_count,
                   self._calls_repr()))

    def assert_called(self):
        """Fail unless the mock has been called at least once."""
        if self.call_count == 0:
            raise AssertionError("Expected '%s' to have been called." %
                                 (self._mock_name or 'mock'))

    def assert_called_once(self):
        """Fail unless the mock has been called exactly once."""
        if self.call_count != 1:
            raise AssertionError(
                "Expected '%s' to have been called once. Called %s times.%s"
                % (self._mock_name or 'mock', self.call_count,
                   self._calls_repr()))

    def assert_called_with(self, /, *args, **kwargs):
        """Fail unless the most recent call used these arguments."""
        if self.call_args is None:
            expected = self._format_mock_call_signature(args, kwargs)
            raise AssertionError(
                'expected call not found.\nExpected: %s\n  Actual: %s'
                % (expected, 'not called.'))
        expected = self._call_matcher(_Call((args, kwargs), two=True))
        actual = self._call_matcher(self.call_args)
        if actual != expected:
            cause = expected if isinstance(expected, Exception) else None
            raise AssertionError(
                self._format_mock_failure_message(args, kwargs)) from cause

    def assert_called_once_with(self, /, *args, **kwargs):
        """Fail unless the mock was called exactly once, with these
        arguments."""
        if self.call_count != 1:
            raise AssertionError(
                "Expected '%s' to be called once. Called %s times.%s"
                % (self._mock_name or 'mock', self.call_count,
                   self._calls_repr()))
        return self.assert_called_with(*args, **kwargs)

    def assert_has_calls(self, calls, any_order=False):
        """Fail unless *calls* appear in ``mock_calls``: as a contiguous
        run, or (with *any_order*) anywhere."""
        expected = [self._call_matcher(c) for c in calls]
        cause = next((e for e in expected if isinstance(e, Exception)), None)
        recorded = _CallList(self._call_matcher(c) for c in self.mock_calls)
        if not any_order:
            if expected not in recorded:
                if cause is None:
                    problem = 'Calls not found.'
                else:
                    errors = [e if isinstance(e, Exception) else None
                              for e in expected]
                    problem = ('Error processing expected calls.\n'
                               'Errors: {}'.format(errors))
                actual = f'\n  Actual: {safe_repr(self.mock_calls)}'
                raise AssertionError(
                    f'{problem}\nExpected: {_CallList(calls)}{actual}'
                ) from cause
            return
        remaining = list(recorded)
        missing = []
        for item in expected:
            try:
                remaining.remove(item)
            except ValueError:
                missing.append(item)
        if missing:
            raise AssertionError(
                '%r does not contain all of %r in its call list, '
                'found %r instead' % (self._mock_name or 'mock',
                                      tuple(missing), remaining)
            ) from cause

    def assert_any_call(self, /, *args, **kwargs):
        """Fail unless the mock was ever called with these arguments."""
        expected = self._call_matcher(_Call((args, kwargs), two=True))
        cause = expected if isinstance(expected, Exception) else None
        actual = [self._call_matcher(c) for c in self.call_args_list]
        if cause or expected not in _AnyComparer(actual):
            raise AssertionError(
                '%s call not found'
                % self._format_mock_call_signature(args, kwargs)
            ) from cause

    def _get_child_mock(self, /, **kw):
        """Create a child mock (attribute or return value).

        The child's class follows the parent's: Mock for Mock, MagicMock for
        MagicMock, AsyncMock for async specs/magics, and the first base for
        user subclasses (to keep them callable or not as appropriate).
        """
        if self._mock_sealed:
            suffix = '.' + kw['name'] if 'name' in kw else '()'
            raise AttributeError(self._extract_mock_name() + suffix)

        new_name = kw.get('_new_name')
        if new_name is not None and self._spec_attribute_is_async(new_name):
            return AsyncMock(**kw)

        own = type(self)
        if issubclass(own, MagicMock) and new_name in _async_method_magics:
            klass = AsyncMock
        elif issubclass(own, AsyncMockMixin):
            if (new_name in _all_sync_magics or
                    (self._mock_methods and new_name in self._mock_methods)):
                klass = MagicMock
            else:
                klass = AsyncMock
        elif not issubclass(own, CallableMixin):
            if issubclass(own, NonCallableMagicMock):
                klass = MagicMock
            elif issubclass(own, NonCallableMock):
                klass = Mock
            else:
                klass = own.__mro__[1]
        else:
            klass = own.__mro__[1]
        return klass(**kw)


class _MockIter:
    """Iterator wrapper used for an iterable side effect set on an
    autospecced function (whose attribute holds the raw value)."""

    def __init__(self, obj):
        self.obj = iter(obj)

    def __next__(self):
        return next(self.obj)

    def __iter__(self):
        return self


def _check_and_set_parent(parent, value, name, new_name):
    """Adopt *value* as a child of *parent* if it is an unowned mock.

    Returns True if it was adopted.  Named mocks and mocks that already have
    a parent are left alone, and a mock never becomes its own ancestor."""
    value = _extract_mock(value)
    if not _is_instance_mock(value):
        return False
    if (value._mock_name or value._mock_new_name
            or value._mock_parent is not None
            or value._mock_new_parent is not None):
        return False
    ancestor = parent
    while ancestor is not None:
        if ancestor is value:
            return False
        ancestor = ancestor._mock_new_parent
    if new_name:
        value._mock_new_parent = parent
        value._mock_new_name = new_name
    if name:
        value._mock_parent = parent
        value._mock_name = name
    return True


class CallableMixin(Base):
    """Adds call support: recording, side effects and return values."""

    def __init__(self, spec=None, side_effect=None, return_value=DEFAULT,
                 wraps=None, name=None, spec_set=None, parent=None,
                 _spec_state=None, _new_name='', _new_parent=None, **kwargs):
        self.__dict__['_mock_return_value'] = return_value
        super().__init__(spec, wraps, name, spec_set, parent, _spec_state,
                         _new_name, _new_parent, **kwargs)
        self.side_effect = side_effect

    def _mock_check_sig(self, /, *args, **kwargs):
        # replaced on the per-instance type when a spec signature exists
        pass

    def __call__(self, /, *args, **kwargs):
        self._mock_check_sig(*args, **kwargs)
        self._increment_mock_call(*args, **kwargs)
        return self._mock_call(*args, **kwargs)

    def _mock_call(self, /, *args, **kwargs):
        return self._execute_mock_call(*args, **kwargs)

    def _increment_mock_call(self, /, *args, **kwargs):
        self.called = True
        self.call_count += 1
        this_call = _Call((args, kwargs), two=True)
        self.call_args = this_call
        self.call_args_list.append(this_call)
        self.mock_calls.append(_Call(('', args, kwargs)))
        self._record_in_ancestors(args, kwargs)

    def _record_in_ancestors(self, args, kwargs):
        """Append this call to ``mock_calls`` (and, along the chain of real
        attribute parents, ``method_calls``) of every ancestor."""
        # name of the call as seen from the current ancestor
        path = self._mock_new_name
        # method_calls only follow attribute relationships
        method_path = self._mock_name
        track_methods = self._mock_parent is not None
        node = self._mock_new_parent
        while node is not None:
            if track_methods:
                node.method_calls.append(_Call((method_path, args, kwargs)))
                track_methods = node._mock_parent is not None
                if track_methods:
                    method_path = node._mock_name + '.' + method_path
            node.mock_calls.append(_Call((path, args, kwargs)))
            prefix = node._mock_new_name
            if prefix:
                if path.startswith('()'):
                    path = prefix + path
                else:
                    path = prefix + '.' + path
            node = node._mock_new_parent

    def _execute_mock_call(self, /, *args, **kwargs):
        effect = self.side_effect
        if effect is not None:
            if _is_exception(effect):
                raise effect
            if not _callable(effect):
                outcome = next(effect)
                if _is_exception(outcome):
                    raise outcome
            else:
                outcome = effect(*args, **kwargs)
            if outcome is not DEFAULT:
                return outcome

        if self._mock_return_value is not DEFAULT:
            return self.return_value
        delegate = self._mock_delegate
        if delegate is not None and delegate.return_value is not DEFAULT:
            return self.return_value
        if self._mock_wraps is not None:
            return self._mock_wraps(*args, **kwargs)
        return self.return_value


class Mock(CallableMixin, NonCallableMock):
    """A flexible, callable mock object.

    Calls are recorded (``called``, ``call_count``, ``call_args``,
    ``call_args_list``, ``mock_calls``, ``method_calls``) and can be checked
    with the ``assert_*`` methods.  Behaviour on call: ``side_effect`` (an
    exception, an iterable of results, or a function) takes precedence over
    ``return_value``; ``wraps`` forwards to a real object when no return
    value has been configured.

    Constructor arguments: spec, spec_set, side_effect, return_value,
    wraps, name, unsafe, plus attribute values as keyword arguments.
    """


class MagicMixin(Base):
    """Configures the supported magic methods on the per-instance class."""

    def __init__(self, /, *args, **kw):
        self._mock_set_magics()
        super().__init__(*args, **kw)
        # again: the spec (now known) may rule some of them out
        self._mock_set_magics()

    def _mock_set_magics(self):
        candidates = _magics | _async_method_magics
        own = type(self)
        allowed = candidates
        spec_names = getattr(self, '_mock_methods', None)
        if spec_names is not None:
            allowed = candidates.intersection(spec_names)
            for name in candidates - allowed:
                if name in own.__dict__:
                    delattr(own, name)
        for name in allowed - set(own.__dict__):
            setattr(own, name, MagicProxy(name, self))


class NonCallableMagicMock(MagicMixin, NonCallableMock):
    """A non-callable MagicMock."""

    def mock_add_spec(self, spec, spec_set=False):
        """Restrict the mock to the attributes of *spec* (magic methods
        included)."""
        self._mock_add_spec(spec, spec_set)
        self._mock_set_magics()


class AsyncMagicMixin(MagicMixin):
    pass


class MagicMock(MagicMixin, Mock):
    """Mock with default implementations of most magic methods.

    Magic methods are created lazily and can be configured like any other
    attribute (``m.__len__.return_value = 3``).
    """

    def mock_add_spec(self, spec, spec_set=False):
        """Restrict the mock to the attributes of *spec* (magic methods
        included)."""
        self._mock_add_spec(spec, spec_set)
        self._mock_set_magics()


# ---------------------------------------------------------------------------
# Async support
# ---------------------------------------------------------------------------

def _coroutine_marker():
    # obtain the attribute name/value that marks a coroutine function by
    # asking inspect to mark a scratch function
    def scratch():
        pass
    before = set(vars(scratch))
    inspect.markcoroutinefunction(scratch)
    return {k: v for k, v in vars(scratch).items() if k not in before}


class AsyncMockMixin(Base):
    """Makes calling the mock return a coroutine; awaits are recorded
    separately from calls."""

    await_count = _delegated('await_count')
    await_args = _delegated('await_args')
    await_args_list = _delegated('await_args_list')

    def __init__(self, /, *args, **kwargs):
        super().__init__(*args, **kwargs)
        state = self.__dict__
        state['_mock_await_count'] = 0
        state['_mock_await_args'] = None
        state['_mock_await_args_list'] = _CallList()
        code = NonCallableMock(spec_set=CodeType)
        code.co_flags = (inspect.CO_COROUTINE + inspect.CO_VARARGS
                         + inspect.CO_VARKEYWORDS)
        code.co_argcount = 0
        code.co_varnames = ('args', 'kwargs')
        code.co_posonlyargcount = 0
        code.co_kwonlyargcount = 0
        state['__code__'] = code
        state['__name__'] = 'AsyncMock'
        state['__defaults__'] = tuple()
        state['__kwdefaults__'] = {}
        state['__annotations__'] = None
        state.update(_coroutine_marker())

    async def _execute_mock_call(self, /, *args, **kwargs):
        this_await = _Call((args, kwargs), two=True)
        self.await_count += 1
        self.await_args = this_await
        self.await_args_list.append(this_await)

        effect = self.side_effect
        if effect is not None:
            if _is_exception(effect):
                raise effect
            if not _callable(effect):
                try:
                    outcome = next(effect)
                except StopIteration:
                    # an exhausted iterator inside a coroutine would turn
                    # into RuntimeError
                    raise StopAsyncIteration
                if _is_exception(outcome):
                    raise outcome
            elif inspect.iscoroutinefunction(effect):
                outcome = await effect(*args, **kwargs)
            else:
                outcome = effect(*args, **kwargs)
            if outcome is not DEFAULT:
                return outcome

        if self._mock_return_value is not DEFAULT:
            return self.return_value
        wrapped = self._mock_wraps
        if wrapped is not None:
            if inspect.iscoroutinefunction(wrapped):
                return await wrapped(*args, **kwargs)
            return wrapped(*args, **kwargs)
        return self.return_value

    def _await_name(self):
        return self._mock_name or 'mock'

    def assert_awaited(self):
        """Fail unless the mock was awaited at least once."""
        if self.await_count == 0:
            raise AssertionError(
                f'Expected {self._await_name()} to have been awaited.')

    def assert_awaited_once(self):
        """Fail unless the mock was awaited exactly once."""
        if self.await_count != 1:
            raise AssertionError(
                f'Expected {self._await_name()} to have been awaited once.'
                f' Awaited {self.await_count} times.')

    def assert_awaited_with(self, /, *args, **kwargs):
        """Fail unless the last await used these arguments."""
        if self.await_args is None:
            expected = self._format_mock_call_signature(args, kwargs)
            raise AssertionError(f'Expected await: {expected}\nNot awaited')
        expected = self._call_matcher(_Call((args, kwargs), two=True))
        actual = self._call_matcher(self.await_args)
        if actual != expected:
            cause = expected if isinstance(expected, Exception) else None
            expected_text = self._format_mock_call_signature(args, kwargs)
            actual_text = self._format_mock_call_signature(*self.await_args)
            raise AssertionError(
                'expected await not found.\nExpected: %s\n  Actual: %s'
                % (expected_text, actual_text)) from cause

    def assert_awaited_once_with(self, /, *args, **kwargs):
        """Fail unless the mock was awaited exactly once, with these
        arguments."""
        if self.await_count != 1:
            raise AssertionError(
                f'Expected {self._await_name()} to have been awaited once.'
                f' Awaited {self.await_count} times.')
        return self.assert_awaited_with(*args, **kwargs)

    def assert_any_await(self, /, *args, **kwargs):
        """Fail unless the mock was ever awaited with these arguments."""
        expected = self._call_matcher(_Call((args, kwargs), two=True))
        cause = expected if isinstance(expected, Exception) else None
        actual = [self._call_matcher(c) for c in self.await_args_list]
        if cause or expected not in _AnyComparer(actual):
            text = self._format_mock_call_signature(args, kwargs)
            raise AssertionError('%s await not found' % text) from cause

    def assert_has_awaits(self, calls, any_order=False):
        """Fail unless *calls* appear in ``await_args_list``: as a
        contiguous run, or (with *any_order*) anywhere."""
        expected = [self._call_matcher(c) for c in calls]
        cause = next((e for e in expected if isinstance(e, Exception)), None)
        recorded = _CallList(self._call_matcher(c)
                             for c in self.await_args_list)
        if not any_order:
            if expected not in recorded:
                if cause is None:
                    problem = 'Awaits not found.'
                else:
                    errors = [e if isinstance(e, Exception) else None
                              for e in expected]
                    problem = ('Error processing expected awaits.\n'
                               'Errors: {}'.format(errors))
                raise AssertionError(
                    f'{problem}\nExpected: {_CallList(calls)}\n'
                    f'Actual: {self.await_args_list}') from cause
            return
        remaining = list(recorded)
        missing = []
        for item in expected:
            try:
                remaining.remove(item)
            except ValueError:
                missing.append(item)
        if missing:
            raise AssertionError(
                '%r not all found in await list' % (tuple(missing),)
            ) from cause

    def assert_not_awaited(self):
        """Fail if the mock was ever awaited."""
        if self.await_count != 0:
            raise AssertionError(
                f'Expected {self._await_name()} to not have been awaited.'
                f' Awaited {self.await_count} times.')

    def reset_mock(self, /, *args, **kwargs):
        """See NonCallableMock.reset_mock; also forgets awaits."""
        super().reset_mock(*args, **kwargs)
        self.await_count = 0
        self.await_args = None
        self.await_args_list = _CallList()


class AsyncMock(AsyncMockMixin, AsyncMagicMixin, Mock):
    """A mock whose call returns an awaitable.

    Awaiting the result applies ``side_effect``/``return_value`` like a
    normal Mock call would, and records the await (``await_count``,
    ``await_args``, ``await_args_list``, ``assert_awaited*``).  Child
    attributes are AsyncMocks too, except where a spec says an attribute is
    a plain function, and synchronous magic methods are MagicMocks.
    """


# ---------------------------------------------------------------------------
# ThreadingMock
# ---------------------------------------------------------------------------

class ThreadingMixin(Base):
    """Lets other threads wait until the mock has been called."""

    DEFAULT_TIMEOUT = None

    def _get_child_mock(self, /, **kw):
        if 'timeout' not in kw:
            for key in ('parent', '_new_parent'):
                owner = kw.get(key)
                if isinstance(owner, ThreadingMixin):
                    kw['timeout'] = owner._mock_wait_timeout
                    break
        return super()._get_child_mock(**kw)

    def __init__(self, *args, timeout=_timeout_unset, **kwargs):
        # the lock must exist before children can be created
        self.__dict__['_mock_cond'] = threading.Condition(threading.RLock())
        super().__init__(*args, **kwargs)
        if timeout is _timeout_unset:
            timeout = self.DEFAULT_TIMEOUT
        self.__dict__['_mock_wait_timeout'] = timeout

    def __getattr__(self, name):
        cond = self.__dict__.get('_mock_cond')
        if cond is None or name.startswith('_mock_'):
            return super().__getattr__(name)
        with cond:
            return super().__getattr__(name)

    def _increment_mock_call(self, /, *args, **kwargs):
        cond = self._mock_cond
        with cond:
            super()._increment_mock_call(*args, **kwargs)
            cond.notify_all()

    def reset_mock(self, /, *args, **kwargs):
        """See NonCallableMock.reset_mock."""
        with self._mock_cond:
            super().reset_mock(*args, **kwargs)

    def _resolve_timeout(self, timeout):
        if timeout is _timeout_unset:
            return self._mock_wait_timeout
        return timeout

    def wait_until_called(self, *, timeout=_timeout_unset):
        """Block until the mock has been called (at least once since the
        last reset), or fail after *timeout* seconds."""
        timeout = self._resolve_timeout(timeout)
        with self._mock_cond:
            ok = self._mock_cond.wait_for(lambda: self.call_count > 0,
                                          timeout)
        if not ok:
            raise AssertionError(
                f"{self._mock_name or 'mock'} was not called before "
                f"timeout({timeout}).")

    def wait_until_any_call_with(self, *args, **kwargs):
        """Block until the mock has been called with these arguments, or
        fail after the mock's timeout."""
        wanted = _Call((args, kwargs), two=True)
        with self._mock_cond:
            ok = self._mock_cond.wait_for(
                lambda: wanted in self.call_args_list,
                self._mock_wait_timeout)
        if not ok:
            text = self._format_mock_call_signature(args, kwargs)
            raise AssertionError(f'{text} call not found')


class ThreadingMock(ThreadingMixin, MagicMixin, Mock):
    """A MagicMock whose calls can be waited for from other threads
    (``wait_until_called``, ``wait_until_any_call_with``).

    ``timeout`` (default ``DEFAULT_TIMEOUT``) bounds the waits and is
    inherited by child mocks.
    """


# ---------------------------------------------------------------------------
# Autospeccing
# ---------------------------------------------------------------------------

_AUTOSPEC_PROXIED = (
    'assert_called_with', 'assert_called_once_with', 'assert_has_calls',
    'assert_any_call', 'assert_called', 'assert_not_called',
    'assert_called_once',
)

_AUTOSPEC_ASYNC_PROXIED = (
    'assert_awaited', 'assert_awaited_once', 'assert_awaited_with',
    'assert_awaited_once_with', 'assert_any_await', 'assert_has_awaits',
    'assert_not_awaited',
)


def _function_stand_in(mock, original, instance=False):
    """Return a real function shaped like *original* that validates its
    arguments against the original signature and then calls *mock*.

    The function exposes the mock's API (``mock``, ``return_value``,
    ``assert_called_with`` ...); the mock delegates its bookkeeping to the
    function so both views stay in sync.
    """
    skip_first = isinstance(original, type)
    found = _get_signature_object(original, instance, skip_first)
    if found is None:
        return mock
    func, sig = found

    if inspect.iscoroutinefunction(original):
        async def stand_in(*args, **kwargs):
            sig.bind(*args, **kwargs)
            return await mock(*args, **kwargs)
    else:
        def stand_in(*args, **kwargs):
            sig.bind(*args, **kwargs)
            return mock(*args, **kwargs)

    name = getattr(original, '__name__', None)
    if not isinstance(name, str) or not name.isidentifier():
        name = 'funcopy'
    stand_in.__name__ = name
    stand_in.__qualname__ = name
    for attr in ('__doc__', '__module__'):
        try:
            setattr(stand_in, attr, getattr(func, attr))
        except (AttributeError, TypeError):
            pass
    stand_in.__signature__ = sig
    _wire_stand_in(stand_in, mock)
    return stand_in


def _wire_stand_in(funcopy, mock):
    funcopy.mock = mock
    funcopy.called = False
    funcopy.call_count = 0
    funcopy.call_args = None
    funcopy.call_args_list = _CallList()
    funcopy.method_calls = _CallList()
    funcopy.mock_calls = _CallList()
    funcopy.return_value = mock.return_value
    funcopy.side_effect = mock.side_effect
    funcopy._mock_children = mock._mock_children

    for name in _AUTOSPEC_PROXIED:
        setattr(funcopy, name, getattr(mock, name))

    def reset_mock(*args, **kwargs):
        funcopy.method_calls = _CallList()
        funcopy.mock_calls = _CallList()
        mock.reset_mock(*args, **kwargs)
        current = funcopy.return_value
        if _is_instance_mock(current) and current is not mock:
            current.reset_mock()

    funcopy.reset_mock = reset_mock

    if isinstance(mock, AsyncMockMixin):
        funcopy.await_count = 0
        funcopy.await_args = None
        funcopy.await_args_list = _CallList()
        for name in _AUTOSPEC_ASYNC_PROXIED:
            setattr(funcopy, name, getattr(mock, name))

        def reset_async(*args, **kwargs):
            reset_mock(*args, **kwargs)
            funcopy.await_count = 0
            funcopy.await_args = None
            funcopy.await_args_list = _CallList()
        funcopy.reset_mock = reset_async

    mock._mock_delegate = funcopy


def _check_spec_arg_typos(kwargs):
    for typo in ('autospect', 'auto_spec', 'set_spec'):
        if typo in kwargs:
            raise RuntimeError(
                f'{typo!r} might be a typo; use unsafe=True if this is '
                f'intended')


def create_autospec(spec, spec_set=False, instance=False, _parent=None,
                    _name=None, *, unsafe=False, **kwargs):
    """Create a mock with the same shape as *spec*.

    Attributes are mocked lazily with their own spec, and functions/methods
    (and the class or instance itself, if callable) check that calls
    match the original signature.  With *spec_set*, setting non-existent
    attributes fails.  *instance* says that *spec* is a class but the mock
    should stand for an instance of it.  Extra keyword arguments configure
    the mock.
    """
    if _is_list(spec):
        spec = type(spec)
    if _is_instance_mock(spec):
        raise InvalidSpecError(
            f'Cannot autospec a Mock object. [object={spec!r}]')

    is_type = isinstance(spec, type)
    is_async_func = _is_async_func(spec)
    if is_async_func and instance:
        raise RuntimeError('Instance can not be True when create_autospec '
                           'is mocking an async function')

    entries = [(name, _missing) for name in dir(spec)]
    dataclass_names = None
    if is_type and instance and _is_dataclass(spec):
        dc_fields = _dc_fields(spec)
        dataclass_names = [f.name for f in dc_fields]
        entries.extend((f.name, f.type) for f in dc_fields)

    if spec is None:
        spec_kwargs = {}
    elif spec_set:
        spec_kwargs = {'spec_set': spec}
    else:
        spec_kwargs = {'spec': spec}
    if spec_kwargs and instance:
        spec_kwargs['_spec_as_instance'] = True

    if not unsafe:
        _check_spec_arg_typos(kwargs)

    _name = kwargs.pop('name', _name)
    new_name = _name if _parent is not None else ''
    # dotted settings may reach into the autospecced return value or
    # children, so they are applied once the whole structure exists
    dotted_settings = {k: v for k, v in kwargs.items() if '.' in k}
    spec_kwargs.update((k, v) for k, v in kwargs.items() if '.' not in k)

    if inspect.isdatadescriptor(spec):
        # the value type a descriptor produces is unknown
        klass = MagicMock
        spec_kwargs = {}
    elif is_async_func:
        klass = AsyncMock
    elif is_type and instance and not _instance_callable(spec):
        klass = NonCallableMagicMock
    elif not _callable(spec):
        klass = NonCallableMagicMock
    else:
        klass = MagicMock

    mock = klass(parent=_parent, _new_parent=_parent, _new_name=new_name,
                 name=_name, **spec_kwargs)
    if dataclass_names is not None:
        mock._mock_extend_spec_methods(dataclass_names)

    if isinstance(spec, FunctionTypes):
        # top-level function: hand out a real function wrapping the mock
        mock = _function_stand_in(mock, spec)
    else:
        _check_sig(spec, mock, is_type, instance)

    if _parent is not None and not instance:
        _parent._mock_children[_name] = mock

    wrapped = kwargs.pop('wraps', None)

    if is_type and not instance and 'return_value' not in kwargs:
        mock.return_value = create_autospec(spec, spec_set, instance=True,
                                            _name='()', _parent=mock,
                                            wraps=wrapped)

    for entry, original in entries:
        if _is_magic(entry):
            # MagicMock already provides the useful magic methods
            continue
        if original is _missing:
            try:
                original = getattr(spec, entry)
            except AttributeError:
                continue

        child_kwargs = {'spec': original}
        if wrapped is not None and hasattr(wrapped, entry):
            child_kwargs['wraps'] = original
        if spec_set:
            child_kwargs = {'spec_set': original}

        if not isinstance(original, FunctionTypes):
            # anything but a function is specced on first access
            mock._mock_children[entry] = _SpecState(original, spec_set, mock,
                                                    entry, instance)
            continue

        owner = mock.mock if isinstance(mock, FunctionTypes) else mock
        skip_first = _must_skip(spec, entry, is_type)
        child_kwargs['_eat_self'] = skip_first
        child_klass = (AsyncMock if inspect.iscoroutinefunction(original)
                       else MagicMock)
        child = child_klass(parent=owner, name=entry, _new_name=entry,
                            _new_parent=owner, **child_kwargs)
        mock._mock_children[entry] = child
        child.return_value = child_klass()
        _check_sig(original, child, skip_first)
        if isinstance(child, FunctionTypes):
            setattr(mock, entry, child)

    if dotted_settings:
        _extract_mock(mock).configure_mock(**dotted_settings)
    return mock


# ---------------------------------------------------------------------------
# patch
# ---------------------------------------------------------------------------

def _resolve_dotted(path):
    """Import/look up a dotted name like ``package.module.Class.attr``."""
    parts = path.split('.')
    import importlib
    obj = importlib.import_module(parts[0])
    prefix = parts[0]
    for part in parts[1:]:
        prefix = prefix + '.' + part
        try:
            obj = getattr(obj, part)
        except AttributeError:
            importlib.import_module(prefix)
            obj = getattr(obj, part)
    return obj


def _get_target(target):
    try:
        owner, attribute = target.rsplit('.', 1)
    except (TypeError, ValueError, AttributeError):
        raise TypeError(
            f'Need a valid target to patch. You supplied: {target!r}')
    return _partial(_resolve_dotted, owner), attribute


class _patch:
    """One attribute replacement, usable as decorator, context manager or
    via start()/stop()."""

    attribute_name = None
    _active_patches = []

    def __init__(self, getter, attribute, new, spec, create, spec_set,
                 autospec, new_callable, kwargs, *, unsafe=False):
        if new_callable is not None:
            if new is not DEFAULT:
                raise ValueError(
                    "Cannot use 'new' and 'new_callable' together")
            if autospec is not None:
                raise ValueError(
                    "Cannot use 'autospec' and 'new_callable' together")
        if not unsafe:
            _check_spec_arg_typos(kwargs)
        if _is_instance_mock(spec):
            raise InvalidSpecError(
                f'Cannot spec attr {attribute!r} as the spec has already '
                f'been mocked out. [spec={spec!r}]')
        if _is_instance_mock(spec_set):
            raise InvalidSpecError(
                f'Cannot spec attr {attribute!r} as the spec_set target '
                f'has already been mocked out. [spec_set={spec_set!r}]')

        self.getter = getter
        self.attribute = attribute
        self.new = new
        self.new_callable = new_callable
        self.spec = spec
        self.create = create
        self.has_local = False
        self.spec_set = spec_set
        self.autospec = autospec
        self.kwargs = kwargs
        self.additional_patchers = []
        self.is_started = False
        self._unsafe = unsafe

    def copy(self):
        clone = _patch(self.getter, self.attribute, self.new, self.spec,
                       self.create, self.spec_set, self.autospec,
                       self.new_callable, self.kwargs, unsafe=self._unsafe)
        clone.attribute_name = self.attribute_name
        clone.additional_patchers = [p.copy()
                                     for p in self.additional_patchers]
        return clone

    # -- decorating ------------------------------------------------------------

    def __call__(self, func):
        if isinstance(func, type):
            return self.decorate_class(func)
        if inspect.iscoroutinefunction(func):
            return self.decorate_async_callable(func)
        return self.decorate_callable(func)

    def decorate_class(self, klass):
        for attr in dir(klass):
            if not attr.startswith(patch.TEST_PREFIX):
                continue
            value = getattr(klass, attr)
            if not hasattr(value, '__call__'):
                continue
            setattr(klass, attr, self.copy()(value))
        return klass

    @contextlib.contextmanager
    def decoration_helper(self, patched, args, keywargs):
        extra = []
        with contextlib.ExitStack() as stack:
            for patcher in patched.patchings:
                produced = stack.enter_context(patcher)
                if patcher.attribute_name is not None:
                    keywargs.update(produced)
                elif patcher.new is DEFAULT:
                    extra.append(produced)
            yield args + tuple(extra), keywargs

    def decorate_callable(self, func):
        if hasattr(func, 'patchings'):
            # stacked decorators share one wrapper
            func.patchings.append(self)
            return func

        @_wraps(func)
        def patched(*args, **keywargs):
            with self.decoration_helper(patched, args, keywargs) as \
                    (new_args, new_kwargs):
                return func(*new_args, **new_kwargs)

        patched.patchings = [self]
        return patched

    def decorate_async_callable(self, func):
        if hasattr(func, 'patchings'):
            func.patchings.append(self)
            return func

        @_wraps(func)
        async def patched(*args, **keywargs):
            with self.decoration_helper(patched, args, keywargs) as \
                    (new_args, new_kwargs):
                return await func(*new_args, **new_kwargs)

        patched.patchings = [self]
        return patched

    # -- the actual patching ------------------------------------------------------

    def get_original(self):
        target = self.getter()
        name = self.attribute
        local = False
        try:
            original = target.__dict__[name]
        except (AttributeError, KeyError):
            original = getattr(target, name, DEFAULT)
        else:
            local = True
        if name in _builtins and isinstance(target, ModuleType):
            self.create = True
        if not self.create and original is DEFAULT:
            raise AttributeError(
                '%s does not have the attribute %r' % (target, name))
        return original, local

    def _make_new(self, original):
        """Build the replacement object when none was given explicitly."""
        spec, spec_set = self.spec, self.spec_set
        autospec, kwargs = self.autospec, self.kwargs
        new_callable = self.new_callable
        if spec is False:
            spec = None
        if spec_set is False:
            spec_set = None
        if autospec is False:
            autospec = None
        if spec is not None and autospec is not None:
            raise TypeError("Can't specify spec and autospec")
        if ((spec is not None or autospec is not None)
                and spec_set not in (True, None)):
            raise TypeError("Can't provide explicit spec_set *and* spec "
                            "or autospec")

        if self.new is not DEFAULT:
            if autospec is not None:
                raise TypeError("autospec creates the mock for you. Can't "
                                "specify autospec and new.")
            if kwargs:
                raise TypeError("Can't pass kwargs to a mock we aren't "
                                "creating")
            return self.new

        if autospec is not None:
            if original is DEFAULT:
                raise TypeError("Can't use 'autospec' with create=True")
            spec_set = bool(spec_set)
            if autospec is True:
                autospec = original
            if _is_instance_mock(self.target):
                raise InvalidSpecError(
                    f'Cannot autospec attr {self.attribute!r} as the patch '
                    f'target has already been mocked out. '
                    f'[target={self.target!r}, attr={autospec!r}]')
            if _is_instance_mock(autospec):
                target_name = getattr(self.target, '__name__', self.target)
                raise InvalidSpecError(
                    f'Cannot autospec attr {self.attribute!r} from target '
                    f'{target_name!r} as it has already been mocked out. '
                    f'[target={self.target!r}, attr={autospec!r}]')
            return create_autospec(autospec, spec_set=spec_set,
                                   _name=self.attribute, **kwargs)

        # a plain MagicMock (or new_callable), possibly with a spec
        if spec is True:
            spec = original
            if spec_set is True:
                spec_set = original
                spec = None
        elif spec is not None:
            if spec_set is True:
                spec_set = spec
                spec = None
        elif spec_set is True:
            spec_set = original

        inherit = False
        if spec is not None or spec_set is not None:
            if original is DEFAULT:
                raise TypeError("Can't use 'spec' with create=True")
            if isinstance(original, type):
                # the class is patched: instances get the spec too
                inherit = True

        if spec is None and _is_async_obj(original):
            klass = AsyncMock
        else:
            klass = MagicMock
        options = {}
        if new_callable is not None:
            klass = new_callable
        elif spec is not None or spec_set is not None:
            effective = spec_set if spec_set is not None else spec
            if _is_list(effective):
                not_callable = '__call__' not in effective
            else:
                not_callable = not callable(effective)
            if _is_async_func(effective):
                klass = AsyncMock
            elif not_callable:
                klass = NonCallableMagicMock

        if spec is not None:
            options['spec'] = spec
        if spec_set is not None:
            options['spec_set'] = spec_set
        if (isinstance(klass, type) and issubclass(klass, NonCallableMock)
                and self.attribute):
            options['name'] = self.attribute
        options.update(kwargs)
        new = klass(**options)

        if inherit and _is_instance_mock(new):
            effective = spec_set if spec_set is not None else spec
            if (not _is_list(effective)
                    and not _instance_callable(effective)):
                klass = NonCallableMagicMock
            options.pop('name')
            new.return_value = klass(_new_parent=new, _new_name='()',
                                     **options)
        return new

    def __enter__(self):
        """Apply the patch; returns the replacement object."""
        self.target = self.getter()
        original, local = self.get_original()
        new = self._make_new(original)

        self.temp_original = original
        self.is_local = local
        self._exit_stack = contextlib.ExitStack()
        self.is_started = True
        try:
            setattr(self.target, self.attribute, new)
            if self.attribute_name is None:
                return new
            # patch.multiple: hand back a dict of the created mocks
            produced = {}
            if self.new is DEFAULT:
                produced[self.attribute_name] = new
            for patcher in self.additional_patchers:
                value = self._exit_stack.enter_context(patcher)
                if patcher.new is DEFAULT:
                    produced.update(value)
            return produced
        except:
            if not self.__exit__(*sys.exc_info()):
                raise

    def __exit__(self, *exc_info):
        """Undo the patch."""
        if not self.is_started:
            return
        target, name = self.target, self.attribute
        if self.is_local and self.temp_original is not DEFAULT:
            setattr(target, name, self.temp_original)
        else:
            delattr(target, name)
            if not self.create and (
                    not hasattr(target, name) or name in (
                        '__doc__', '__module__', '__defaults__',
                        '__annotations__', '__kwdefaults__')):
                # e.g. proxies that keep answering after the delete
                setattr(target, name, self.temp_original)

        del self.temp_original
        del self.is_local
        del self.target
        stack = self._exit_stack
        del self._exit_stack
        self.is_started = False
        return stack.__exit__(*exc_info)

    def start(self):
        """Activate the patch and return the replacement."""
        result = self.__enter__()
        self._active_patches.append(self)
        return result

    def stop(self):
        """Undo a patch started with start()."""
        try:
            self._active_patches.remove(self)
        except ValueError:
            # not started, or already stopped
            pass
        return self.__exit__(None, None, None)


def _patch_object(target, attribute, new=DEFAULT, spec=None, create=False,
                  spec_set=None, autospec=None, new_callable=None, *,
                  unsafe=False, **kwargs):
    """patch.object(target, attribute, new=DEFAULT, ...)

    Like patch(), but the object to patch is passed directly instead of
    being named by a dotted string.
    """
    if type(target) is str:
        raise TypeError(f'{target!r} must be the actual object to be '
                        f'patched, not a str')
    return _patch(lambda: target, attribute, new, spec, create, spec_set,
                  autospec, new_callable, kwargs, unsafe=unsafe)


def _patch_multiple(target, spec=None, create=False, spec_set=None,
                    autospec=None, new_callable=None, **kwargs):
    """patch.multiple(target, **attributes)

    Patch several attributes of one target at once.  Use DEFAULT as a value
    to have a mock created; as a decorator the created mocks are passed as
    keyword arguments, as a context manager a dict of them is returned.
    """
    if type(target) is str:
        getter = _partial(_resolve_dotted, target)
    else:
        getter = lambda: target
    if not kwargs:
        raise ValueError(
            'Must supply at least one keyword argument with patch.multiple')
    patchers = []
    for attribute, new in kwargs.items():
        patcher = _patch(getter, attribute, new, spec, create, spec_set,
                         autospec, new_callable, {})
        patcher.attribute_name = attribute
        patchers.append(patcher)
    first = patchers[0]
    first.additional_patchers.extend(patchers[1:])
    return first


def patch(target, new=DEFAULT, spec=None, create=False, spec_set=None,
          autospec=None, new_callable=None, *, unsafe=False, **kwargs):
    """Replace the object named by *target* (``'package.module.Name'``)
    while a test runs.

    Works as a function/class decorator, a context manager, or through
    ``start()``/``stop()``.  Without *new* a MagicMock (or AsyncMock for
    async targets, or ``new_callable()``) is created and passed to the
    decorated function / returned by ``__enter__``.  *spec*, *spec_set* and
    *autospec* shape the created mock (``True`` means "like the original").
    *create* allows patching attributes that do not exist.  Remaining
    keyword arguments configure the created mock.
    """
    getter, attribute = _get_target(target)
    return _patch(getter, attribute, new, spec, create, spec_set, autospec,
                  new_callable, kwargs, unsafe=unsafe)


class _patch_dict:
    """patch.dict(in_dict, values=(), clear=False, **kwargs)

    Temporarily update a mapping (or the mapping named by a dotted string),
    restoring its previous contents afterwards.  With *clear* the mapping
    is emptied first.
    """

    def __init__(self, in_dict, values=(), clear=False, **kwargs):
        self.in_dict = in_dict
        self.values = dict(values)
        self.values.update(kwargs)
        self.clear = clear
        self._original = None

    def __call__(self, f):
        if isinstance(f, type):
            return self.decorate_class(f)
        if inspect.iscoroutinefunction(f):
            return self.decorate_async_callable(f)
        return self.decorate_callable(f)

    def decorate_callable(self, f):
        @_wraps(f)
        def _inner(*args, **kw):
            self._patch_dict()
            try:
                return f(*args, **kw)
            finally:
                self._unpatch_dict()
        return _inner

    def decorate_async_callable(self, f):
        @_wraps(f)
        async def _inner(*args, **kw):
            self._patch_dict()
            try:
                return await f(*args, **kw)
            finally:
                self._unpatch_dict()
        return _inner

    def decorate_class(self, klass):
        for attr in dir(klass):
            value = getattr(klass, attr)
            if attr.startswith(patch.TEST_PREFIX) and hasattr(value,
                                                              '__call__'):
                clone = _patch_dict(self.in_dict, self.values, self.clear)
                setattr(klass, attr, clone(value))
        return klass

    def __enter__(self):
        """Patch the dict."""
        self._patch_dict()
        return self.in_dict

    def _target(self):
        if isinstance(self.in_dict, str):
            # resolved on every use: the name may be rebound meanwhile
            if not hasattr(self, '_dotted_name'):
                self._dotted_name = self.in_dict
        dotted = getattr(self, '_dotted_name', None)
        if dotted is not None:
            self.in_dict = _resolve_dotted(dotted)
        return self.in_dict

    def _patch_dict(self):
        mapping = self._target()
        try:
            saved = mapping.copy()
        except AttributeError:
            saved = {key: mapping[key] for key in mapping}
        self._original = saved
        if self.clear:
            _clear_dict(mapping)
        try:
            mapping.update(self.values)
        except AttributeError:
            for key in self.values:
                mapping[key] = self.values[key]

    def _unpatch_dict(self):
        mapping = self.in_dict
        saved = self._original
        _clear_dict(mapping)
        try:
            mapping.update(saved)
        except AttributeError:
            for key in saved:
                mapping[key] = saved[key]

    def __exit__(self, *args):
        """Unpatch the dict."""
        if self._original is not None:
            self._unpatch_dict()
        return False

    def start(self):
        """Activate the patch and return the dict."""
        result = self.__enter__()
        _patch._active_patches.append(self)
        return result

    def stop(self):
        """Undo a patch started with start()."""
        try:
            _patch._active_patches.remove(self)
        except ValueError:
            return None
        return self.__exit__(None, None, None)


def _clear_dict(mapping):
    try:
        mapping.clear()
    except AttributeError:
        for key in list(mapping):
            del mapping[key]


def _patch_stopall():
    """Stop every patch that was started with start()."""
    for active in reversed(_patch._active_patches):
        active.stop()


patch.object = _patch_object
patch.dict = _patch_dict
patch.multiple = _patch_multiple
patch.stopall = _patch_stopall
patch.TEST_PREFIX = 'test'


# ---------------------------------------------------------------------------
# mock_open, PropertyMock, seal
# ---------------------------------------------------------------------------

file_spec = None
open_spec = None


def _to_stream(read_data):
    if isinstance(read_data, bytes):
        return io.BytesIO(read_data)
    return io.StringIO(read_data)


def mock_open(mock=None, read_data=''):
    """Build a mock suitable for replacing ``open()``.

    The returned file handle mock supports read(), readline(), readlines(),
    iteration and use as a context manager, all served from *read_data*
    (str or bytes).  Each call of the open mock restarts the data.
    """
    global file_spec, open_spec
    if file_spec is None:
        import _io
        file_spec = sorted(set(dir(_io.TextIOWrapper)) |
                           set(dir(_io.BytesIO)))
    if open_spec is None:
        import _io
        open_spec = _io.open

    data = {'stream': _to_stream(read_data), 'lines': None}

    if mock is None:
        mock = MagicMock(name='open', spec=open_spec)
    handle = MagicMock(spec=file_spec)
    handle.__enter__.return_value = handle

    def configured(method):
        value = method.return_value
        return value is not None, value

    def do_read(*args, **kwargs):
        overridden, value = configured(handle.read)
        if overridden:
            return value
        return data['stream'].read(*args, **kwargs)

    def do_readlines(*args, **kwargs):
        overridden, value = configured(handle.readlines)
        if overridden:
            return value
        return data['stream'].readlines(*args, **kwargs)

    def line_source():
        overridden, value = configured(handle.readline)
        if overridden:
            while True:
                yield value
        for line in data['stream']:
            yield line

    def readline_results():
        yield from line_source()
        while True:
            yield data['stream'].readline()

    def do_next():
        overridden, value = configured(handle.readline)
        if overridden:
            return value
        return next(data['stream'])

    def do_exit(exc_type, exc, tb):
        handle.close()

    handle.write.return_value = None
    handle.read.return_value = None
    handle.readline.return_value = None
    handle.readlines.return_value = None

    handle.read.side_effect = do_read
    data['lines'] = readline_results()
    handle.readline.side_effect = data['lines']
    handle.readlines.side_effect = do_readlines
    handle.__iter__.side_effect = line_source
    handle.__next__.side_effect = do_next
    handle.__exit__.side_effect = do_exit

    def reopen(*args, **kwargs):
        data['stream'] = _to_stream(read_data)
        # restart readline() unless the user replaced its side effect
        if handle.readline.side_effect == data['lines']:
            data['lines'] = readline_results()
            handle.readline.side_effect = data['lines']
        return DEFAULT

    mock.side_effect = reopen
    mock.return_value = handle
    return mock


class PropertyMock(Mock):
    """A mock meant to be installed as a property on a class: getting the
    attribute calls the mock with no arguments, setting it calls the mock
    with the value."""

    def _get_child_mock(self, /, **kwargs):
        return MagicMock(**kwargs)

    def __get__(self, obj, obj_type=None):
        return self()

    def __set__(self, obj, val):
        self(val)


def seal(mock):
    """Forbid creating new attributes on *mock* and its child mocks.

    Already existing children (and configured return values) keep working;
    reading or setting anything else raises AttributeError.
    """
    mock._mock_sealed = True
    for name in dir(mock):
        try:
            child = getattr(mock, name)
        except AttributeError:
            continue
        if not isinstance(child, NonCallableMock):
            continue
        if isinstance(child._mock_children.get(name), _SpecState):
            continue
        if child._mock_new_parent is mock:
            seal(child)
