'''Container datatypes that complement the builtin dict, list, set and tuple.

* namedtuple   factory for tuple subclasses with named fields
* deque        double-ended queue with fast appends/pops at both ends
* ChainMap     a single view over a sequence of mappings
* Counter      dict subclass mapping elements to counts
* OrderedDict  dict subclass with order-aware operations
* defaultdict  dict subclass that creates missing values with a factory
* UserDict     dict wrapper meant for subclassing
* UserList     list wrapper meant for subclassing
* UserString   str wrapper meant for subclassing
'''

__all__ = [
    'ChainMap',
    'Counter',
    'OrderedDict',
    'UserDict',
    'UserList',
    'UserString',
    'defaultdict',
    'deque',
    'namedtuple',
]

import _collections_abc
import sys as _sys

_sys.modules['collections.abc'] = _collections_abc
abc = _collections_abc

from itertools import chain as _chain
from itertools import repeat as _repeat
from itertools import starmap as _starmap
from keyword import iskeyword as _iskeyword
from operator import eq as _eq
from operator import itemgetter as _itemgetter
from reprlib import recursive_repr as _recursive_repr

try:
    from _collections import deque
except ImportError:
    pass
else:
    _collections_abc.MutableSequence.register(deque)

try:
    # needed so that pickled deque iterators can be restored
    from _collections import _deque_iterator  # noqa: F401
except ImportError:
    pass

try:
    from _collections import defaultdict
except ImportError:
    pass


# ---------------------------------------------------------------------------
# OrderedDict

class _OrderedDictKeysView(_collections_abc.KeysView):

    def __reversed__(self):
        yield from reversed(self._mapping)

class _OrderedDictItemsView(_collections_abc.ItemsView):

    def __reversed__(self):
        m = self._mapping
        for k in reversed(m):
            yield (k, m[k])

class _OrderedDictValuesView(_collections_abc.ValuesView):

    def __reversed__(self):
        m = self._mapping
        for k in reversed(m):
            yield m[k]

class _Link(object):
    __slots__ = 'prev', 'next', 'key'


