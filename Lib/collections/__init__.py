"""Specialised container types to complement dict, list, set and tuple.

namedtuple()   factory of tuple subclasses whose items also have names
deque          list-like sequence with fast appends and pops at both ends
ChainMap       a single mapping view over several mappings
Counter        dict subclass counting hashable objects (a multiset)
OrderedDict    dict subclass with order-aware operations
defaultdict    dict subclass that calls a factory for missing keys
UserDict, UserList, UserString
               wrappers around dict, list and str meant to be subclassed
"""

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

# collections.abc is the same module object as _collections_abc
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
    # referenced by the pickles of deque iterators
    from _collections import _deque_iterator  # noqa: F401
except ImportError:
    pass

try:
    from _collections import defaultdict
except ImportError:
    pass

try:
    _DICT_TYPES = (dict, frozendict)
except NameError:
    _DICT_TYPES = (dict,)


# ===========================================================================
# OrderedDict (pure Python version; the accelerator's type replaces it)
# ===========================================================================

class _Node:
    # one key in the circular list that records the order of an OrderedDict
    __slots__ = ('prev', 'next', 'key', '__weakref__')


class _OrderedKeys(_collections_abc.KeysView):
    def __reversed__(self):
        yield from reversed(self._mapping)


class _OrderedItems(_collections_abc.ItemsView):
    def __reversed__(self):
        mapping = self._mapping
        for key in reversed(mapping):
            yield (key, mapping[key])


class _OrderedValues(_collections_abc.ValuesView):
    def __reversed__(self):
        mapping = self._mapping
        for key in reversed(mapping):
            yield mapping[key]


_absent = object()


