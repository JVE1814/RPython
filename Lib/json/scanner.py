"""Value dispatch for the JSON decoder.

make_scanner(context) returns a callable ``scan(string, idx)`` that decodes
the single JSON value starting at ``string[idx]`` and returns
``(value, end)``.  When no value starts there it raises
``StopIteration(idx)``.  The context (normally a JSONDecoder) supplies the
container and string parsers, the hooks and the key memo.
"""
import re

try:
    from _json import make_scanner as c_make_scanner
except ImportError:
    c_make_scanner = None

__all__ = ['make_scanner']

# integer part, optional fraction, optional exponent (ASCII digits only)
NUMBER_RE = re.compile(r'(-?(?:0|[1-9][0-9]*))(\.[0-9]+)?([eE][-+]?[0-9]+)?')

# literal words and the value (or parse_constant argument) they stand for
_WORDS = (('null', None), ('true', True), ('false', False))
_SPECIAL_FLOATS = ('NaN', 'Infinity', '-Infinity')


class _PyScanner:
    """Pure-Python scanner bound to one decoder context."""

    def __init__(self, context):
        self.parse_object = context.parse_object
        self.parse_array = context.parse_array
        self.parse_string = context.parse_string
        self.strict = context.strict
        self.object_hook = context.object_hook
        self.object_pairs_hook = context.object_pairs_hook
        self.array_hook = getattr(context, 'array_hook', None)
        self.parse_float = context.parse_float
        self.parse_int = context.parse_int
        self.parse_constant = context.parse_constant
        self.memo = context.memo

    def __call__(self, string, idx):
        # the key memo only lives for one top-level value
        try:
            return self.value_at(string, idx)
        finally:
            self.memo.clear()

    def value_at(self, s, idx):
        if idx < 0 or idx >= len(s):
            raise StopIteration(idx)
        lead = s[idx]
        if lead == '"':
            return self.parse_string(s, idx + 1, self.strict)
        if lead == '{':
            return self.parse_object((s, idx + 1), self.strict,
                                     self.value_at, self.object_hook,
                                     self.object_pairs_hook, self.memo)
        if lead == '[':
            return self.parse_array((s, idx + 1), self.value_at,
                                    self.array_hook)
        for word, value in _WORDS:
            if s.startswith(word, idx):
                return value, idx + len(word)
        number = NUMBER_RE.match(s, idx)
        if number is not None:
            whole, fraction, exponent = number.groups()
            if fraction or exponent:
                value = self.parse_float(whole + (fraction or '') +
                                         (exponent or ''))
            else:
                value = self.parse_int(whole)
            return value, number.end()
        for word in _SPECIAL_FLOATS:
            if s.startswith(word, idx):
                return self.parse_constant(word), idx + len(word)
        raise StopIteration(idx)


def py_make_scanner(context):
    """Return a pure-Python scanner for *context* (see module docstring)."""
    return _PyScanner(context)


make_scanner = c_make_scanner or py_make_scanner