class OrderedDict(dict):
    'Dictionary that remembers insertion order'
    # Keys are additionally threaded on a circular doubly linked list of
    # _Link nodes; self.__map maps each key to its node and self.__root is the
    # sentinel node.  Values live only in the dict itself.

    def __new__(cls, /, *args, **kwds):
        "Create the ordered dict object and set up the underlying structures."
        self = dict.__new__(cls)
        sentinel = _Link()
        sentinel.prev = sentinel.next = sentinel
        self.__hardroot = sentinel
        self.__root = sentinel
        self.__map = {}
        return self

    def __init__(self, other=(), /, **kwds):
        '''Initialize an ordered dictionary.  The signature is the same as
        regular dictionaries.  Keyword argument order is preserved.
        '''
        self.__update(other, **kwds)

    def __setitem__(self, key, value,
                    dict_setitem=dict.__setitem__, Link=_Link):
        'od.__setitem__(i, y) <==> od[i]=y'
        if key not in self:
            node = Link()
            self.__map[key] = node
            root = self.__root
            tail = root.prev
            node.prev = tail
            node.next = root
            node.key = key
            tail.next = node
            root.prev = node
        dict_setitem(self, key, value)

    def __unlink(self, key):
        node = self.__map.pop(key)
        before, after = node.prev, node.next
        before.next = after
        after.prev = before
        node.prev = node.next = None

    def __delitem__(self, key, dict_delitem=dict.__delitem__):
        'od.__delitem__(y) <==> del od[y]'
        dict_delitem(self, key)
        self.__unlink(key)

    def __iter__(self):
        'od.__iter__() <==> iter(od)'
        root = self.__root
        node = root.next
        while node is not root:
            yield node.key
            node = node.next

    def __reversed__(self):
        'od.__reversed__() <==> reversed(od)'
        root = self.__root
        node = root.prev
        while node is not root:
            yield node.key
            node = node.prev

    def clear(self):
        'od.clear() -> None.  Remove all items from od.'
        root = self.__root
        root.prev = root.next = root
        self.__map.clear()
        dict.clear(self)

    def popitem(self, last=True):
        '''Remove and return a (key, value) pair from the dictionary.

        Pairs are returned in LIFO order if last is true or FIFO order if false.
        '''
        if not self:
            raise KeyError('dictionary is empty')
        root = self.__root
        node = root.prev if last else root.next
        key = node.key
        self.__unlink(key)
        value = dict.pop(self, key)
        return key, value

    def move_to_end(self, key, last=True):
        '''Move an existing element to the end (or beginning if last is false).

        Raise KeyError if the element does not exist.
        '''
        node = self.__map[key]
        before, after = node.prev, node.next
        before.next = after
        after.prev = before
        root = self.__root
        if last:
            tail = root.prev
            node.prev = tail
            node.next = root
            tail.next = node
            root.prev = node
        else:
            head = root.next
            node.prev = root
            node.next = head
            head.prev = node
            root.next = node

    def __sizeof__(self):
        sizeof = _sys.getsizeof
        nodes = len(self) + 1
        return (sizeof(self.__dict__) + 2 * sizeof(self.__map)
                + 2 * nodes * sizeof(self.__hardroot))

    update = __update = _collections_abc.MutableMapping.update

    def keys(self):
        "D.keys() -> a set-like object providing a view on D's keys"
        return _OrderedDictKeysView(self)

    def items(self):
        "D.items() -> a set-like object providing a view on D's items"
        return _OrderedDictItemsView(self)

    def values(self):
        "D.values() -> an object providing a view on D's values"
        return _OrderedDictValuesView(self)

    __ne__ = _collections_abc.MutableMapping.__ne__

    __marker = object()

    def pop(self, key, default=__marker):
        '''od.pop(k[,d]) -> v, remove specified key and return the corresponding
        value.  If key is not found, d is returned if given, otherwise KeyError
        is raised.

        '''
        missing = self.__marker
        value = dict.pop(self, key, missing)
        if value is not missing:
            self.__unlink(key)
            return value
        if default is missing:
            raise KeyError(key)
        return default

    def setdefault(self, key, default=None):
        '''Insert key with a value of default if key is not in the dictionary.

        Return the value for key if key is in the dictionary, else default.
        '''
        if key in self:
            return self[key]
        self[key] = default
        return default

    @_recursive_repr()
    def __repr__(self):
        'od.__repr__() <==> repr(od)'
        name = self.__class__.__name__
        if not self:
            return f'{name}()'
        return f'{name}({dict(self.items())!r})'

    def __reduce__(self):
        'Return state information for pickling'
        state = self.__getstate__()
        if state:
            if isinstance(state, tuple):
                state, slots = state
            else:
                slots = {}
            state = state.copy()
            slots = slots.copy()
            # drop our private bookkeeping attributes
            for k in vars(OrderedDict()):
                state.pop(k, None)
                slots.pop(k, None)
            if slots:
                state = state, slots
            else:
                state = state or None
        return self.__class__, (), state, None, iter(self.items())

    def copy(self):
        'od.copy() -> a shallow copy of od'
        return self.__class__(self)

    @classmethod
    def fromkeys(cls, iterable, value=None):
        '''Create a new ordered dictionary with keys from iterable and values set to value.
        '''
        new = cls()
        for k in iterable:
            new[k] = value
        return new

    def __eq__(self, other):
        '''od.__eq__(y) <==> od==y.  Comparison to another OD is order-sensitive
        while comparison to a regular mapping is order-insensitive.

        '''
        if isinstance(other, OrderedDict):
            return dict.__eq__(self, other) and all(map(_eq, self, other))
        return dict.__eq__(self, other)

    def __ior__(self, other):
        self.update(other)
        return self

    def __or__(self, other):
        if not isinstance(other, (dict, frozendict)):
            return NotImplemented
        new = self.__class__(self)
        new.update(other)
        return new

    def __ror__(self, other):
        if not isinstance(other, (dict, frozendict)):
            return NotImplemented
        new = self.__class__(other)
        new.update(self)
        return new


try:
    from _collections import OrderedDict
except ImportError:
    pass


# ---------------------------------------------------------------------------
# namedtuple

try:
    from _collections import _tuplegetter
except ImportError:
    _tuplegetter = lambda index, doc: property(_itemgetter(index), doc=doc)

_field_docs = {}

def _field_doc(index):
    # All namedtuple classes share one docstring object per field position.
    doc = _field_docs.get(index)
    if doc is None:
        doc = _field_docs[index] = _sys.intern(f'Alias for field number {index}')
    return doc


def _validate_names(typename, field_names, rename):
    if rename:
        used = set()
        for i, name in enumerate(field_names):
            if (not name.isidentifier() or _iskeyword(name)
                    or name.startswith('_') or name in used):
                field_names[i] = f'_{i}'
            used.add(name)
    for name in [typename, *field_names]:
        if type(name) is not str:
            raise TypeError('Type names and field names must be strings')
        if not name.isidentifier():
            raise ValueError('Type names and field names must be valid '
                             f'identifiers: {name!r}')
        if _iskeyword(name):
            raise ValueError('Type names and field names cannot be a '
                             f'keyword: {name!r}')
    used = set()
    for name in field_names:
        if name.startswith('_') and not rename:
            raise ValueError('Field names cannot start with an underscore: '
                             f'{name!r}')
        if name in used:
            raise ValueError(f'Encountered duplicate field name: {name!r}')
        used.add(name)