class OrderedDict(dict):
    """dict subclass that remembers the order in which keys were inserted
    and can reorder them (move_to_end, popitem(last=False)); equality with
    another OrderedDict is order-sensitive."""

    # Besides the dict itself each instance keeps a circular doubly linked
    # list of _Node objects in key order, with a sentinel node __head, and
    # a dict __nodes mapping keys to their nodes.  Unlinked nodes get
    # prev/next = None, so iterators notice deletions.

    def __new__(cls, /, *args, **kwds):
        self = dict.__new__(cls)
        head = _Node()
        head.prev = head.next = head
        self.__head = head
        self.__nodes = {}
        return self

    def __init__(self, other=(), /, **kwds):
        self.__update(other, **kwds)

    def __link_last(self, node):
        head = self.__head
        last = head.prev
        node.prev, node.next = last, head
        last.next = head.prev = node

    def __unlink(self, node):
        node.prev.next = node.next
        node.next.prev = node.prev
        node.prev = node.next = None

    def __setitem__(self, key, value, _dict_setitem=dict.__setitem__):
        if key not in self:
            node = _Node()
            node.key = key
            self.__nodes[key] = node
            self.__link_last(node)
        _dict_setitem(self, key, value)

    def __delitem__(self, key, _dict_delitem=dict.__delitem__):
        _dict_delitem(self, key)
        self.__unlink(self.__nodes.pop(key))

    def __iter__(self):
        head = self.__head
        node = head.next
        while node is not head:
            yield node.key
            node = node.next

    def __reversed__(self):
        head = self.__head
        node = head.prev
        while node is not head:
            yield node.key
            node = node.prev

    def clear(self):
        head = self.__head
        head.prev = head.next = head
        self.__nodes.clear()
        dict.clear(self)

    def popitem(self, last=True):
        """Remove and return the last (or, if last is false, the first)
        (key, value) pair."""
        if not self:
            raise KeyError('dictionary is empty')
        head = self.__head
        key = (head.prev if last else head.next).key
        self.__unlink(self.__nodes.pop(key))
        return key, dict.pop(self, key)

    def move_to_end(self, key, last=True):
        """Move key to the end (or, if last is false, to the start).
        KeyError if key is missing."""
        node = self.__nodes[key]
        self.__unlink(node)
        if last:
            self.__link_last(node)
        else:
            head = self.__head
            first = head.next
            node.prev, node.next = head, first
            first.prev = head.next = node

    def __sizeof__(self):
        size = _sys.getsizeof
        n = len(self) + 1                           # nodes incl. the head
        return (size(self.__dict__) + 2 * size(self.__nodes)
                + 2 * n * size(self.__head))

    __update = _collections_abc.MutableMapping.update
    update = __update

    def keys(self):
        """A set-like, reversible view of the keys."""
        return _OrderedKeys(self)

    def items(self):
        """A set-like, reversible view of the (key, value) pairs."""
        return _OrderedItems(self)

    def values(self):
        """A reversible view of the values."""
        return _OrderedValues(self)

    __ne__ = _collections_abc.MutableMapping.__ne__

    def pop(self, key, default=_absent):
        """Remove key and return its value; default (if given) when key is
        missing, else KeyError."""
        value = dict.pop(self, key, _absent)
        if value is not _absent:
            self.__unlink(self.__nodes.pop(key))
            return value
        if default is _absent:
            raise KeyError(key)
        return default

    def setdefault(self, key, default=None):
        """Return self[key], first inserting key with default if missing."""
        if key in self:
            return self[key]
        self[key] = default
        return default

    @_recursive_repr()
    def __repr__(self):
        if not self:
            return f'{self.__class__.__name__}()'
        return f'{self.__class__.__name__}({dict(self.items())!r})'

    def __reduce__(self):
        # the instance state without our private bookkeeping attributes
        state = self.__getstate__()
        if state:
            slots = None
            if isinstance(state, tuple):
                state, slots = state
            private = vars(OrderedDict())
            state = {k: v for k, v in (state or {}).items()
                     if k not in private} or None
            if slots:
                slots = {k: v for k, v in slots.items() if k not in private}
                if slots:
                    state = (state, slots)
        return self.__class__, (), state or None, None, iter(self.items())

    def copy(self):
        """A shallow copy."""
        return self.__class__(self)

    @classmethod
    def fromkeys(cls, iterable, value=None):
        """New ordered dictionary with the keys of iterable, all mapped to
        value."""
        result = cls()
        for key in iterable:
            result[key] = value
        return result

    def __eq__(self, other):
        if isinstance(other, OrderedDict):
            return dict.__eq__(self, other) and all(map(_eq, self, other))
        return dict.__eq__(self, other)

    def __ior__(self, other):
        self.update(other)
        return self

    def __or__(self, other):
        if not isinstance(other, _DICT_TYPES):
            return NotImplemented
        result = self.__class__(self)
        result.update(other)
        return result

    def __ror__(self, other):
        if not isinstance(other, _DICT_TYPES):
            return NotImplemented
        result = self.__class__(other)
        result.update(self)
        return result


try:
    from _collections import OrderedDict
except ImportError:
    pass


# ===========================================================================
# namedtuple
# ===========================================================================

try:
    from _collections import _tuplegetter
except ImportError:
    def _tuplegetter(index, doc):
        return property(_itemgetter(index), doc=doc)

# one interned docstring per field position, shared by all named tuples
_FIELD_DOCS = {}


def _field_doc(index):
    doc = _FIELD_DOCS.get(index)
    if doc is None:
        doc = _FIELD_DOCS[index] = _sys.intern(
            f'Alias for field number {index}')
    return doc


def _check_names(typename, fields, rename):
    """Validate (and with rename, repair) the type and field names."""
    if rename:
        seen = set()
        for i, name in enumerate(fields):
            if (not name.isidentifier() or _iskeyword(name)
                    or name.startswith('_') or name in seen):
                fields[i] = f'_{i}'
            seen.add(name)
    for name in (typename, *fields):
        if type(name) is not str:
            raise TypeError('Type names and field names must be strings')
        if not name.isidentifier():
            raise ValueError('Type names and field names must be valid '
                             f'identifiers: {name!r}')
        if _iskeyword(name):
            raise ValueError('Type names and field names cannot be a '
                             f'keyword: {name!r}')
    seen = set()
    for name in fields:
        if name.startswith('_') and not rename:
            raise ValueError('Field names cannot start with an underscore: '
                             f'{name!r}')
        if name in seen:
            raise ValueError(f'Encountered duplicate field name: {name!r}')
        seen.add(name)


