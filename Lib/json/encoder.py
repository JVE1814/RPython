"""Python data -> JSON text: JSONEncoder and the string escapers.

    >>> from json.encoder import JSONEncoder
    >>> JSONEncoder(sort_keys=True).encode({"b": [1, 2.0], "a": None})
    '{"a": null, "b": [1, 2.0]}'
    >>> list(JSONEncoder().iterencode([True]))
    ['[true]']
"""
import re

try:
    import _json
except ImportError:
    _json = None

c_encode_basestring_ascii = getattr(_json, 'encode_basestring_ascii', None)
c_encode_basestring = getattr(_json, 'encode_basestring', None)
c_make_encoder = getattr(_json, 'make_encoder', None)

__all__ = ['JSONEncoder']

INFINITY = float('inf')

# --------------------------------------------------------------------------
# string literals

_NAMED_ESCAPES = {
    '"': '\\"', '\\': '\\\\',
    '\b': '\\b', '\f': '\\f', '\n': '\\n', '\r': '\\r', '\t': '\\t',
}

# characters that must be escaped in any JSON string
_MUST_ESCAPE = re.compile(r'["\\\x00-\x1f]')
# ... and additionally everything outside printable ASCII
_MUST_ESCAPE_ASCII = re.compile(r'["\\]|[^\x20-\x7e]')


def _escape(match):
    ch = match.group()
    named = _NAMED_ESCAPES.get(ch)
    if named is not None:
        return named
    code = ord(ch)
    if code > 0xFFFF:
        # astral characters become a UTF-16 surrogate pair
        code -= 0x10000
        return '\\u{:04x}\\u{:04x}'.format(0xD800 + (code >> 10),
                                           0xDC00 + (code & 0x3FF))
    return '\\u{:04x}'.format(code)


def py_encode_basestring(s):
    """JSON string literal for *s*, keeping non-ASCII characters."""
    return '"' + _MUST_ESCAPE.sub(_escape, s) + '"'


def py_encode_basestring_ascii(s):
    """JSON string literal for *s* using only ASCII characters."""
    return '"' + _MUST_ESCAPE_ASCII.sub(_escape, s) + '"'


encode_basestring = c_encode_basestring or py_encode_basestring
encode_basestring_ascii = (c_encode_basestring_ascii or
                           py_encode_basestring_ascii)


# --------------------------------------------------------------------------
# the encoder

class JSONEncoder(object):
    """Configurable serialiser of Python data to JSON.

    Natively supported:

        dict (and frozendict)  -> object      list, tuple   -> array
        str                    -> string      int, float    -> number
        True / False           -> true/false  None          -> null

    int and float subclasses (enums included) are written as their numeric
    value.  For anything else the default() method is consulted; subclasses
    override it to return a serialisable replacement.
    """
    item_separator = ', '
    key_separator = ': '

    def __init__(self, *, skipkeys=False, ensure_ascii=True,
                 check_circular=True, allow_nan=True, sort_keys=False,
                 indent=None, separators=None, default=None):
        """Options:

        skipkeys        silently drop dict keys that are not str, int,
                        float, bool or None (otherwise TypeError)
        ensure_ascii    escape every non-ASCII character
        check_circular  detect reference cycles (ValueError) instead of
                        recursing until RecursionError
        allow_nan       write NaN/Infinity/-Infinity for non-finite floats
                        (otherwise ValueError)
        sort_keys       emit object members ordered by key
        indent          None for single-line output, else a string or a
                        number of spaces used per nesting level (members
                        then go on separate lines)
        separators      (item_separator, key_separator); defaults to
                        (', ', ': '), or (',', ': ') when indenting
        default         callable used for objects that cannot otherwise be
                        serialised
        """
        self.skipkeys = skipkeys
        self.ensure_ascii = ensure_ascii
        self.check_circular = check_circular
        self.allow_nan = allow_nan
        self.sort_keys = sort_keys
        self.indent = indent
        if separators is not None:
            self.item_separator, self.key_separator = separators
        elif indent is not None:
            self.item_separator = ','
        if default is not None:
            self.default = default

    def default(self, o):
        """Return a serialisable stand-in for *o*, or raise TypeError.

        Override in a subclass, e.g. to encode iterables as arrays::

            def default(self, o):
                if hasattr(o, '__iter__'):
                    return list(o)
                return super().default(o)
        """
        raise TypeError(f'Object of type {o.__class__.__name__} '
                        f'is not JSON serializable')

    def encode(self, o):
        """Return the JSON text of *o* as one string."""
        if isinstance(o, str):
            return self._string_encoder()(o)
        pieces = self.iterencode(o, _one_shot=True)
        if not isinstance(pieces, (list, tuple)):
            pieces = list(pieces)
        return ''.join(pieces)

    def iterencode(self, o, _one_shot=False):
        """Yield the JSON text of *o* piece by piece (for streaming)."""
        markers = {} if self.check_circular else None
        quote = self._string_encoder()
        indent = self.indent
        if indent is not None and not isinstance(indent, str):
            indent = ' ' * indent
        if _one_shot and c_make_encoder is not None:
            emit = c_make_encoder(markers, self.default, quote, indent,
                                  self.key_separator, self.item_separator,
                                  self.sort_keys, self.skipkeys,
                                  self.allow_nan)
            return emit(o, 0)
        writer = _Writer(markers, self.default, quote, indent,
                         self.key_separator, self.item_separator,
                         self.sort_keys, self.skipkeys, self.allow_nan)
        return writer.value(o, 0)

    def _string_encoder(self):
        return encode_basestring_ascii if self.ensure_ascii \
            else encode_basestring