def namedtuple(typename, field_names, *, rename=False, defaults=None, module=None):
    """Create a tuple subclass whose items are also accessible by name.

    >>> Pair = namedtuple('Pair', 'left right')
    >>> Pair.__doc__
    'Pair(left, right)'
    >>> p = Pair(3, right=4)
    >>> p
    Pair(left=3, right=4)
    >>> p.left * p.right, p[0] + p[1]
    (12, 7)
    >>> a, b = p
    >>> b
    4
    >>> p._asdict()
    {'left': 3, 'right': 4}
    >>> p._replace(right=10)
    Pair(left=3, right=10)
    >>> Pair._make([5, 6])
    Pair(left=5, right=6)

    """
    if isinstance(field_names, str):
        field_names = field_names.replace(',', ' ').split()
    field_names = list(map(str, field_names))
    typename = _sys.intern(str(typename))
    _validate_names(typename, field_names, rename)

    field_defaults = {}
    if defaults is not None:
        defaults = tuple(defaults)
        if len(defaults) > len(field_names):
            raise TypeError('Got more default values than field names')
        field_defaults = dict(zip(field_names[len(field_names) - len(defaults):],
                                  defaults))

    field_names = tuple(map(_sys.intern, field_names))
    num_fields = len(field_names)
    arg_list = ', '.join(field_names)
    if num_fields == 1:
        arg_list += ','
    repr_fmt = '(' + ', '.join(f'{name}=%r' for name in field_names) + ')'
    tuple_new = tuple.__new__
    _dict, _tuple, _len, _map, _zip = dict, tuple, len, map, zip

    # __new__ needs a real signature with the field names as parameters, so it
    # is compiled from source.  Only validated identifiers reach this string.
    ns = {'_tuple_new': tuple_new, '__builtins__': {},
          '__name__': f'namedtuple_{typename}'}
    __new__ = eval(f'lambda _cls, {arg_list}: _tuple_new(_cls, ({arg_list}))', ns)
    __new__.__name__ = '__new__'
    __new__.__doc__ = f'Create new instance of {typename}({arg_list})'
    if defaults is not None:
        __new__.__defaults__ = defaults

    @classmethod
    def _make(cls, iterable):
        result = tuple_new(cls, iterable)
        if _len(result) != num_fields:
            raise TypeError(f'Expected {num_fields} arguments, got {len(result)}')
        return result

    _make.__func__.__doc__ = (f'Make a new {typename} object from a sequence '
                              'or iterable')

    def _replace(self, /, **kwds):
        result = self._make(_map(kwds.pop, field_names, self))
        if kwds:
            raise TypeError(f'Got unexpected field names: {list(kwds)!r}')
        return result

    _replace.__doc__ = (f'Return a new {typename} object replacing specified '
                        'fields with new values')

    def __repr__(self):
        'Return a nicely formatted representation string'
        return self.__class__.__name__ + repr_fmt % self

    def _asdict(self):
        'Return a new dict which maps field names to their values.'
        return _dict(_zip(self._fields, self))

    def __getnewargs__(self):
        'Return self as a plain tuple.  Used by copy and pickle.'
        return _tuple(self)

    for method in (__new__, _make.__func__, _replace, __repr__, _asdict,
                   __getnewargs__):
        method.__qualname__ = f'{typename}.{method.__name__}'

    namespace = {
        '__doc__': f'{typename}({arg_list})',
        '__slots__': (),
        '_fields': field_names,
        '_field_defaults': field_defaults,
        '__new__': __new__,
        '_make': _make,
        '__replace__': _replace,
        '_replace': _replace,
        '__repr__': __repr__,
        '_asdict': _asdict,
        '__getnewargs__': __getnewargs__,
        '__match_args__': field_names,
    }
    for index, name in enumerate(field_names):
        namespace[name] = _tuplegetter(index, _field_doc(index))

    result = type(typename, (tuple,), namespace)

    # Pickling needs __module__ to name the module that created the class.
    if module is None:
        try:
            module = _sys._getframemodulename(1) or '__main__'
        except AttributeError:
            try:
                module = _sys._getframe(1).f_globals.get('__name__', '__main__')
            except (AttributeError, ValueError):
                pass
    if module is not None:
        result.__module__ = module

    return result


# ---------------------------------------------------------------------------
# Counter

def _count_elements(mapping, iterable):
    'Tally elements from the iterable.'
    get = mapping.get
    for elem in iterable:
        mapping[elem] = get(elem, 0) + 1

try:
    from _collections import _count_elements
except ImportError:
    pass