def namedtuple(typename, field_names, *, rename=False, defaults=None,
               module=None):
    """Return a new tuple subclass whose items can also be read as named
    attributes.

    field_names is a sequence of names or a single string of names
    separated by whitespace and/or commas.  rename=True replaces invalid
    names by positional ones; defaults supplies default values for the
    rightmost fields; module sets __module__ of the new class.

    >>> Card = namedtuple('Card', 'rank suit')
    >>> c = Card('Q', suit='hearts')
    >>> c
    Card(rank='Q', suit='hearts')
    >>> c.rank, c[1]
    ('Q', 'hearts')
    >>> c._replace(rank='K')
    Card(rank='K', suit='hearts')
    >>> c._asdict()
    {'rank': 'Q', 'suit': 'hearts'}
    >>> Card._make(['7', 'spades'])
    Card(rank='7', suit='spades')
    """
    if isinstance(field_names, str):
        field_names = field_names.replace(',', ' ').split()
    fields = [str(name) for name in field_names]
    typename = _sys.intern(str(typename))
    _check_names(typename, fields, rename)
    fields = tuple(_sys.intern(name) for name in fields)
    count = len(fields)

    field_defaults = {}
    if defaults is not None:
        defaults = tuple(defaults)
        if len(defaults) > count:
            raise TypeError('Got more default values than field names')
        field_defaults = dict(zip(fields[count - len(defaults):], defaults))

    params = ', '.join(fields)
    signature = params + (',' if count == 1 else '')
    # __new__ needs the field names as real parameters, so it is compiled;
    # only validated identifiers are interpolated
    scope = {'_new_tuple': tuple.__new__, '__builtins__': {},
             '__name__': f'namedtuple_{typename}'}
    __new__ = eval(f'lambda _cls, {params}: _new_tuple(_cls, ({signature}))',
                   scope)
    __new__.__name__ = '__new__'
    __new__.__doc__ = f'Create new instance of {typename}({params})'
    if defaults is not None:
        __new__.__defaults__ = defaults

    new_tuple = tuple.__new__
    item_format = '(' + ', '.join(f'{name}=%r' for name in fields) + ')'

    @classmethod
    def _make(cls, iterable):
        made = new_tuple(cls, iterable)
        if len(made) != count:
            raise TypeError(f'Expected {count} arguments, got {len(made)}')
        return made

    _make.__func__.__doc__ = (f'Make a new {typename} object from a sequence '
                              'or iterable')

    def _replace(self, /, **changes):
        result = self._make(map(changes.pop, fields, self))
        if changes:
            raise TypeError(f'Got unexpected field names: {list(changes)!r}')
        return result

    _replace.__doc__ = (f'Return a new {typename} object replacing specified '
                        'fields with new values')

    def __repr__(self):
        """Return a string such as Name(field=value, ...)."""
        return self.__class__.__name__ + item_format % self

    def _asdict(self):
        """Return a dict mapping the field names to the values."""
        return dict(zip(self._fields, self))

    def __getnewargs__(self):
        """Return the values as a plain tuple (for copy and pickle)."""
        return tuple(self)

    namespace = {
        '__doc__': f'{typename}({params})',
        '__slots__': (),
        '_fields': fields,
        '_field_defaults': field_defaults,
        '__new__': __new__,
        '_make': _make,
        '_replace': _replace,
        '__replace__': _replace,
        '__repr__': __repr__,
        '_asdict': _asdict,
        '__getnewargs__': __getnewargs__,
        '__match_args__': fields,
    }
    for method in (__new__, _make.__func__, _replace, __repr__, _asdict,
                   __getnewargs__):
        method.__qualname__ = f'{typename}.{method.__name__}'
    for index, name in enumerate(fields):
        namespace[name] = _tuplegetter(index, _field_doc(index))

    cls = type(typename, (tuple,), namespace)

    # pickle finds the class through __module__: default to the caller's
    if module is None:
        try:
            module = _sys._getframemodulename(1) or '__main__'
        except AttributeError:
            try:
                module = _sys._getframe(1).f_globals.get('__name__',
                                                          '__main__')
            except (AttributeError, ValueError):
                pass
    if module is not None:
        cls.__module__ = module
    return cls


