"""Python's operators as plain functions.

Every operator of the language has a function here, so that operators can
be passed around as callables: ``operator.add(x, y)`` computes ``x + y``,
``operator.itemgetter(1)(seq)`` computes ``seq[1]``.  Most functions are
also available under the special-method spelling (``__add__`` etc.).

This file is the pure Python version; the _operator accelerator replaces
its functions when available.
"""

__all__ = [
    'abs', 'add', 'and_', 'attrgetter', 'call', 'concat', 'contains',
    'countOf', 'delitem', 'eq', 'floordiv', 'ge', 'getitem', 'gt', 'iadd',
    'iand', 'iconcat', 'ifloordiv', 'ilshift', 'imatmul', 'imod', 'imul',
    'index', 'indexOf', 'inv', 'invert', 'ior', 'ipow', 'irshift', 'is_',
    'is_none', 'is_not', 'is_not_none', 'isub', 'itemgetter', 'itruediv',
    'ixor', 'le', 'length_hint', 'lshift', 'lt', 'matmul', 'methodcaller',
    'mod', 'mul', 'ne', 'neg', 'not_', 'or_', 'pos', 'pow', 'rshift',
    'setitem', 'sub', 'truediv', 'truth', 'xor',
]

from builtins import abs as _builtin_abs


# -- rich comparisons --------------------------------------------------------

def lt(a, b, /):
    "Return a < b."
    return a < b


def le(a, b, /):
    "Return a <= b."
    return a <= b


def eq(a, b, /):
    "Return a == b."
    return a == b


def ne(a, b, /):
    "Return a != b."
    return a != b


def ge(a, b, /):
    "Return a >= b."
    return a >= b


def gt(a, b, /):
    "Return a > b."
    return a > b


# -- truth and identity -------------------------------------------------------

def not_(a, /):
    "Return not a."
    return not a


def truth(a, /):
    "Return bool(a)."
    return bool(a)


def is_(a, b, /):
    "Return a is b."
    return a is b


def is_not(a, b, /):
    "Return a is not b."
    return a is not b


def is_none(a, /):
    "Return a is None."
    return a is None


def is_not_none(a, /):
    "Return a is not None."
    return a is not None


# -- arithmetic and bitwise operators ------------------------------------------

def abs(a, /):
    "Return abs(a)."
    return _builtin_abs(a)


def add(a, b, /):
    "Return a + b."
    return a + b


def sub(a, b, /):
    "Return a - b."
    return a - b


def mul(a, b, /):
    "Return a * b."
    return a * b


def matmul(a, b, /):
    "Return a @ b."
    return a @ b


def truediv(a, b, /):
    "Return a / b."
    return a / b


def floordiv(a, b, /):
    "Return a // b."
    return a // b


def mod(a, b, /):
    "Return a % b."
    return a % b


def pow(a, b, /):
    "Return a ** b."
    return a ** b


def neg(a, /):
    "Return -a."
    return -a


def pos(a, /):
    "Return +a."
    return +a


def invert(a, /):
    "Return ~a."
    return ~a


def inv(a, /):
    "Return ~a (another name for invert)."
    return ~a


def lshift(a, b, /):
    "Return a << b."
    return a << b


def rshift(a, b, /):
    "Return a >> b."
    return a >> b


def and_(a, b, /):
    "Return a & b."
    return a & b


def or_(a, b, /):
    "Return a | b."
    return a | b


def xor(a, b, /):
    "Return a ^ b."
    return a ^ b


def index(a, /):
    "Return a converted to an integer by a.__index__()."
    return a.__index__()


# -- sequences and containers ---------------------------------------------------

def concat(a, b, /):
    "Return a + b for sequences a and b."
    if not hasattr(a, '__getitem__'):
        raise TypeError("'%s' object can't be concatenated"
                        % type(a).__name__)
    return a + b


def contains(a, b, /):
    "Return b in a (note the order of the arguments)."
    return b in a


def countOf(a, b, /):
    "Return how many items of a are b or equal to b."
    n = 0
    for item in a:
        if item is b or item == b:
            n += 1
    return n


def indexOf(a, b, /):
    "Return the position of the first item of a that is b or equal to b."
    for position, item in enumerate(a):
        if item is b or item == b:
            return position
    raise ValueError('sequence.index(x): x not in sequence')


def getitem(a, b, /):
    "Return a[b]."
    return a[b]


def setitem(a, b, c, /):
    "Do a[b] = c."
    a[b] = c


def delitem(a, b, /):
    "Do del a[b]."
    del a[b]


def length_hint(obj, default=0, /):
    """Return len(obj) if it has a length, else the estimate from
    obj.__length_hint__(), else default.  The result is a non-negative
    int."""
    if not isinstance(default, int):
        raise TypeError("'%s' object cannot be interpreted as an integer"
                        % type(default).__name__)
    try:
        return len(obj)
    except TypeError:
        pass
    try:
        hint = type(obj).__length_hint__
    except AttributeError:
        return default
    try:
        estimate = hint(obj)
    except TypeError:
        return default
    if estimate is NotImplemented:
        return default
    if not isinstance(estimate, int):
        raise TypeError('__length_hint__ must be integer, not %s'
                        % type(estimate).__name__)
    if estimate < 0:
        raise ValueError('__length_hint__() should return >= 0')
    return estimate