class Counter(dict):
    '''Dict subclass mapping hashable elements to their counts (a multiset).

    >>> c = Counter('mississippi')
    >>> c['s'], c['z']
    (4, 0)
    >>> c.most_common(2)
    [('i', 4), ('s', 4)]
    >>> sorted(c.elements())
    ['i', 'i', 'i', 'i', 'm', 'p', 'p', 's', 's', 's', 's']
    >>> c.total()
    11
    >>> c.update('sip')
    >>> c['p']
    3
    >>> del c['s']
    >>> c['s']
    0
    >>> c.clear()
    >>> c
    Counter()

    Zero and negative counts are kept until the entry is deleted:

    >>> c = Counter(x=2, y=1)
    >>> c['y'] -= 1
    >>> c.most_common()
    [('x', 2), ('y', 0)]

    '''

    def __init__(self, iterable=None, /, **kwds):
        '''Create a Counter, optionally counting the elements of an iterable
        or taking initial counts from a mapping or from keyword arguments.

        >>> Counter('abca')['a']
        2
        >>> Counter({'red': 3})['red']
        3
        >>> Counter(blue=2)['blue']
        2

        '''
        super().__init__()
        self.update(iterable, **kwds)

    def __missing__(self, key):
        'The count of elements not in the Counter is zero.'
        return 0

    def total(self):
        'Sum of the counts'
        return sum(self.values())

    def most_common(self, n=None):
        '''List the n most common elements and their counts from the most
        common to the least.  If n is None, then list all element counts.

        >>> Counter('banana').most_common(2)
        [('a', 3), ('n', 2)]

        '''
        if n is None:
            return sorted(self.items(), key=_itemgetter(1), reverse=True)
        import heapq
        return heapq.nlargest(n, self.items(), key=_itemgetter(1))

    def elements(self):
        '''Iterator over elements repeating each as many times as its count.

        >>> sorted(Counter(x=2, y=1).elements())
        ['x', 'x', 'y']
        >>> import math
        >>> math.prod(Counter({3: 2, 5: 1}).elements())
        45

        Elements with a count of zero or less are skipped.

        '''
        return _chain.from_iterable(_starmap(_repeat, self.items()))

    @classmethod
    def fromkeys(cls, iterable, v=None):
        # Ambiguous for counters: use Counter(iterable) instead.
        raise NotImplementedError(
            'Counter.fromkeys() is undefined.  Use Counter(iterable) instead.')

    def update(self, iterable=None, /, **kwds):
        '''Like dict.update() but add counts instead of replacing them.

        Source can be an iterable, a dictionary, or another Counter instance.

        >>> c = Counter('ab')
        >>> c.update('abb')
        >>> c['b']
        3

        '''
        if iterable is not None:
            if isinstance(iterable, _collections_abc.Mapping):
                if self:
                    get = self.get
                    for elem, count in iterable.items():
                        self[elem] = count + get(elem, 0)
                else:
                    super().update(iterable)
            else:
                _count_elements(self, iterable)
        if kwds:
            self.update(kwds)

    def subtract(self, iterable=None, /, **kwds):
        '''Like dict.update() but subtracts counts instead of replacing them.
        Counts can be reduced below zero.  Both the inputs and outputs are
        allowed to contain zero and negative counts.

        Source can be an iterable, a dictionary, or another Counter instance.

        >>> c = Counter('ab')
        >>> c.subtract('bbc')
        >>> c['b'], c['c']
        (-1, -1)

        '''
        if iterable is not None:
            get = self.get
            if isinstance(iterable, _collections_abc.Mapping):
                for elem, count in iterable.items():
                    self[elem] = get(elem, 0) - count
            else:
                for elem in iterable:
                    self[elem] = get(elem, 0) - 1
        if kwds:
            self.subtract(kwds)

    def copy(self):
        'Return a shallow copy.'
        return self.__class__(self)

    def __reduce__(self):
        return self.__class__, (dict(self),)

    def __delitem__(self, elem):
        'Like dict.__delitem__() but does not raise KeyError for missing values.'
        if elem in self:
            super().__delitem__(elem)

    def __repr__(self):
        name = self.__class__.__name__
        if not self:
            return f'{name}()'
        try:
            d = dict(self.most_common())
        except TypeError:
            # counts that cannot be ordered
            d = dict(self)
        return f'{name}({d!r})'

    # Rich comparisons treat missing elements as having a zero count.

    def _all_counts(self, other, op):
        return all(op(self[e], other[e]) for c in (self, other) for e in c)

    def __eq__(self, other):
        'True if all counts agree. Missing counts are treated as zero.'
        if not isinstance(other, Counter):
            return NotImplemented
        return self._all_counts(other, _eq)

    def __ne__(self, other):
        'True if any counts disagree. Missing counts are treated as zero.'
        if not isinstance(other, Counter):
            return NotImplemented
        return not self == other

    def __le__(self, other):
        'True if all counts in self are a subset of those in other.'
        if not isinstance(other, Counter):
            return NotImplemented
        return self._all_counts(other, lambda a, b: a <= b)

    def __lt__(self, other):
        'True if all counts in self are a proper subset of those in other.'
        if not isinstance(other, Counter):
            return NotImplemented
        return self <= other and self != other

    def __ge__(self, other):
        'True if all counts in self are a superset of those in other.'
        if not isinstance(other, Counter):
            return NotImplemented
        return self._all_counts(other, lambda a, b: a >= b)

    def __gt__(self, other):
        'True if all counts in self are a proper superset of those in other.'
        if not isinstance(other, Counter):
            return NotImplemented
        return self >= other and self != other

    # Multiset arithmetic: results only keep positive counts and are ordered
    # by first appearance in the left operand, then in the right one.

    def _combine(self, other, op, keep_other_only):
        result = Counter()
        for elem, count in self.items():
            n = op(count, other[elem])
            if n > 0:
                result[elem] = n
        if keep_other_only is not None:
            for elem, count in other.items():
                if elem not in self:
                    n = keep_other_only(count)
                    if n > 0:
                        result[elem] = n
        return result

    def __add__(self, other):
        '''Add counts from two counters.

        >>> Counter('aab') + Counter('bc')
        Counter({'a': 2, 'b': 2, 'c': 1})

        '''
        if not isinstance(other, Counter):
            return NotImplemented
        return self._combine(other, lambda a, b: a + b, lambda b: b)

    def __sub__(self, other):
        '''Subtract counts, keeping only positive results.

        >>> Counter('aab') - Counter('abc')
        Counter({'a': 1})

        '''
        if not isinstance(other, Counter):
            return NotImplemented
        return self._combine(other, lambda a, b: a - b, lambda b: 0 - b)

    def __or__(self, other):
        '''Union is the maximum of value in either of the input counters.

        >>> Counter('aab') | Counter('abbc')
        Counter({'a': 2, 'b': 2, 'c': 1})

        '''
        if not isinstance(other, Counter):
            return NotImplemented
        return self._combine(other, lambda a, b: b if a < b else a, lambda b: b)

    def __and__(self, other):
        '''Intersection is the minimum of corresponding counts.

        >>> Counter('aab') & Counter('abbc')
        Counter({'a': 1, 'b': 1})

        '''
        if not isinstance(other, Counter):
            return NotImplemented
        return self._combine(other, lambda a, b: a if a < b else b, None)

    def __xor__(self, other):
        '''Symmetric difference: the absolute difference of the counts.

        >>> Counter('aab') ^ Counter('abbc')
        Counter({'a': 1, 'b': 1, 'c': 1})

        '''
        if not isinstance(other, Counter):
            return NotImplemented
        return self._combine(other, lambda a, b: abs(a - b), lambda b: b)

    def __pos__(self):
        'Adds an empty counter, effectively stripping negative and zero counts'
        result = Counter()
        for elem, count in self.items():
            if count > 0:
                result[elem] = count
        return result

    def __neg__(self):
        '''Subtracts from an empty counter.  Strips positive and zero counts,
        and flips the sign on negative counts.

        '''
        result = Counter()
        for elem, count in self.items():
            if count < 0:
                result[elem] = 0 - count
        return result

    def _keep_positive(self):
        '''Internal method to strip elements with a negative or zero count'''
        for elem in [e for e, c in self.items() if not c > 0]:
            del self[elem]
        return self

    def __iadd__(self, other):
        '''Inplace add from another counter, keeping only positive counts.

        >>> c = Counter('ab')
        >>> c += Counter('b')
        >>> c
        Counter({'b': 2, 'a': 1})

        '''
        for elem, count in other.items():
            self[elem] += count
        return self._keep_positive()

    def __isub__(self, other):
        '''Inplace subtract counter, but keep only results with positive counts.

        >>> c = Counter('abb')
        >>> c -= Counter('ab')
        >>> c
        Counter({'b': 1})

        '''
        for elem, count in other.items():
            self[elem] -= count
        return self._keep_positive()

    def __ior__(self, other):
        '''Inplace union is the maximum of value from either counter.

        >>> c = Counter('ab')
        >>> c |= Counter('bb')
        >>> c
        Counter({'b': 2, 'a': 1})

        '''
        for elem, other_count in other.items():
            if other_count > self[elem]:
                self[elem] = other_count
        return self._keep_positive()

    def __iand__(self, other):
        '''Inplace intersection is the minimum of corresponding counts.

        >>> c = Counter('abb')
        >>> c &= Counter('bc')
        >>> c
        Counter({'b': 1})

        '''
        for elem, count in self.items():
            other_count = other[elem]
            if other_count < count:
                self[elem] = other_count
        return self._keep_positive()

    def __ixor__(self, other):
        '''Inplace symmetric difference: the absolute difference of the counts.

        >>> c = Counter('abb')
        >>> c ^= Counter('bc')
        >>> c
        Counter({'a': 1, 'b': 1, 'c': 1})

        '''
        for elem, other_count in other.items():
            self[elem] = abs(self[elem] - other_count)
        return self._keep_positive()