# ===========================================================================
# Counter
# ===========================================================================

def _count_elements(mapping, iterable):
    """Add one to mapping[x] for every x of iterable."""
    get = mapping.get
    for element in iterable:
        mapping[element] = get(element, 0) + 1


try:
    from _collections import _count_elements
except ImportError:
    pass


class Counter(dict):
    """dict subclass for counting hashable objects (a bag or multiset).

    Elements are keys and their counts are values; missing elements count
    zero.  Counts may also be zero or negative.

    >>> c = Counter('abracadabra')
    >>> c['a'], c['z']
    (5, 0)
    >>> c.most_common(2)
    [('a', 5), ('b', 2)]
    >>> c.total()
    11
    >>> c.subtract('aaa')
    >>> c['a']
    2
    >>> sorted((Counter('aab') + Counter('bc')).elements())
    ['a', 'a', 'b', 'b', 'c']
    >>> Counter('aab') - Counter('abc')
    Counter({'a': 1})
    >>> Counter('aab') | Counter('abbc'), Counter('aab') & Counter('abbc')
    (Counter({'a': 2, 'b': 2, 'c': 1}), Counter({'a': 1, 'b': 1}))
    >>> Counter('aab') ^ Counter('abbc')
    Counter({'a': 1, 'b': 1, 'c': 1})
    """

    def __init__(self, iterable=None, /, **kwds):
        """Count the elements of iterable, or copy the counts of a mapping;
        keyword arguments add counts as well."""
        super().__init__()
        self.update(iterable, **kwds)

    def __missing__(self, key):
        return 0

    def total(self):
        """Sum of all counts."""
        return sum(self.values())

    def most_common(self, n=None):
        """List of (element, count) pairs, most common first; only the
        first n if n is given.  Equal counts keep insertion order.

        >>> Counter('mississippi').most_common(3)
        [('i', 4), ('s', 4), ('p', 2)]
        """
        by_count = _itemgetter(1)
        if n is None:
            return sorted(self.items(), key=by_count, reverse=True)
        import heapq
        return heapq.nlargest(n, self.items(), key=by_count)

    def elements(self):
        """Iterator over the elements, each repeated as often as its count
        (elements with a count below one are skipped)."""
        return _chain.from_iterable(_starmap(_repeat, self.items()))

    @classmethod
    def fromkeys(cls, iterable, v=None):
        raise NotImplementedError(
            'Counter.fromkeys() is not supported; use Counter(iterable)')

    def update(self, iterable=None, /, **kwds):
        """Add counts: from the elements of an iterable, from a mapping of
        counts, and from keyword arguments.

        >>> c = Counter('ab')
        >>> c.update('abb', c=3)
        >>> c
        Counter({'b': 3, 'c': 3, 'a': 2})
        """
        if iterable is not None:
            if isinstance(iterable, _collections_abc.Mapping):
                if self:
                    get = self.get
                    for element, count in iterable.items():
                        self[element] = count + get(element, 0)
                else:
                    super().update(iterable)        # nothing to add to
            else:
                _count_elements(self, iterable)
        if kwds:
            self.update(kwds)

    def subtract(self, iterable=None, /, **kwds):
        """Like update(), but subtract counts.  Counts may become zero or
        negative.

        >>> c = Counter('ab')
        >>> c.subtract('bbc')
        >>> c['b'], c['c']
        (-1, -1)
        """
        if iterable is not None:
            get = self.get
            if isinstance(iterable, _collections_abc.Mapping):
                for element, count in iterable.items():
                    self[element] = get(element, 0) - count
            else:
                for element in iterable:
                    self[element] = get(element, 0) - 1
        if kwds:
            self.subtract(kwds)

    def copy(self):
        """A shallow copy (of the same class)."""
        return self.__class__(self)

    def __reduce__(self):
        return self.__class__, (dict(self),)

    def __delitem__(self, element):
        # deleting a missing element is not an error
        if element in self:
            super().__delitem__(element)

    def __repr__(self):
        name = self.__class__.__name__
        if not self:
            return f'{name}()'
        try:
            counts = dict(self.most_common())
        except TypeError:
            counts = dict(self)                     # unorderable counts
        return f'{name}({counts!r})'

    # -- multiset comparisons (missing elements count zero) -----------------

    def _compare(self, other, op):
        return all(op(self[e], other[e]) for e in self.keys() | other.keys())

    def __eq__(self, other):
        if not isinstance(other, Counter):
            return NotImplemented
        return self._compare(other, _eq)

    def __ne__(self, other):
        if not isinstance(other, Counter):
            return NotImplemented
        return not self == other

    def __le__(self, other):
        if not isinstance(other, Counter):
            return NotImplemented
        return self._compare(other, lambda a, b: a <= b)

    def __lt__(self, other):
        if not isinstance(other, Counter):
            return NotImplemented
        return self <= other and self != other

    def __ge__(self, other):
        if not isinstance(other, Counter):
            return NotImplemented
        return self._compare(other, lambda a, b: a >= b)

    def __gt__(self, other):
        if not isinstance(other, Counter):
            return NotImplemented
        return self >= other and self != other

    # -- multiset arithmetic: results keep only positive counts ------------

    def _merged(self, other, combine):
        # combine(count_here, count_there) for every element of either
        # operand, self's elements first
        result = Counter()
        for element, count in self.items():
            value = combine(count, other[element])
            if value > 0:
                result[element] = value
        for element, count in other.items():
            if element not in self:
                value = combine(0, count)
                if value > 0:
                    result[element] = value
        return result

    def __add__(self, other):
        """Sum of counts.

        >>> Counter('abbb') + Counter('bcc')
        Counter({'b': 4, 'c': 2, 'a': 1})
        """
        if not isinstance(other, Counter):
            return NotImplemented
        return self._merged(other, lambda a, b: a + b)

    def __sub__(self, other):
        """Difference of counts."""
        if not isinstance(other, Counter):
            return NotImplemented
        return self._merged(other, lambda a, b: a - b)

    def __or__(self, other):
        """Maximum of counts (union)."""
        if not isinstance(other, Counter):
            return NotImplemented
        return self._merged(other, lambda a, b: b if a < b else a)

    def __and__(self, other):
        """Minimum of counts (intersection)."""
        if not isinstance(other, Counter):
            return NotImplemented
        return self._merged(other, lambda a, b: a if a < b else b)

    def __xor__(self, other):
        """Absolute difference of counts (symmetric difference)."""
        if not isinstance(other, Counter):
            return NotImplemented
        return self._merged(other, lambda a, b: abs(a - b))

    def __pos__(self):
        """The elements with a positive count."""
        return self._merged(Counter(), lambda a, b: a)

    def __neg__(self):
        """The elements with a negative count, negated."""
        return self._merged(Counter(), lambda a, b: -a)

    # -- in-place versions: same results, modifying self --------------------

    def _drop_nonpositive(self):
        for element in [e for e, count in self.items() if not count > 0]:
            del self[element]
        return self

    def __iadd__(self, other):
        """In-place sum of counts.

        >>> c = Counter('ab')
        >>> c += Counter('b')
        >>> c
        Counter({'b': 2, 'a': 1})
        """
        for element, count in other.items():
            self[element] += count
        return self._drop_nonpositive()

    def __isub__(self, other):
        """In-place difference of counts."""
        for element, count in other.items():
            self[element] -= count
        return self._drop_nonpositive()

    def __ior__(self, other):
        """In-place maximum of counts."""
        for element, count in other.items():
            if count > self[element]:
                self[element] = count
        return self._drop_nonpositive()

    def __iand__(self, other):
        """In-place minimum of counts."""
        for element, count in self.items():
            there = other[element]
            if there < count:
                self[element] = there
        return self._drop_nonpositive()

    def __ixor__(self, other):
        """In-place absolute difference of counts."""
        for element, count in self.items():
            self[element] = abs(count - other[element])
        for element, count in other.items():
            if element not in self:
                self[element] = abs(count)
        return self._drop_nonpositive()


