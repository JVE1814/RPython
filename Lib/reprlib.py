"""repr() with size limits, and a guard for recursive __repr__ methods.

Repr objects produce abbreviated representations: long strings and numbers
are cut in the middle, containers show only their first few items and
nesting is limited, the omitted parts being replaced by a fill value.
``reprlib.repr`` uses a shared default instance.
"""

__all__ = ["Repr", "repr", "recursive_repr"]

import builtins
from itertools import islice
from _thread import get_ident


def recursive_repr(fillvalue='...'):
    """Decorator for __repr__ methods: a call that recurses into the same
    object (in the same thread) returns fillvalue instead."""

    def decorate(method):
        running = set()

        def wrapper(self):
            token = (id(self), get_ident())
            if token in running:
                return fillvalue
            running.add(token)
            try:
                return method(self)
            finally:
                running.discard(token)

        # what functools.update_wrapper would copy (functools imports this
        # module, so it cannot be used here)
        for name in ('__module__', '__name__', '__qualname__', '__doc__',
                     '__annotate__', '__type_params__'):
            try:
                setattr(wrapper, name, getattr(method, name))
            except AttributeError:
                pass
        wrapper.__wrapped__ = method
        return wrapper

    return decorate


def _ordered(items):
    # sorted when possible, so that sets and dicts give stable output
    try:
        return sorted(items)
    except Exception:
        return list(items)


# For these type names the repr_<name> method is used only for the genuine
# type (from this module), not for unrelated classes with the same name.
_GENUINE = {
    'tuple': 'builtins', 'list': 'builtins', 'set': 'builtins',
    'frozenset': 'builtins', 'dict': 'builtins', 'str': 'builtins',
    'int': 'builtins', 'array': 'array', 'deque': 'collections',
}


class Repr:
    """Configurable abbreviated repr().

    The max* attributes bound the number of items shown per container type,
    the length of strings (maxstring), ints (maxlong) and other reprs
    (maxother), and the nesting depth (maxlevel).  indent, when not None,
    puts each container item on its own line, indented by that string (or
    that many spaces).  Subclasses can add repr_<typename>(obj, level)
    methods.
    """

    def __init__(self, *, maxlevel=6, maxtuple=6, maxlist=6, maxarray=5,
                 maxdict=4, maxset=6, maxfrozenset=6, maxdeque=6,
                 maxstring=30, maxlong=40, maxother=30, fillvalue='...',
                 indent=None):
        self.maxlevel = maxlevel
        self.maxtuple = maxtuple
        self.maxlist = maxlist
        self.maxarray = maxarray
        self.maxdict = maxdict
        self.maxset = maxset
        self.maxfrozenset = maxfrozenset
        self.maxdeque = maxdeque
        self.maxstring = maxstring
        self.maxlong = maxlong
        self.maxother = maxother
        self.fillvalue = fillvalue
        self.indent = indent

    def repr(self, x):
        return self.repr1(x, self.maxlevel)

    def repr1(self, x, level):
        cls = type(x)
        name = '_'.join(cls.__name__.split())
        handler = getattr(self, 'repr_' + name, None)
        if handler is not None and _GENUINE.get(name, cls.__module__) == cls.__module__:
            return handler(x, level)
        return self.repr_instance(x, level)

    # -- helpers ---------------------------------------------------------------

    def _indent_unit(self):
        unit = self.indent
        if isinstance(unit, str):
            return unit
        if isinstance(unit, int):
            if unit < 0:
                raise ValueError(
                    f'Repr.indent cannot be negative int (was {unit!r})')
            return ' ' * unit
        raise TypeError(
            f'Repr.indent must be a str, int or None, not {type(unit)}')

    def _layout(self, parts, level):
        """Join item reprs, on one line or one per line (with indent)."""
        if self.indent is None:
            return ', '.join(parts)
        if not parts:
            return ''
        unit = self._indent_unit()
        depth = self.maxlevel - level + 1
        inner = unit * depth
        return ('\n' + ''.join(f'{inner}{part},\n' for part in parts)
                + unit * (depth - 1))

    def _sequence(self, x, level, opening, closing, limit, single=''):
        size = len(x)
        if size and level <= 0:
            return opening + self.fillvalue + closing
        parts = [self.repr1(item, level - 1) for item in islice(x, limit)]
        if size > limit:
            parts.append(self.fillvalue)
        if size == 1 and single and self.indent is None:
            closing = single + closing      # the comma of a 1-tuple
        return opening + self._layout(parts, level) + closing

    def _cut(self, text, limit):
        """Keep the start and end of text, joined by the fill value."""
        head = max(0, (limit - 3) // 2)
        tail = max(0, limit - 3 - head)
        return head, tail

    # -- per-type methods ----------------------------------------------------------

    def repr_tuple(self, x, level):
        return self._sequence(x, level, '(', ')', self.maxtuple, ',')

    def repr_list(self, x, level):
        return self._sequence(x, level, '[', ']', self.maxlist)

    def repr_array(self, x, level):
        if not x:
            return f"array('{x.typecode}')"
        return self._sequence(x, level, f"array('{x.typecode}', [", '])',
                              self.maxarray)

    def repr_set(self, x, level):
        if not x:
            return 'set()'
        return self._sequence(_ordered(x), level, '{', '}', self.maxset)

    def repr_frozenset(self, x, level):
        if not x:
            return 'frozenset()'
        return self._sequence(_ordered(x), level, 'frozenset({', '})',
                              self.maxfrozenset)

    def repr_deque(self, x, level):
        return self._sequence(x, level, 'deque([', '])', self.maxdeque)

    def repr_dict(self, x, level):
        if not x:
            return '{}'
        if level <= 0:
            return '{' + self.fillvalue + '}'
        below = level - 1
        parts = [f'{self.repr1(key, below)}: {self.repr1(x[key], below)}'
                 for key in islice(_ordered(x), self.maxdict)]
        if len(x) > self.maxdict:
            parts.append(self.fillvalue)
        return '{' + self._layout(parts, level) + '}'

    def repr_str(self, x, level):
        text = builtins.repr(x[:self.maxstring])
        if len(text) > self.maxstring:
            head, tail = self._cut(text, self.maxstring)
            # quote the shortened string, then cut its repr again
            text = builtins.repr(x[:head] + x[len(x) - tail:])
            text = text[:head] + self.fillvalue + text[len(text) - tail:]
        return text

    def repr_int(self, x, level):
        try:
            text = builtins.repr(x)
        except ValueError:
            # more digits than int->str conversion allows
            import math
            import sys
            digits = 1 + int(math.log10(abs(x)))
            return (f'<{x.__class__.__name__} instance with roughly {digits} '
                    f'digits (limit at {sys.get_int_max_str_digits()}) '
                    f'at 0x{id(x):x}>')
        if len(text) > self.maxlong:
            head, tail = self._cut(text, self.maxlong)
            text = text[:head] + self.fillvalue + text[len(text) - tail:]
        return text

    def repr_instance(self, x, level):
        try:
            text = builtins.repr(x)
        except Exception:
            return f'<{x.__class__.__name__} instance at {id(x):#x}>'
        if len(text) > self.maxother:
            head, tail = self._cut(text, self.maxother)
            text = text[:head] + self.fillvalue + text[len(text) - tail:]
        return text


aRepr = Repr()
repr = aRepr.repr