# ---------------------------------------------------------------------------
# ChainMap

class ChainMap(_collections_abc.MutableMapping):
    '''A view combining several mappings into one.

    Lookups try each mapping of the public list ``maps`` in order; writes,
    updates and deletions only touch ``maps[0]``.
    '''

    def __init__(self, *maps):
        '''Initialize a ChainMap by setting *maps* to the given mappings.
        If no mappings are provided, a single empty dictionary is used.

        '''
        self.maps = list(maps) or [{}]

    def __missing__(self, key):
        raise KeyError(key)

    def __getitem__(self, key):
        for m in self.maps:
            try:
                # `key in m` would bypass __missing__ of defaultdict & co.
                return m[key]
            except KeyError:
                pass
        return self.__missing__(key)

    def get(self, key, default=None):
        return self[key] if key in self else default

    def __len__(self):
        return len(set().union(*self.maps))

    def __iter__(self):
        merged = {}
        for m in reversed(self.maps):
            merged |= dict.fromkeys(m)
        return iter(merged)

    def __contains__(self, key):
        return any(key in m for m in self.maps)

    def __bool__(self):
        return any(self.maps)

    @_recursive_repr()
    def __repr__(self):
        return f'{self.__class__.__name__}({", ".join(map(repr, self.maps))})'

    @classmethod
    def fromkeys(cls, iterable, value=None, /):
        'Create a new ChainMap with keys from iterable and values set to value.'
        return cls(dict.fromkeys(iterable, value))

    def copy(self):
        'New ChainMap or subclass with a new copy of maps[0] and refs to maps[1:]'
        return self.__class__(self.maps[0].copy(), *self.maps[1:])

    __copy__ = copy

    def new_child(self, m=None, **kwargs):
        '''New ChainMap with a new map followed by all previous maps.
        If no map is provided, an empty dict is used.
        Keyword arguments update the map or new empty dict.
        '''
        if m is None:
            m = kwargs
        elif kwargs:
            m.update(kwargs)
        return self.__class__(m, *self.maps)

    @property
    def parents(self):
        'New ChainMap from maps[1:].'
        return self.__class__(*self.maps[1:])

    def __setitem__(self, key, value):
        self.maps[0][key] = value

    def __delitem__(self, key):
        try:
            del self.maps[0][key]
        except KeyError:
            raise KeyError(f'Key not found in the first mapping: {key!r}')

    def popitem(self):
        'Remove and return an item pair from maps[0]. Raise KeyError if maps[0] is empty.'
        try:
            return self.maps[0].popitem()
        except KeyError:
            raise KeyError('No keys found in the first mapping.')

    def pop(self, key, *args):
        'Remove *key* from maps[0] and return its value. Raise KeyError if *key* not in maps[0].'
        try:
            return self.maps[0].pop(key, *args)
        except KeyError:
            raise KeyError(f'Key not found in the first mapping: {key!r}')

    def clear(self):
        'Clear maps[0], leaving maps[1:] intact.'
        self.maps[0].clear()

    def __ior__(self, other):
        self.maps[0].update(other)
        return self

    def __or__(self, other):
        if not isinstance(other, _collections_abc.Mapping):
            return NotImplemented
        new = self.copy()
        new.maps[0].update(other)
        return new

    def __ror__(self, other):
        if not isinstance(other, _collections_abc.Mapping):
            return NotImplemented
        merged = dict(other)
        for m in reversed(self.maps):
            merged.update(m)
        return self.__class__(merged)