# ===========================================================================
# ChainMap
# ===========================================================================

class ChainMap(_collections_abc.MutableMapping):
    """A mapping that looks keys up in a list of mappings, ``maps``, in
    order.  Writes, updates and deletions only affect maps[0]."""

    def __init__(self, *maps):
        """Use the given mappings (a new empty dict if there are none)."""
        self.maps = list(maps) or [{}]

    def __missing__(self, key):
        raise KeyError(key)

    def __getitem__(self, key):
        for mapping in self.maps:
            try:
                return mapping[key]     # (honours defaultdict's __missing__)
            except KeyError:
                pass
        return self.__missing__(key)

    def get(self, key, default=None):
        return self[key] if key in self else default

    def __len__(self):
        return len(set().union(*self.maps))

    def __iter__(self):
        # keys in the order of the last mapping first, without duplicates
        seen = {}
        for mapping in reversed(self.maps):
            seen.update(dict.fromkeys(mapping))
        return iter(seen)

    def __contains__(self, key):
        return any(key in mapping for mapping in self.maps)

    def __bool__(self):
        return any(self.maps)

    @_recursive_repr()
    def __repr__(self):
        return f'{self.__class__.__name__}({", ".join(map(repr, self.maps))})'

    @classmethod
    def fromkeys(cls, iterable, value=None, /):
        """A ChainMap over a new dict with the keys of iterable."""
        return cls(dict.fromkeys(iterable, value))

    def copy(self):
        """A new ChainMap with a copy of maps[0] and the same other maps."""
        return self.__class__(self.maps[0].copy(), *self.maps[1:])

    __copy__ = copy

    def new_child(self, m=None, **kwargs):
        """A new ChainMap with m (default: a new dict, updated with the
        keyword arguments) in front of the current maps."""
        if m is None:
            m = kwargs
        elif kwargs:
            m.update(kwargs)
        return self.__class__(m, *self.maps)

    @property
    def parents(self):
        """A new ChainMap over all maps but the first."""
        return self.__class__(*self.maps[1:])

    def __setitem__(self, key, value):
        self.maps[0][key] = value

    def __delitem__(self, key):
        try:
            del self.maps[0][key]
        except KeyError:
            raise KeyError(f'Key not found in the first mapping: {key!r}')

    def popitem(self):
        """Remove and return a (key, value) pair of maps[0]."""
        try:
            return self.maps[0].popitem()
        except KeyError:
            raise KeyError('No keys found in the first mapping.')

    def pop(self, key, *args):
        """Remove key from maps[0] and return its value."""
        try:
            return self.maps[0].pop(key, *args)
        except KeyError:
            raise KeyError(f'Key not found in the first mapping: {key!r}')

    def clear(self):
        """Empty maps[0] (the other maps are left alone)."""
        self.maps[0].clear()

    def __ior__(self, other):
        self.maps[0].update(other)
        return self

    def __or__(self, other):
        if not isinstance(other, _collections_abc.Mapping):
            return NotImplemented
        result = self.copy()
        result.maps[0].update(other)
        return result

    def __ror__(self, other):
        if not isinstance(other, _collections_abc.Mapping):
            return NotImplemented
        merged = dict(other)
        for mapping in reversed(self.maps):
            merged.update(mapping)
        return self.__class__(merged)