def _float_text(value, allow_nan):
    if value != value:
        text = 'NaN'
    elif value == INFINITY:
        text = 'Infinity'
    elif value == -INFINITY:
        text = '-Infinity'
    else:
        return float.__repr__(value)
    if not allow_nan:
        raise ValueError(
            'Out of range float values are not JSON compliant: '
            + repr(value))
    return text


class _Writer:
    """Generator-based serialiser used by iterencode() (and whenever the
    C accelerator is unavailable)."""

    def __init__(self, markers, default, quote, indent, key_separator,
                 item_separator, sort_keys, skipkeys, allow_nan):
        self.markers = markers
        self.default = default
        self.quote = quote
        self.indent = indent
        self.key_separator = key_separator
        self.item_separator = item_separator
        self.sort_keys = sort_keys
        self.skipkeys = skipkeys
        self.allow_nan = allow_nan

    # -- helpers ----------------------------------------------------------

    def scalar(self, o):
        """Text of a scalar value, or None if *o* is not a scalar."""
        if isinstance(o, str):
            return self.quote(o)
        if o is None:
            return 'null'
        if o is True:
            return 'true'
        if o is False:
            return 'false'
        if isinstance(o, int):
            return int.__repr__(o)
        if isinstance(o, float):
            return _float_text(o, self.allow_nan)
        return None

    def key_text(self, key):
        """Text of a dict key as a JSON string body, or None to skip it."""
        if isinstance(key, str):
            return key
        if isinstance(key, float):
            return _float_text(key, self.allow_nan)
        if key is True:
            return 'true'
        if key is False:
            return 'false'
        if key is None:
            return 'null'
        if isinstance(key, int):
            return int.__repr__(key)
        if self.skipkeys:
            return None
        raise TypeError(f'keys must be str, int, float, bool or None, '
                        f'not {key.__class__.__name__}')

    def enter(self, o):
        if self.markers is not None:
            ident = id(o)
            if ident in self.markers:
                raise ValueError("Circular reference detected")
            self.markers[ident] = o

    def leave(self, o):
        if self.markers is not None:
            del self.markers[id(o)]

    def layout(self, level):
        """(opening break, item separator, closing break) for a container
        whose contents sit at nesting *level*."""
        if self.indent is None:
            return '', self.item_separator, ''
        inner = '\n' + self.indent * level
        outer = '\n' + self.indent * (level - 1)
        return inner, self.item_separator + inner, outer

    # -- generators -------------------------------------------------------

    def value(self, o, level):
        text = self.scalar(o)
        if text is not None:
            yield text
        elif isinstance(o, (list, tuple)):
            yield from self.sequence(o, level)
        elif isinstance(o, (dict, frozendict)):
            yield from self.mapping(o, level)
        else:
            self.enter(o)
            replacement = self.default(o)
            try:
                yield from self.value(replacement, level)
            except BaseException as exc:
                exc.add_note(f'when serializing {type(o).__name__} object')
                raise
            self.leave(o)

    def sequence(self, seq, level):
        if not seq:
            yield '[]'
            return
        self.enter(seq)
        level += 1
        first, between, last = self.layout(level)
        yield '[' + first
        for index, item in enumerate(seq):
            if index:
                yield between
            text = self.scalar(item)
            if text is not None:
                yield text
                continue
            try:
                yield from self.value(item, level)
            except BaseException as exc:
                exc.add_note(
                    f'when serializing {type(seq).__name__} item {index}')
                raise
        yield last + ']'
        self.leave(seq)

    def mapping(self, mapping, level):
        if not mapping:
            yield '{}'
            return
        self.enter(mapping)
        level += 1
        first, between, last = self.layout(level)
        members = mapping.items()
        if self.sort_keys:
            members = sorted(members)
        started = False
        for key, item in members:
            name = self.key_text(key)
            if name is None:
                continue
            # the opening brace waits for the first kept member, so that a
            # mapping whose keys are all skipped still prints as {}
            yield (between if started else '{' + first)
            started = True
            yield self.quote(name) + self.key_separator
            text = self.scalar(item)
            if text is not None:
                yield text
                continue
            try:
                yield from self.value(item, level)
            except BaseException as exc:
                exc.add_note(
                    f'when serializing {type(mapping).__name__} item {key!r}')
                raise
        yield (last + '}') if started else '{}'
        self.leave(mapping)