################################################################################
### UserDict
################################################################################

class UserDict(_collections_abc.MutableMapping):

    def __init__(self, dict=None, /, **kwargs):
        self.data = {}
        if dict is not None:
            self.update(dict)
        if kwargs:
            self.update(kwargs)

    def __len__(self):
        return len(self.data)

    def __getitem__(self, key):
        if key in self.data:
            return self.data[key]
        if hasattr(self.__class__, "__missing__"):
            return self.__class__.__missing__(self, key)
        raise KeyError(key)

    def __setitem__(self, key, item):
        self.data[key] = item

    def __delitem__(self, key):
        del self.data[key]

    def __iter__(self):
        return iter(self.data)

    def __contains__(self, key):
        return key in self.data

    def get(self, key, default=None):
        if key in self:
            return self[key]
        return default

    def __repr__(self):
        return repr(self.data)

    def __or__(self, other):
        if isinstance(other, UserDict):
            return self.__class__(self.data | other.data)
        if isinstance(other, dict):
            return self.__class__(self.data | other)
        return NotImplemented

    def __ror__(self, other):
        if isinstance(other, UserDict):
            return self.__class__(other.data | self.data)
        if isinstance(other, dict):
            return self.__class__(other | self.data)
        return NotImplemented

    def __ior__(self, other):
        if isinstance(other, UserDict):
            self.data |= other.data
        else:
            self.data |= other
        return self

    def __copy__(self):
        inst = self.__class__.__new__(self.__class__)
        inst.__dict__.update(self.__dict__)
        inst.__dict__["data"] = self.__dict__["data"].copy()
        return inst

    def copy(self):
        if self.__class__ is UserDict:
            return UserDict(self.data.copy())
        import copy
        data = self.data
        try:
            self.data = {}
            c = copy.copy(self)
        finally:
            self.data = data
        c.update(self)
        return c

    @classmethod
    def fromkeys(cls, iterable, value=None):
        d = cls()
        for key in iterable:
            d[key] = value
        return d


