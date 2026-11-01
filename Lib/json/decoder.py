"""JSON text -> Python data: JSONDecoder and its building blocks.

    >>> from json.decoder import JSONDecoder
    >>> JSONDecoder().decode('{"a": [1, 2.5, "x"]}')
    {'a': [1, 2.5, 'x']}
    >>> JSONDecoder().raw_decode('[0] trailing text')
    ([0], 3)
"""
import re

from json import scanner

try:
    from _json import scanstring as c_scanstring
except ImportError:
    c_scanstring = None

__all__ = ['JSONDecoder', 'JSONDecodeError']

NaN = float('nan')
PosInf = float('inf')
NegInf = float('-inf')

_CONSTANTS = {'NaN': NaN, 'Infinity': PosInf, '-Infinity': NegInf}


class JSONDecodeError(ValueError):
    """ValueError raised for malformed JSON, with the location attached.

    msg: the bare description of the problem
    doc: the document being parsed
    pos: index into doc where the problem was found
    lineno, colno: 1-based line and column of pos
    """

    def __init__(self, msg, doc, pos):
        line = doc.count('\n', 0, pos) + 1
        column = pos - doc.rfind('\n', 0, pos)
        super().__init__(
            f'{msg}: line {line} column {column} (char {pos})')
        self.msg = msg
        self.doc = doc
        self.pos = pos
        self.lineno = line
        self.colno = column

    def __reduce__(self):
        return type(self), (self.msg, self.doc, self.pos)


# --------------------------------------------------------------------------
# strings

# a run of characters that need no special treatment inside a string
_PLAIN_RUN = re.compile(r'[^"\\\x00-\x1f]*')

_ESCAPED = {
    '"': '"', '\\': '\\', '/': '/',
    'b': '\b', 'f': '\f', 'n': '\n', 'r': '\r', 't': '\t',
}

_HEX_DIGITS = frozenset('0123456789abcdefABCDEF')


def _hex_quad(s, pos):
    """Value of the four hex digits of a \\u escape whose 'u' is at pos-1."""
    digits = s[pos:pos + 4]
    if len(digits) == 4 and _HEX_DIGITS.issuperset(digits):
        return int(digits, 16)
    raise JSONDecodeError("Invalid \\uXXXX escape", s, pos - 2)


def py_scanstring(s, end, strict=True):
    """Decode the JSON string whose opening quote is at s[end - 1].

    Returns (decoded string, index just past the closing quote).  Unless
    strict is false, raw control characters inside the string are errors.
    """
    start = end - 1
    pieces = []
    size = len(s)
    while True:
        run = _PLAIN_RUN.match(s, end)
        if run.end() > end:
            pieces.append(run.group())
            end = run.end()
        if end >= size:
            raise JSONDecodeError("Unterminated string starting at", s, start)
        ch = s[end]
        if ch == '"':
            return ''.join(pieces), end + 1
        if ch != '\\':
            if strict:
                raise JSONDecodeError(
                    f"Invalid control character {ch!r} at", s, end)
            pieces.append(ch)
            end += 1
            continue
        # backslash escape
        if end + 1 >= size:
            raise JSONDecodeError("Unterminated string starting at", s, start)
        kind = s[end + 1]
        if kind != 'u':
            try:
                pieces.append(_ESCAPED[kind])
            except KeyError:
                raise JSONDecodeError(
                    f"Invalid \\escape: {kind!r}", s, end) from None
            end += 2
            continue
        code = _hex_quad(s, end + 2)
        end += 6
        if 0xD800 <= code < 0xDC00 and s[end:end + 2] == '\\u':
            low = _hex_quad(s, end + 2)
            if 0xDC00 <= low < 0xE000:
                code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00)
                end += 6
        pieces.append(chr(code))


scanstring = c_scanstring or py_scanstring


# --------------------------------------------------------------------------
# containers

WHITESPACE = re.compile(r'[ \t\n\r]*')

_skip = WHITESPACE.match


