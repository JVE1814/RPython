"""Redo the builtin repr() (representation) but with limits on most sizes."""

__all__ = ["Repr", "repr", "recursive_repr"]

import builtins
from itertools import islice
from _thread import get_ident


def recursive_repr(fillvalue='...'):
    'Decorator to make a repr function return fillvalue for a recursive call'

    def decorating_function(user_function):
        active = set()

        def wrapper(self):
            key = id(self), get_ident()
            if key in active:
                return fillvalue
            active.add(key)
            try:
                return user_function(self)
            finally:
                active.discard(key)

        # Copy the metadata functools.update_wrapper() would copy (functools
        # itself depends on this module, so it cannot be used here).
        for attr in ('__module__', '__doc__', '__name__', '__qualname__',
                     '__annotate__', '__type_params__'):
            try:
                value = getattr(user_function, attr)
            except AttributeError:
                continue
            setattr(wrapper, attr, value)
        wrapper.__wrapped__ = user_function
        return wrapper

    return decorating_function


def _possibly_sorted(x):
    # Sorting gives reproducible output, but not everything is orderable.
    try:
        return sorted(x)
    except Exception:
        return list(x)


class Repr:
    # Types whose specialised repr_<typename> method only applies to the
    # genuine type from the given module (not to look-alike classes).
    _lookup = {
        'tuple': 'builtins',
        'list': 'builtins',
        'array': 'array',
        'set': 'builtins',
        'frozenset': 'builtins',
        'deque': 'collections',
        'dict': 'builtins',
        'str': 'builtins',
        'int': 'builtins',
    }

    def __init__(
        self, *, maxlevel=6, maxtuple=6, maxlist=6, maxarray=5, maxdict=4,
        maxset=6, maxfrozenset=6, maxdeque=6, maxstring=30, maxlong=40,
        maxother=30, fillvalue='...', indent=None,
    ):
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
        typename = cls.__name__
        if ' ' in typename:
            typename = '_'.join(typename.split())
        method = getattr(self, 'repr_' + typename, None)
        if method is not None:
            module = self._lookup.get(typename)
            if module is None or module == cls.__module__:
                return method(x, level)
        return self.repr_instance(x, level)

    def _check_indent(self):
        indent = self.indent
        if isinstance(indent, int):
            if indent < 0:
                raise ValueError(
                    f'Repr.indent cannot be negative int (was {indent!r})')
            return indent * ' '
        if isinstance(indent, str):
            return indent
        raise TypeError(
            f'Repr.indent must be a str, int or None, not {type(indent)}')

    def _join(self, pieces, level):
        if self.indent is None:
            return ', '.join(pieces)
        if not pieces:
            return ''
        indent = self._check_indent()
        sep = ',\n' + (self.maxlevel - level + 1) * indent
        joined = sep.join(('', *pieces, ''))
        return joined[1:-len(indent) or None]

    def _repr_iterable(self, x, level, left, right, maxiter, trail=''):
        n = len(x)
        if level <= 0 and n:
            body = self.fillvalue
        else:
            sub = level - 1
            pieces = [self.repr1(item, sub) for item in islice(x, maxiter)]
            if n > maxiter:
                pieces.append(self.fillvalue)
            body = self._join(pieces, level)
            if n == 1 and trail and self.indent is None:
                right = trail + right
        return f'{left}{body}{right}'

    def repr_tuple(self, x, level):
        return self._repr_iterable(x, level, '(', ')', self.maxtuple, ',')

    def repr_list(self, x, level):
        return self._repr_iterable(x, level, '[', ']', self.maxlist)

    def repr_array(self, x, level):
        if not x:
            return f"array('{x.typecode}')"
        return self._repr_iterable(x, level, f"array('{x.typecode}', [",
                                   '])', self.maxarray)

    def repr_set(self, x, level):
        if not x:
            return 'set()'
        return self._repr_iterable(_possibly_sorted(x), level, '{', '}',
                                   self.maxset)

    def repr_frozenset(self, x, level):
        if not x:
            return 'frozenset()'
        return self._repr_iterable(_possibly_sorted(x), level, 'frozenset({',
                                   '})', self.maxfrozenset)

    def repr_deque(self, x, level):
        return self._repr_iterable(x, level, 'deque([', '])', self.maxdeque)

    def repr_dict(self, x, level):
        n = len(x)
        if n == 0:
            return '{}'
        if level <= 0:
            return '{' + self.fillvalue + '}'
        sub = level - 1
        pieces = []
        for key in islice(_possibly_sorted(x), self.maxdict):
            pieces.append(f'{self.repr1(key, sub)}: {self.repr1(x[key], sub)}')
        if n > self.maxdict:
            pieces.append(self.fillvalue)
        return '{' + self._join(pieces, level) + '}'

    def _shorten(self, s, limit):
        head = max(0, (limit - 3) // 2)
        tail = max(0, limit - 3 - head)
        return head, tail

    def repr_str(self, x, level):
        s = builtins.repr(x[:self.maxstring])
        if len(s) > self.maxstring:
            i, j = self._shorten(s, self.maxstring)
            s = builtins.repr(x[:i] + x[len(x) - j:])
            s = s[:i] + self.fillvalue + s[len(s) - j:]
        return s

    def repr_int(self, x, level):
        try:
            s = builtins.repr(x)
        except ValueError as exc:
            assert 'sys.set_int_max_str_digits()' in str(exc)
            # Too many digits to convert: describe the number instead.
            import math, sys
            k = 1 + int(math.log10(abs(x)))
            limit = sys.get_int_max_str_digits()
            return (f'<{x.__class__.__name__} instance with roughly {k} '
                    f'digits (limit at {limit}) at 0x{id(x):x}>')
        if len(s) > self.maxlong:
            i, j = self._shorten(s, self.maxlong)
            s = s[:i] + self.fillvalue + s[len(s) - j:]
        return s

    def repr_instance(self, x, level):
        try:
            s = builtins.repr(x)
        except Exception:
            return f'<{x.__class__.__name__} instance at {id(x):#x}>'
        if len(s) > self.maxother:
            i, j = self._shorten(s, self.maxother)
            s = s[:i] + self.fillvalue + s[len(s) - j:]
        return s


aRepr = Repr()
repr = aRepr.repr
