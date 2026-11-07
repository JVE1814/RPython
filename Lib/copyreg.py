"""Registry of pickle support functions, plus the helpers pickle and copy
use to reduce objects.

pickle(type, function) registers a reduction function for instances of an
(extension) type; the extension-code functions maintain the compact codes of
pickle protocol 2.  Classes written in Python do not need this module: they
pickle through __reduce_ex__ / __getstate__.
"""

__all__ = ["pickle", "constructor",
           "add_extension", "remove_extension", "clear_extension_cache"]

# type -> function(obj) returning a reduce tuple; consulted by pickle and copy
dispatch_table = {}


def pickle(ob_type, pickle_function, constructor_ob=None):
    """Make pickle and copy reduce instances of ob_type with
    pickle_function.  constructor_ob is only checked for callability."""
    if not callable(pickle_function):
        raise TypeError("reduction functions must be callable")
    dispatch_table[ob_type] = pickle_function
    if constructor_ob is not None:
        constructor(constructor_ob)


def constructor(object):
    """Check that object can serve as a constructor (kept for old code)."""
    if not callable(object):
        raise TypeError("constructors must be callable")


# Built-in types without a reduce method of their own.

def pickle_complex(c):
    return complex, (c.real, c.imag)


def pickle_super(obj):
    return super, (obj.__thisclass__, obj.__self__)


pickle(complex, pickle_complex, complex)
pickle(super, pickle_super)


# -- protocols 0 and 1 ---------------------------------------------------------
# object.__reduce_ex__ calls _reduce_ex for these protocols.  The object is
# rebuilt by _reconstructor from its class, the nearest native base class and
# that base's view of the object, then its state is restored.

_HEAPTYPE = 1 << 9
_builtin_new = type(int.__new__)


def _reconstructor(cls, base, state):
    if base is object:
        try:
            return object.__new__(cls)
        except TypeError:
            # a native base that _reduce_ex passed over needs its own __new__
            return cls.__new__(cls)
    obj = base.__new__(cls, state)
    if base.__init__ != object.__init__:
        base.__init__(obj, state)
    return obj


def _native_base(cls):
    """The base class whose layout the instances of cls really have."""
    for base in cls.__mro__:
        native = hasattr(base, '__flags__') and not base.__flags__ & _HEAPTYPE
        if native:
            # types from extension modules count as ordinary classes here
            # (CPython creates them as heap types)
            if base is cls or base.__module__ == 'builtins':
                return base
            continue
        new = base.__new__
        if isinstance(new, _builtin_new) and new.__self__ is base:
            return base
    return object


def _reduce_ex(self, proto):
    assert proto < 2
    cls = self.__class__
    base = _native_base(cls)
    if base is object:
        base_state = None
    elif base is cls:
        raise TypeError(f"cannot pickle {cls.__name__!r} object")
    else:
        base_state = base(self)
    args = (cls, base, base_state)
    has_slots = bool(getattr(self, "__slots__", None))
    try:
        getstate = self.__getstate__
    except AttributeError:
        if has_slots:
            raise TypeError(f"cannot pickle {cls.__name__!r} object: a class "
                            f"that defines __slots__ without defining "
                            f"__getstate__ cannot be pickled with protocol "
                            f"{proto}") from None
        state = getattr(self, '__dict__', None)
    else:
        if has_slots and type(self).__getstate__ is object.__getstate__:
            raise TypeError("a class that defines __slots__ without defining "
                            "__getstate__ cannot be pickled")
        state = getstate()
    if state:
        return _reconstructor, args, state
    return _reconstructor, args


# -- protocol 2 and later ------------------------------------------------------

def __newobj__(cls, *args):
    return cls.__new__(cls, *args)


def __newobj_ex__(cls, args, kwargs):
    """Like __newobj__, with keyword arguments (pickle protocol 4)."""
    return cls.__new__(cls, *args, **kwargs)


def _slotnames(cls):
    """List the instance slot names of cls and its bases (with private
    names mangled), cached as cls.__slotnames__."""
    cached = cls.__dict__.get("__slotnames__")
    if cached is not None:
        return cached
    names = []
    if hasattr(cls, "__slots__"):
        for klass in cls.__mro__:
            declared = klass.__dict__.get("__slots__")
            if declared is None:
                continue
            if isinstance(declared, str):
                declared = (declared,)
            for name in declared:
                if name in ("__dict__", "__weakref__"):
                    continue
                if name.startswith('__') and not name.endswith('__'):
                    owner = klass.__name__.lstrip('_')
                    if owner:
                        name = '_%s%s' % (owner, name)
                names.append(name)
    try:
        cls.__slotnames__ = names
    except Exception:
        pass            # immutable (built-in) classes cannot cache it
    return names


# -- extension codes -------------------------------------------------------------
# Protocol 2 can refer to a registered global by a small integer code instead
# of by module and name.  1-127 are meant for the standard library, 128-191
# for third parties and 192-239 for private use.

_extension_registry = {}        # (module, name) -> code
_inverted_registry = {}         # code -> (module, name)
_extension_cache = {}           # code -> unpickled object


def add_extension(module, name, code):
    """Register code as the extension code of module.name."""
    code = int(code)
    if not 1 <= code <= 0x7fffffff:
        raise ValueError("code out of range")
    key = (module, name)
    if (_extension_registry.get(key) == code
            and _inverted_registry.get(code) == key):
        return                  # registering the same pair again is harmless
    if key in _extension_registry:
        raise ValueError("key %s is already registered with code %s"
                         % (key, _extension_registry[key]))
    if code in _inverted_registry:
        raise ValueError("code %s is already in use for key %s"
                         % (code, _inverted_registry[code]))
    _extension_registry[key] = code
    _inverted_registry[code] = key


def remove_extension(module, name, code):
    """Unregister an extension code (meant for tests)."""
    key = (module, name)
    if (_extension_registry.get(key) != code
            or _inverted_registry.get(code) != key):
        raise ValueError("key %s is not registered with code %s"
                         % (key, code))
    del _extension_registry[key]
    del _inverted_registry[code]
    _extension_cache.pop(code, None)


def clear_extension_cache():
    """Forget the objects loaded through extension codes."""
    _extension_cache.clear()