def JSONObject(s_and_end, strict, scan_once, object_hook, object_pairs_hook,
               memo=None, _w=_skip):
    """Parse the members of an object whose '{' precedes s_and_end[1].

    Returns (value, index past the closing '}'); the value is built by
    object_pairs_hook, else object_hook(dict), else a plain dict.  memo
    interns repeated keys.
    """
    s, end = s_and_end
    if memo is None:
        memo = {}
    remember = memo.setdefault
    pairs = []
    end = _w(s, end).end()
    if s[end:end + 1] != '}':
        while True:
            if s[end:end + 1] != '"':
                raise JSONDecodeError(
                    "Expecting property name enclosed in double quotes",
                    s, end)
            key, end = scanstring(s, end + 1, strict)
            key = remember(key, key)
            end = _w(s, end).end()
            if s[end:end + 1] != ':':
                raise JSONDecodeError("Expecting ':' delimiter", s, end)
            end = _w(s, end + 1).end()
            try:
                value, end = scan_once(s, end)
            except StopIteration as stop:
                raise JSONDecodeError("Expecting value", s,
                                      stop.value) from None
            pairs.append((key, value))
            end = _w(s, end).end()
            sep = s[end:end + 1]
            if sep == '}':
                break
            if sep != ',':
                raise JSONDecodeError("Expecting ',' delimiter", s, end)
            comma = end
            end = _w(s, end + 1).end()
            if s[end:end + 1] == '}':
                raise JSONDecodeError(
                    "Illegal trailing comma before end of object", s, comma)
    end += 1
    if object_pairs_hook is not None:
        return object_pairs_hook(pairs), end
    result = dict(pairs)
    if object_hook is not None:
        result = object_hook(result)
    return result, end


def JSONArray(s_and_end, scan_once, array_hook=None, _w=_skip):
    """Parse the elements of an array whose '[' precedes s_and_end[1].

    Returns (value, index past the closing ']'); the value is
    array_hook(list) when a hook is given.
    """
    s, end = s_and_end
    items = []
    end = _w(s, end).end()
    if s[end:end + 1] != ']':
        while True:
            try:
                value, end = scan_once(s, end)
            except StopIteration as stop:
                raise JSONDecodeError("Expecting value", s,
                                      stop.value) from None
            items.append(value)
            end = _w(s, end).end()
            sep = s[end:end + 1]
            if sep == ']':
                break
            if sep != ',':
                raise JSONDecodeError("Expecting ',' delimiter", s, end)
            comma = end
            end = _w(s, end + 1).end()
            if s[end:end + 1] == ']':
                raise JSONDecodeError(
                    "Illegal trailing comma before end of array", s, comma)
    end += 1
    if array_hook is not None:
        return array_hook(items), end
    return items, end


# --------------------------------------------------------------------------
# the decoder

class JSONDecoder(object):
    """Configurable JSON parser.

    JSON objects become dicts, arrays lists, strings str, numbers int or
    float, true/false/null True/False/None; the non-standard literals NaN,
    Infinity and -Infinity are accepted as the matching floats.

    Keyword arguments:

    object_hook       called with each decoded dict; its result is used
                      instead of the dict
    object_pairs_hook called with the list of (key, value) pairs of each
                      object, in document order; wins over object_hook
    array_hook        called with each decoded list; its result is used
                      instead
    parse_float       called with the text of each number that has a
                      fraction or exponent (default float)
    parse_int         called with the text of each integer (default int)
    parse_constant    called with 'NaN', 'Infinity' or '-Infinity'
    strict            when false, raw control characters are allowed
                      inside strings
    """

    def __init__(self, *, object_hook=None, parse_float=None,
                 parse_int=None, parse_constant=None, strict=True,
                 object_pairs_hook=None, array_hook=None):
        self.object_hook = object_hook
        self.object_pairs_hook = object_pairs_hook
        self.array_hook = array_hook
        self.parse_float = parse_float or float
        self.parse_int = parse_int or int
        self.parse_constant = parse_constant or _CONSTANTS.__getitem__
        self.strict = strict
        self.parse_object = JSONObject
        self.parse_array = JSONArray
        self.parse_string = scanstring
        self.memo = {}
        self.scan_once = scanner.make_scanner(self)

    def decode(self, s, _w=_skip):
        """Return the Python value of the JSON document *s* (a str).

        Only whitespace may surround the document.
        """
        value, end = self.raw_decode(s, idx=_w(s, 0).end())
        end = _w(s, end).end()
        if end != len(s):
            raise JSONDecodeError("Extra data", s, end)
        return value

    def raw_decode(self, s, idx=0):
        """Decode one JSON value starting exactly at s[idx].

        Returns (value, index just past it); text after it is ignored.
        """
        try:
            return self.scan_once(s, idx)
        except StopIteration as stop:
            raise JSONDecodeError("Expecting value", s, stop.value) from None