################################################################################
### UserList
################################################################################

class UserList(_collections_abc.MutableSequence):
    """A more or less complete user-defined wrapper around list objects."""

    def __init__(self, initlist=None):
        self.data = []
        if initlist is not None:
            if type(initlist) == type(self.data):
                self.data[:] = initlist
            elif isinstance(initlist, UserList):
                self.data[:] = initlist.data[:]
            else:
                self.data = list(initlist)

    def __repr__(self):
        return repr(self.data)

    def __lt__(self, other):
        return self.data < self.__cast(other)

    def __le__(self, other):
        return self.data <= self.__cast(other)

    def __eq__(self, other):
        return self.data == self.__cast(other)

    def __gt__(self, other):
        return self.data > self.__cast(other)

    def __ge__(self, other):
        return self.data >= self.__cast(other)

    def __cast(self, other):
        return other.data if isinstance(other, UserList) else other

    def __contains__(self, item):
        return item in self.data

    def __len__(self):
        return len(self.data)

    def __getitem__(self, i):
        if isinstance(i, slice):
            return self.__class__(self.data[i])
        else:
            return self.data[i]

    def __setitem__(self, i, item):
        self.data[i] = item

    def __delitem__(self, i):
        del self.data[i]

    def __add__(self, other):
        if isinstance(other, UserList):
            return self.__class__(self.data + other.data)
        elif isinstance(other, type(self.data)):
            return self.__class__(self.data + other)
        return self.__class__(self.data + list(other))

    def __radd__(self, other):
        if isinstance(other, UserList):
            return self.__class__(other.data + self.data)
        elif isinstance(other, type(self.data)):
            return self.__class__(other + self.data)
        return self.__class__(list(other) + self.data)

    def __iadd__(self, other):
        if isinstance(other, UserList):
            self.data += other.data
        elif isinstance(other, type(self.data)):
            self.data += other
        else:
            self.data += list(other)
        return self

    def __mul__(self, n):
        return self.__class__(self.data * n)

    __rmul__ = __mul__

    def __imul__(self, n):
        self.data *= n
        return self

    def __copy__(self):
        inst = self.__class__.__new__(self.__class__)
        inst.__dict__.update(self.__dict__)
        inst.__dict__["data"] = self.__dict__["data"][:]
        return inst

    def append(self, item):
        self.data.append(item)

    def insert(self, i, item):
        self.data.insert(i, item)

    def pop(self, i=-1):
        return self.data.pop(i)

    def remove(self, item):
        self.data.remove(item)

    def clear(self):
        self.data.clear()

    def copy(self):
        return self.__class__(self)

    def count(self, item):
        return self.data.count(item)

    def index(self, item, *args):
        return self.data.index(item, *args)

    def reverse(self):
        self.data.reverse()

    def sort(self, /, *args, **kwds):
        self.data.sort(*args, **kwds)

    def extend(self, other):
        if isinstance(other, UserList):
            self.data.extend(other.data)
        else:
            self.data.extend(other)


################################################################################
### UserString
################################################################################