# ===========================================================================
# UserDict, UserList, UserString
# ===========================================================================

def _copy_with_data(obj, data):
    # a copy of obj (same class, same instance attributes) holding data
    clone = obj.__class__.__new__(obj.__class__)
    clone.__dict__.update(obj.__dict__)
    clone.__dict__['data'] = data
    return clone


class UserDict(_collections_abc.MutableMapping):
    """Dictionary wrapper: the contents live in the dict ``data``."""

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
        missing = getattr(self.__class__, '__missing__', None)
        if missing is not None:
            return missing(self, key)
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
            other = other.data
        elif not isinstance(other, _collections_abc.Mapping):
            return NotImplemented
        merged = dict(self.data)
        merged.update(other)
        return self.__class__(merged)

    def __ror__(self, other):
        if isinstance(other, UserDict):
            other = other.data
        elif not isinstance(other, _collections_abc.Mapping):
            return NotImplemented
        merged = dict(other)
        merged.update(self.data)
        return self.__class__(merged)

    def __ior__(self, other):
        self.data |= other.data if isinstance(other, UserDict) else other
        return self

    def __copy__(self):
        return _copy_with_data(self, self.__dict__['data'].copy())

    def copy(self):
        if self.__class__ is UserDict:
            return UserDict(self.data.copy())
        # a subclass may keep other state: copy the object, then the data
        import copy
        data = self.data
        try:
            self.data = {}
            clone = copy.copy(self)
        finally:
            self.data = data
        clone.update(self)
        return clone

    @classmethod
    def fromkeys(cls, iterable, value=None):
        result = cls()
        for key in iterable:
            result[key] = value
        return result