def call(obj, /, *args, **kwargs):
    "Return obj(*args, **kwargs)."
    return obj(*args, **kwargs)


# -- in-place operators -----------------------------------------------------------

def iadd(a, b, /):
    "Do a += b and return a."
    a += b
    return a


def isub(a, b, /):
    "Do a -= b and return a."
    a -= b
    return a


def imul(a, b, /):
    "Do a *= b and return a."
    a *= b
    return a


def imatmul(a, b, /):
    "Do a @= b and return a."
    a @= b
    return a


def itruediv(a, b, /):
    "Do a /= b and return a."
    a /= b
    return a


def ifloordiv(a, b, /):
    "Do a //= b and return a."
    a //= b
    return a


def imod(a, b, /):
    "Do a %= b and return a."
    a %= b
    return a


def ipow(a, b, /):
    "Do a **= b and return a."
    a **= b
    return a


def ilshift(a, b, /):
    "Do a <<= b and return a."
    a <<= b
    return a


def irshift(a, b, /):
    "Do a >>= b and return a."
    a >>= b
    return a


def iand(a, b, /):
    "Do a &= b and return a."
    a &= b
    return a


def ior(a, b, /):
    "Do a |= b and return a."
    a |= b
    return a


def ixor(a, b, /):
    "Do a ^= b and return a."
    a ^= b
    return a


def iconcat(a, b, /):
    "Do a += b for sequences a and b and return a."
    if not hasattr(a, '__getitem__'):
        raise TypeError("'%s' object can't be concatenated"
                        % type(a).__name__)
    a += b
    return a


# -- callable getters -------------------------------------------------------------

class attrgetter:
    """Callable fetching attributes: attrgetter('a')(obj) is obj.a,
    attrgetter('a', 'b.c')(obj) is (obj.a, obj.b.c)."""

    __slots__ = ('_attrs', '_fetch')

    @staticmethod
    def _path_getter(names):
        # names: the parts of one dotted attribute path
        if len(names) == 1:
            name = names[0]
            return lambda obj: getattr(obj, name)

        def resolve(obj):
            for name in names:
                obj = getattr(obj, name)
            return obj
        return resolve

    def __init__(self, attr, /, *attrs):
        names = (attr,) + attrs
        for name in names:
            if not isinstance(name, str):
                raise TypeError('attribute name must be a string')
        self._attrs = names
        getters = [self._path_getter(name.split('.')) for name in names]
        if len(getters) == 1:
            self._fetch = getters[0]
        else:
            self._fetch = lambda obj: tuple([g(obj) for g in getters])

    def __call__(self, obj, /):
        return self._fetch(obj)

    def __repr__(self):
        return '%s.%s(%s)' % (type(self).__module__, type(self).__qualname__,
                                ', '.join(map(repr, self._attrs)))

    def __reduce__(self):
        return type(self), self._attrs


class itemgetter:
    """Callable fetching items: itemgetter(2)(seq) is seq[2],
    itemgetter(2, 5)(seq) is (seq[2], seq[5])."""

    __slots__ = ('_items', '_fetch')

    def __init__(self, item, /, *items):
        keys = (item,) + items
        self._items = keys
        if items:
            self._fetch = lambda obj: tuple([obj[k] for k in keys])
        else:
            self._fetch = lambda obj: obj[item]

    def __call__(self, obj, /):
        return self._fetch(obj)

    def __repr__(self):
        return '%s.%s(%s)' % (type(self).__module__, type(self).__qualname__,
                                ', '.join(map(repr, self._items)))

    def __reduce__(self):
        return type(self), self._items


class methodcaller:
    """Callable calling a method: methodcaller('m', 1, k=2)(obj) is
    obj.m(1, k=2)."""

    __slots__ = ('_name', '_args', '_kwargs')

    def __init__(self, name, /, *args, **kwargs):
        if not isinstance(name, str):
            raise TypeError('method name must be a string')
        self._name = name
        self._args = args
        self._kwargs = kwargs

    def __call__(self, obj, /):
        return getattr(obj, self._name)(*self._args, **self._kwargs)

    def __repr__(self):
        parts = [repr(self._name)]
        parts += map(repr, self._args)
        parts += ['%s=%r' % item for item in self._kwargs.items()]
        return '%s.%s(%s)' % (type(self).__module__, type(self).__qualname__,
                                ', '.join(parts))

    def __reduce__(self):
        if self._kwargs:
            # keyword arguments cannot be passed through a plain reduce tuple
            from functools import partial
            return (partial(type(self), self._name, **self._kwargs),
                    self._args)
        return type(self), (self._name,) + self._args


try:
    from _operator import *
    from _operator import __doc__
except ImportError:
    pass


# Special-method spellings.  They are bound after the accelerator import so
# that they name the same objects as the plain names.
for _name in ('lt le eq ne ge gt not_ abs add and_ call floordiv index inv '
              'invert lshift mod mul matmul neg or_ pos pow rshift sub '
              'truediv xor concat contains delitem getitem setitem iadd iand '
              'iconcat ifloordiv ilshift imod imul imatmul ior ipow irshift '
              'isub itruediv ixor').split():
    globals()['__%s__' % _name.rstrip('_')] = globals()[_name]
del _name