class UserString(_collections_abc.Sequence):

    def __init__(self, seq):
        if isinstance(seq, str):
            self.data = seq
        elif isinstance(seq, UserString):
            self.data = seq.data[:]
        else:
            self.data = str(seq)

    def __str__(self):
        return str(self.data)

    def __repr__(self):
        return repr(self.data)

    def __int__(self):
        return int(self.data)

    def __float__(self):
        return float(self.data)

    def __complex__(self):
        return complex(self.data)

    def __hash__(self):
        return hash(self.data)

    def __getnewargs__(self):
        return (self.data[:],)

    def __eq__(self, string):
        if isinstance(string, UserString):
            return self.data == string.data
        return self.data == string

    def __lt__(self, string):
        if isinstance(string, UserString):
            return self.data < string.data
        return self.data < string

    def __le__(self, string):
        if isinstance(string, UserString):
            return self.data <= string.data
        return self.data <= string

    def __gt__(self, string):
        if isinstance(string, UserString):
            return self.data > string.data
        return self.data > string

    def __ge__(self, string):
        if isinstance(string, UserString):
            return self.data >= string.data
        return self.data >= string

    def __contains__(self, char):
        if isinstance(char, UserString):
            char = char.data
        return char in self.data

    def __len__(self):
        return len(self.data)

    def __getitem__(self, index):
        return self.__class__(self.data[index])

    def __add__(self, other):
        if isinstance(other, UserString):
            return self.__class__(self.data + other.data)
        elif isinstance(other, str):
            return self.__class__(self.data + other)
        return self.__class__(self.data + str(other))

    def __radd__(self, other):
        if isinstance(other, str):
            return self.__class__(other + self.data)
        return self.__class__(str(other) + self.data)

    def __mul__(self, n):
        return self.__class__(self.data * n)

    __rmul__ = __mul__

    def __mod__(self, args):
        return self.__class__(self.data % args)

    def __rmod__(self, template):
        return self.__class__(str(template) % self)

    def capitalize(self):
        return self.__class__(self.data.capitalize())

    def casefold(self):
        return self.__class__(self.data.casefold())

    def center(self, width, *args):
        return self.__class__(self.data.center(width, *args))

    def count(self, sub, start=0, end=_sys.maxsize):
        if isinstance(sub, UserString):
            sub = sub.data
        return self.data.count(sub, start, end)

    def removeprefix(self, prefix, /):
        if isinstance(prefix, UserString):
            prefix = prefix.data
        return self.__class__(self.data.removeprefix(prefix))

    def removesuffix(self, suffix, /):
        if isinstance(suffix, UserString):
            suffix = suffix.data
        return self.__class__(self.data.removesuffix(suffix))

    def encode(self, encoding='utf-8', errors='strict'):
        encoding = 'utf-8' if encoding is None else encoding
        errors = 'strict' if errors is None else errors
        return self.data.encode(encoding, errors)

    def endswith(self, suffix, start=0, end=_sys.maxsize):
        return self.data.endswith(suffix, start, end)

    def expandtabs(self, tabsize=8):
        return self.__class__(self.data.expandtabs(tabsize))

    def find(self, sub, start=0, end=_sys.maxsize):
        if isinstance(sub, UserString):
            sub = sub.data
        return self.data.find(sub, start, end)

    def format(self, /, *args, **kwds):
        return self.data.format(*args, **kwds)

    def format_map(self, mapping):
        return self.data.format_map(mapping)

    def index(self, sub, start=0, end=_sys.maxsize):
        return self.data.index(sub, start, end)

    def isalpha(self):
        return self.data.isalpha()

    def isalnum(self):
        return self.data.isalnum()

    def isascii(self):
        return self.data.isascii()

    def isdecimal(self):
        return self.data.isdecimal()

    def isdigit(self):
        return self.data.isdigit()

    def isidentifier(self):
        return self.data.isidentifier()

    def islower(self):
        return self.data.islower()

    def isnumeric(self):
        return self.data.isnumeric()

    def isprintable(self):
        return self.data.isprintable()

    def isspace(self):
        return self.data.isspace()

    def istitle(self):
        return self.data.istitle()

    def isupper(self):
        return self.data.isupper()

    def join(self, seq):
        return self.data.join(seq)

    def ljust(self, width, *args):
        return self.__class__(self.data.ljust(width, *args))

    def lower(self):
        return self.__class__(self.data.lower())

    def lstrip(self, chars=None):
        return self.__class__(self.data.lstrip(chars))

    maketrans = str.maketrans

    def partition(self, sep):
        return self.data.partition(sep)

    def replace(self, old, new, maxsplit=-1):
        if isinstance(old, UserString):
            old = old.data
        if isinstance(new, UserString):
            new = new.data
        return self.__class__(self.data.replace(old, new, maxsplit))

    def rfind(self, sub, start=0, end=_sys.maxsize):
        if isinstance(sub, UserString):
            sub = sub.data
        return self.data.rfind(sub, start, end)

    def rindex(self, sub, start=0, end=_sys.maxsize):
        return self.data.rindex(sub, start, end)

    def rjust(self, width, *args):
        return self.__class__(self.data.rjust(width, *args))

    def rpartition(self, sep):
        return self.data.rpartition(sep)

    def rstrip(self, chars=None):
        return self.__class__(self.data.rstrip(chars))

    def split(self, sep=None, maxsplit=-1):
        return self.data.split(sep, maxsplit)

    def rsplit(self, sep=None, maxsplit=-1):
        return self.data.rsplit(sep, maxsplit)

    def splitlines(self, keepends=False):
        return self.data.splitlines(keepends)

    def startswith(self, prefix, start=0, end=_sys.maxsize):
        return self.data.startswith(prefix, start, end)

    def strip(self, chars=None):
        return self.__class__(self.data.strip(chars))

    def swapcase(self):
        return self.__class__(self.data.swapcase())

    def title(self):
        return self.__class__(self.data.title())

    def translate(self, *args):
        return self.__class__(self.data.translate(*args))

    def upper(self):
        return self.__class__(self.data.upper())

    def zfill(self, width):
        return self.__class__(self.data.zfill(width))