def _unwrap_list(other):
    return other.data if isinstance(other, UserList) else other


class UserList(_collections_abc.MutableSequence):
    """List wrapper: the contents live in the list ``data``."""

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
        return self.data < _unwrap_list(other)

    def __le__(self, other):
        return self.data <= _unwrap_list(other)

    def __eq__(self, other):
        return self.data == _unwrap_list(other)

    def __gt__(self, other):
        return self.data > _unwrap_list(other)

    def __ge__(self, other):
        return self.data >= _unwrap_list(other)

    def __contains__(self, item):
        return item in self.data

    def __len__(self):
        return len(self.data)

    def __getitem__(self, i):
        if isinstance(i, slice):
            return self.__class__(self.data[i])
        return self.data[i]

    def __setitem__(self, i, item):
        self.data[i] = item

    def __delitem__(self, i):
        del self.data[i]

    def __add__(self, other):
        if isinstance(other, UserList):
            return self.__class__(self.data + other.data)
        if isinstance(other, type(self.data)):
            return self.__class__(self.data + other)
        return self.__class__(self.data + list(other))

    def __radd__(self, other):
        if isinstance(other, UserList):
            return self.__class__(other.data + self.data)
        if isinstance(other, type(self.data)):
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
        return _copy_with_data(self, self.__dict__['data'][:])

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
        self.data.extend(_unwrap_list(other))


def _unwrap_str(value):
    return value.data if isinstance(value, UserString) else value


class UserString(_collections_abc.Sequence):
    """String wrapper: the contents live in the str ``data``.  Methods that
    produce a string return an instance of the same class."""

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
        return self.data == _unwrap_str(string)

    def __lt__(self, string):
        return self.data < _unwrap_str(string)

    def __le__(self, string):
        return self.data <= _unwrap_str(string)

    def __gt__(self, string):
        return self.data > _unwrap_str(string)

    def __ge__(self, string):
        return self.data >= _unwrap_str(string)

    def __contains__(self, char):
        return _unwrap_str(char) in self.data

    def __len__(self):
        return len(self.data)

    def __getitem__(self, index):
        return self.__class__(self.data[index])

    def __add__(self, other):
        if isinstance(other, (UserString, str)):
            return self.__class__(self.data + _unwrap_str(other))
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

    # -- methods returning a new string of this class ----------------------

    def capitalize(self):
        return self.__class__(self.data.capitalize())

    def casefold(self):
        return self.__class__(self.data.casefold())

    def center(self, width, *args):
        return self.__class__(self.data.center(width, *args))

    def expandtabs(self, tabsize=8):
        return self.__class__(self.data.expandtabs(tabsize))

    def ljust(self, width, *args):
        return self.__class__(self.data.ljust(width, *args))

    def rjust(self, width, *args):
        return self.__class__(self.data.rjust(width, *args))

    def lower(self):
        return self.__class__(self.data.lower())

    def upper(self):
        return self.__class__(self.data.upper())

    def swapcase(self):
        return self.__class__(self.data.swapcase())

    def title(self):
        return self.__class__(self.data.title())

    def lstrip(self, chars=None):
        return self.__class__(self.data.lstrip(chars))

    def rstrip(self, chars=None):
        return self.__class__(self.data.rstrip(chars))

    def strip(self, chars=None):
        return self.__class__(self.data.strip(chars))

    def removeprefix(self, prefix, /):
        return self.__class__(self.data.removeprefix(_unwrap_str(prefix)))

    def removesuffix(self, suffix, /):
        return self.__class__(self.data.removesuffix(_unwrap_str(suffix)))

    def replace(self, old, new, maxsplit=-1):
        return self.__class__(self.data.replace(_unwrap_str(old),
                                                _unwrap_str(new), maxsplit))

    def translate(self, *args):
        return self.__class__(self.data.translate(*args))

    def zfill(self, width):
        return self.__class__(self.data.zfill(width))

    # -- searching -----------------------------------------------------------

    def count(self, sub, start=0, end=_sys.maxsize):
        return self.data.count(_unwrap_str(sub), start, end)

    def find(self, sub, start=0, end=_sys.maxsize):
        return self.data.find(_unwrap_str(sub), start, end)

    def rfind(self, sub, start=0, end=_sys.maxsize):
        return self.data.rfind(_unwrap_str(sub), start, end)

    def index(self, sub, start=0, end=_sys.maxsize):
        return self.data.index(_unwrap_str(sub), start, end)

    def rindex(self, sub, start=0, end=_sys.maxsize):
        return self.data.rindex(_unwrap_str(sub), start, end)

    def startswith(self, prefix, start=0, end=_sys.maxsize):
        return self.data.startswith(prefix, start, end)

    def endswith(self, suffix, start=0, end=_sys.maxsize):
        return self.data.endswith(suffix, start, end)

    # -- methods returning other types ---------------------------------------

    def encode(self, encoding='utf-8', errors='strict'):
        return self.data.encode('utf-8' if encoding is None else encoding,
                                'strict' if errors is None else errors)

    def format(self, /, *args, **kwds):
        return self.data.format(*args, **kwds)

    def format_map(self, mapping):
        return self.data.format_map(mapping)

    def join(self, seq):
        return self.data.join(seq)

    def partition(self, sep):
        return self.data.partition(sep)

    def rpartition(self, sep):
        return self.data.rpartition(sep)

    def split(self, sep=None, maxsplit=-1):
        return self.data.split(sep, maxsplit)

    def rsplit(self, sep=None, maxsplit=-1):
        return self.data.rsplit(sep, maxsplit)

    def splitlines(self, keepends=False):
        return self.data.splitlines(keepends)

    maketrans = str.maketrans

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
