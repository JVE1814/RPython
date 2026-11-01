"""A hand-written TOML 1.1 parser (recursive descent over the source str).

Regular expressions are deliberately not used, so importing and using
tomllib stays cheap.
"""

from types import MappingProxyType

# Dotted keys deeper than this are rejected (protects the stack and the
# path bookkeeping from absurd input).
MAX_KEY_PARTS = 1000

_WS = frozenset(' \t')
_BARE_KEY_CHARS = frozenset(
    'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-')
_DIGITS = frozenset('0123456789')
_HEX = frozenset('0123456789abcdefABCDEF')
_NUMBER_CHARS = frozenset(
    '0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_+-.')
_VALUE_END = frozenset(' \t\r\n,]}#')

_SIMPLE_ESCAPES = MappingProxyType({
    'b': '\b', 't': '\t', 'n': '\n', 'f': '\f', 'r': '\r',
    'e': '\x1b', '"': '"', '\\': '\\',
})
_HEX_ESCAPE_LENGTH = MappingProxyType({'x': 2, 'u': 4, 'U': 8})

_MISSING = object()


class TOMLDecodeError(ValueError):
    """Raised for a document that is not valid TOML.

    Attributes: msg (the bare message), doc (the document), pos (index of
    the error), lineno and colno (1-based position of the error).
    """

    def __init__(self, msg=_MISSING, doc=_MISSING, pos=_MISSING, *args):
        if (args or not isinstance(msg, str) or not isinstance(doc, str)
                or not isinstance(pos, int)):
            import warnings
            warnings.warn(
                "Free-form arguments for TOMLDecodeError are deprecated. "
                "Please set 'msg' (str), 'doc' (str) and 'pos' (int) "
                "arguments only.",
                DeprecationWarning,
                stacklevel=2,
            )
            given = [a for a in (msg, doc, pos) if a is not _MISSING]
            ValueError.__init__(self, *given, *args)
            return
        lineno = doc.count('\n', 0, pos) + 1
        if lineno == 1:
            colno = pos + 1
        else:
            colno = pos - doc.rindex('\n', 0, pos)
        if pos >= len(doc):
            where = "end of document"
        else:
            where = f"line {lineno}, column {colno}"
        ValueError.__init__(self, f"{msg} (at {where})")
        self.msg = msg
        self.doc = doc
        self.pos = pos
        self.lineno = lineno
        self.colno = colno


def load(fp, /, *, parse_float=float):
    """Parse a TOML document read from a binary file object."""
    data = fp.read()
    try:
        text = data.decode()
    except AttributeError:
        raise TypeError(
            "File must be opened in binary mode, e.g. use "
            "`open('foo.toml', 'rb')`") from None
    return loads(text, parse_float=parse_float)


def loads(s, /, *, parse_float=float):
    """Parse a TOML document from a str and return it as a dict."""
    if not isinstance(s, str):
        raise TypeError(f"Expected str object, not '{type(s).__qualname__}'")
    return _Parser(s, parse_float).parse()


def _is_control(ch):
    code = ord(ch)
    return (code < 0x20 and ch != '\t') or code == 0x7f


class _Parser:

    def __init__(self, src, parse_float):
        self.src = src
        self.n = len(src)
        self.pos = 0
        self.user_parse_float = parse_float
        self.root = {}
        # bookkeeping of table paths (tuples; array-of-tables elements are
        # addressed by their index)
        self.header_tables = set()   # created (implicitly too) by headers
        self.explicit = set()        # defined by a [header]
        self.dotted = set()          # created by dotted keys
        self.frozen = set()          # inline tables and static arrays
        self.aot = set()             # arrays of tables

    # -- errors ---------------------------------------------------------------

    def error(self, msg, pos=None):
        if pos is None:
            pos = self.pos
        return TOMLDecodeError(msg, self.src, pos)

    def illegal_char(self, pos):
        return self.error(f"Illegal character {self.src[pos]!r}", pos)

    # -- low level scanning ------------------------------------------------

    def peek(self, offset=0):
        p = self.pos + offset
        return self.src[p] if p < self.n else ''

    def skip_ws(self):
        src, n, p = self.src, self.n, self.pos
        while p < n and src[p] in _WS:
            p += 1
        self.pos = p

    def skip_comment(self):
        src, n, p = self.src, self.n, self.pos
        if p < n and src[p] == '#':
            p += 1
            while p < n and src[p] != '\n':
                ch = src[p]
                if _is_control(ch) and not (ch == '\r' and src.startswith('\n', p + 1)):
                    raise self.illegal_char(p)
                p += 1
        self.pos = p

    def at_newline(self):
        """Consume a newline if there is one; return whether it did."""
        src, p = self.src, self.pos
        if src.startswith('\n', p):
            self.pos = p + 1
            return True
        if src.startswith('\r\n', p):
            self.pos = p + 2
            return True
        return False

    def skip_blank(self):
        """Skip whitespace, comments and newlines (inside arrays and inline
        tables)."""
        while True:
            self.skip_ws()
            self.skip_comment()
            if not self.at_newline():
                return

    # -- document level --------------------------------------------------

    def parse(self):
        section = self.root
        section_path = ()
        while True:
            self.skip_ws()
            self.skip_comment()
            if self.at_newline():
                continue
            if self.pos >= self.n:
                break
            ch = self.src[self.pos]
            if ch == '[':
                section, section_path = self.parse_header()
            elif ch in _BARE_KEY_CHARS or ch == '"' or ch == "'":
                self.parse_keyval(section, section_path)
            else:
                raise self.error("Invalid statement")
            self.skip_ws()
            self.skip_comment()
            if self.pos < self.n and not self.at_newline():
                raise self.error(
                    "Expected newline or end of document after a statement")
        return self.root

    def parse_header(self):
        start = self.pos
        is_aot = self.src.startswith('[[', self.pos)
        self.pos += 2 if is_aot else 1
        self.skip_ws()
        keys = self.parse_key()
        self.skip_ws()
        closing = ']]' if is_aot else ']'
        if not self.src.startswith(closing, self.pos):
            if is_aot:
                raise self.error("Expected ']]' at the end of an array declaration")
            raise self.error("Expected ']' at the end of a table declaration")
        self.pos += len(closing)
        return self.open_table(keys, is_aot, start)

    def open_table(self, keys, is_aot, start):
        node = self.root
        path = ()
        last = len(keys) - 1
        for i, key in enumerate(keys):
            path = path + (key,)
            child = node.get(key, _MISSING)
            if i == last:
                if is_aot:
                    if child is _MISSING:
                        child = []
                        node[key] = child
                        self.aot.add(path)
                        self.header_tables.add(path)
                    elif not (isinstance(child, list) and path in self.aot):
                        raise self.error(f"Cannot overwrite a value", start)
                    table = {}
                    child.append(table)
                    path = path + (len(child) - 1,)
                    self.explicit.add(path)
                    self.header_tables.add(path)
                    return table, path
                if child is _MISSING:
                    child = {}
                    node[key] = child
                elif (not isinstance(child, dict) or path in self.explicit
                      or path in self.dotted or path in self.frozen):
                    raise self.error("Cannot declare the same table twice", start)
                self.explicit.add(path)
                self.header_tables.add(path)
                return child, path
            if child is _MISSING:
                child = {}
                node[key] = child
                self.header_tables.add(path)
            elif isinstance(child, dict):
                if path in self.frozen:
                    raise self.error("Cannot mutate an inline table", start)
            elif isinstance(child, list) and path in self.aot:
                path = path + (len(child) - 1,)
                child = child[-1]
            else:
                raise self.error("Cannot overwrite a value", start)
            node = child
        raise AssertionError("unreachable")

    def parse_keyval(self, section, section_path):
        start = self.pos
        keys = self.parse_key()
        self.skip_ws()
        if self.peek() != '=':
            raise self.error("Expected '=' after a key in a key/value pair")
        self.pos += 1
        self.skip_ws()
        value, frozen = self.parse_value()
        self.insert(section, section_path, keys, value, frozen, start,
                    self.frozen, self.header_tables, self.dotted)

    def insert(self, node, path, keys, value, value_frozen, start,
               frozen, header_tables, dotted):
        for key in keys[:-1]:
            path = path + (key,)
            child = node.get(key, _MISSING)
            if child is _MISSING:
                child = {}
                node[key] = child
                dotted.add(path)
            elif (not isinstance(child, dict) or path in frozen
                  or path in header_tables):
                raise self.error("Cannot overwrite a value", start)
            node = child
        key = keys[-1]
        if key in node:
            raise self.error("Cannot overwrite a value", start)
        node[key] = value
        if value_frozen:
            frozen.add(path + (key,))

    # -- keys -------------------------------------------------------------

    def parse_key(self):
        parts = [self.parse_key_part()]
        while True:
            save = self.pos
            self.skip_ws()
            if self.peek() != '.':
                self.pos = save
                break
            self.pos += 1
            self.skip_ws()
            parts.append(self.parse_key_part())
            if len(parts) > MAX_KEY_PARTS:
                raise RecursionError(
                    f"TOML key has more than the allowed {MAX_KEY_PARTS} parts")
        return tuple(parts)

    def parse_key_part(self):
        src, n = self.src, self.n
        p = self.pos
        if p < n:
            ch = src[p]
            if ch == '"':
                return self.parse_basic_string()
            if ch == "'":
                return self.parse_literal_string()
            q = p
            while q < n and src[q] in _BARE_KEY_CHARS:
                q += 1
            if q > p:
                self.pos = q
                return src[p:q]
        raise self.error("Invalid initial character for a key part")

    # -- values -----------------------------------------------------------

    def parse_value(self):
        """Parse a value at the current position.  Returns (value, frozen),
        where frozen tells whether the value is an inline table or array."""
        src = self.src
        p = self.pos
        ch = src[p] if p < self.n else ''
        if ch == '"':
            if src.startswith('"""', p):
                return self.parse_ml_basic_string(), False
            return self.parse_basic_string(), False
        if ch == "'":
            if src.startswith("'''", p):
                return self.parse_ml_literal_string(), False
            return self.parse_literal_string(), False
        if ch == '[':
            self.pos += 1
            result = []
            self.skip_blank()
            while self.peek() != ']':
                if self.pos >= self.n:
                    raise self.error("Unclosed array")
                item, _ = self.parse_value()
                result.append(item)
                self.skip_blank()
                c = self.peek()
                if c == ',':
                    self.pos += 1
                    self.skip_blank()
                elif c != ']':
                    raise self.error("Unclosed array")
            self.pos += 1
            return result, True
        if ch == '{':
            return self.parse_inline_table(), True
        if src.startswith('true', p) and self.ends_value(p + 4):
            self.pos = p + 4
            return True, False
        if src.startswith('false', p) and self.ends_value(p + 5):
            self.pos = p + 5
            return False, False
        if ch in _DIGITS:
            if self.looks_like_date(p) or self.looks_like_time(p):
                return self.parse_datetime(), False
        if ch in _NUMBER_CHARS and ch:
            return self.parse_number(), False
        raise self.error("Invalid value")

    def ends_value(self, p):
        return p >= self.n or self.src[p] in _VALUE_END

    def parse_inline_table(self):
        start = self.pos
        self.pos += 1
        table = {}
        frozen = set()
        dotted = set()
        no_headers = frozenset()
        self.skip_blank()
        while self.peek() != '}':
            if self.pos >= self.n:
                raise self.error("Unclosed inline table")
            key_start = self.pos
            keys = self.parse_key()
            self.skip_ws()
            if self.peek() != '=':
                raise self.error("Expected '=' after a key in a key/value pair")
            self.pos += 1
            self.skip_ws()
            value, value_frozen = self.parse_value()
            self.insert(table, (), keys, value, value_frozen, key_start,
                        frozen, no_headers, dotted)
            self.skip_blank()
            c = self.peek()
            if c == ',':
                self.pos += 1
                self.skip_blank()
            elif c != '}':
                if self.pos >= self.n:
                    raise self.error("Unclosed inline table")
                raise self.error("Expected ',' or '}' after inline table value")
        self.pos += 1
        return table

    # -- strings ------------------------------------------------------------

    def parse_literal_string(self):
        src, n = self.src, self.n
        p = self.pos + 1
        start = p
        while True:
            if p >= n:
                raise self.error("Unterminated string", p)
            ch = src[p]
            if ch == "'":
                break
            if _is_control(ch):
                raise self.illegal_char(p)
            p += 1
        self.pos = p + 1
        return src[start:p]

    def parse_ml_literal_string(self):
        src, n = self.src, self.n
        p = self.pos + 3
        if src.startswith('\n', p):
            p += 1
        elif src.startswith('\r\n', p):
            p += 2
        start = p
        while True:
            if p >= n:
                raise self.error("Unterminated string", p)
            ch = src[p]
            if ch == "'" and src.startswith("'''", p):
                break
            if ch == '\r' and src.startswith('\n', p + 1):
                p += 2
                continue
            if _is_control(ch) and ch != '\n':
                raise self.illegal_char(p)
            p += 1
        end = p
        p = self.close_ml(p, "'")
        self.pos = p
        return src[start:end] + "'" * (p - end - 3)

    def close_ml(self, p, quote):
        """*p* points at a triple quote; up to two more quotes belong to the
        string content.  Returns the index after the delimiter."""
        q = p + 3
        extra = 0
        while extra < 2 and self.src.startswith(quote, q):
            q += 1
            extra += 1
        return q

    def parse_basic_string(self):
        src, n = self.src, self.n
        p = self.pos + 1
        parts = []
        chunk = p
        while True:
            if p >= n:
                raise self.error("Unterminated string", p)
            ch = src[p]
            if ch == '"':
                parts.append(src[chunk:p])
                break
            if ch == '\\':
                parts.append(src[chunk:p])
                text, p = self.parse_escape(p, False)
                parts.append(text)
                chunk = p
                continue
            if _is_control(ch):
                raise self.illegal_char(p)
            p += 1
        self.pos = p + 1
        return ''.join(parts)

    def parse_ml_basic_string(self):
        src, n = self.src, self.n
        p = self.pos + 3
        if src.startswith('\n', p):
            p += 1
        elif src.startswith('\r\n', p):
            p += 2
        parts = []
        chunk = p
        while True:
            if p >= n:
                raise self.error("Unterminated string", p)
            ch = src[p]
            if ch == '"' and src.startswith('"""', p):
                parts.append(src[chunk:p])
                break
            if ch == '\\':
                parts.append(src[chunk:p])
                text, p = self.parse_escape(p, True)
                parts.append(text)
                chunk = p
                continue
            if ch == '\r' and src.startswith('\n', p + 1):
                p += 2
                continue
            if _is_control(ch) and ch != '\n':
                raise self.illegal_char(p)
            p += 1
        end = p
        p = self.close_ml(p, '"')
        self.pos = p
        parts.append('"' * (p - end - 3))
        return ''.join(parts)

    def parse_escape(self, p, multiline):
        """Decode the escape sequence at src[p] == '\\'.  Returns the text
        and the index after the sequence."""
        src, n = self.src, self.n
        if p + 1 >= n:
            raise self.error("Unterminated string", n)
        kind = src[p + 1]
        simple = _SIMPLE_ESCAPES.get(kind)
        if simple is not None:
            return simple, p + 2
        length = _HEX_ESCAPE_LENGTH.get(kind)
        if length is not None:
            digits = src[p + 2:p + 2 + length]
            if len(digits) != length or not all(c in _HEX for c in digits):
                raise self.error("Invalid hex value", p + 2)
            code = int(digits, 16)
            if 0xd800 <= code <= 0xdfff or code > 0x10ffff:
                raise self.error(
                    "Escaped character is not a Unicode scalar value", p)
            return chr(code), p + 2 + length
        if multiline:
            # a line ending backslash: skip whitespace up to and including
            # the next non-blank line
            q = p + 1
            while q < n and src[q] in _WS:
                q += 1
            if src.startswith('\n', q) or src.startswith('\r\n', q):
                while q < n:
                    c = src[q]
                    if c in _WS or c == '\n':
                        q += 1
                    elif c == '\r' and src.startswith('\n', q + 1):
                        q += 2
                    else:
                        break
                return '', q
        raise self.error("Unescaped '\\' in a string", p)

    # -- numbers ----------------------------------------------------------

    def parse_float_text(self, text):
        value = self.user_parse_float(text)
        if isinstance(value, (dict, list)):
            raise ValueError("parse_float must not return dicts or lists")
        return value

    def parse_number(self):
        src, n = self.src, self.n
        start = p = self.pos
        while p < n and src[p] in _NUMBER_CHARS:
            p += 1
        token = src[start:p]
        self.pos = p
        result = self.convert_number(token)
        if result is None:
            raise self.error("Invalid value", start)
        return result

    def convert_number(self, token):
        body = token
        sign = ''
        if body[:1] in ('+', '-'):
            sign = body[0]
            body = body[1:]
        if body in ('inf', 'nan'):
            return self.parse_float_text(token)
        if body[:2] in ('0x', '0o', '0b'):
            if sign:
                return None
            base = {'x': 16, 'o': 8, 'b': 2}[body[1]]
            digits = body[2:]
            allowed = '0123456789abcdefABCDEF'[:base + (6 if base == 16 else 0)]
            if not _valid_underscored(digits, allowed):
                return None
            return int(digits.replace('_', ''), base)
        # decimal integer or float
        mantissa = body
        exponent = None
        for marker in ('e', 'E'):
            if marker in mantissa:
                mantissa, exponent = mantissa.split(marker, 1)
                break
        int_part, dot, frac = mantissa.partition('.')
        if not _valid_underscored(int_part, '0123456789'):
            return None
        if len(int_part) > 1 and int_part[0] == '0':
            return None
        if dot and not _valid_underscored(frac, '0123456789'):
            return None
        if exponent is not None:
            exp_digits = exponent
            if exp_digits[:1] in ('+', '-'):
                exp_digits = exp_digits[1:]
            if not _valid_underscored(exp_digits, '0123456789'):
                return None
        if not dot and exponent is None:
            return int(token.replace('_', ''))
        return self.parse_float_text(token.replace('_', ''))

    # -- dates and times ----------------------------------------------------

    def looks_like_date(self, p):
        src = self.src
        return (p + 4 < self.n and src[p + 4] == '-'
                and all(c in _DIGITS for c in src[p:p + 4]))

    def looks_like_time(self, p):
        src = self.src
        return (p + 2 < self.n and src[p + 2] == ':'
                and all(c in _DIGITS for c in src[p:p + 2]))

    def take_digits(self, count):
        p = self.pos
        text = self.src[p:p + count]
        if len(text) != count or not all(c in _DIGITS for c in text):
            raise _BadDatetime
        self.pos = p + count
        return int(text)

    def expect(self, ch):
        if self.peek() != ch:
            raise _BadDatetime
        self.pos += 1

    def parse_time_part(self):
        hour = self.take_digits(2)
        self.expect(':')
        minute = self.take_digits(2)
        second = 0
        micro = 0
        if self.peek() == ':':
            self.pos += 1
            second = self.take_digits(2)
            if self.peek() == '.':
                self.pos += 1
                p = q = self.pos
                while q < self.n and self.src[q] in _DIGITS:
                    q += 1
                if q == p:
                    raise _BadDatetime
                micro = int(self.src[p:q][:6].ljust(6, '0'))
                self.pos = q
        return hour, minute, second, micro

    def parse_datetime(self):
        import datetime
        start = self.pos
        try:
            if self.looks_like_time(start):
                hour, minute, second, micro = self.parse_time_part()
                if not self.ends_value(self.pos):
                    raise _BadDatetime
                return datetime.time(hour, minute, second, micro)
            year = self.take_digits(4)
            self.expect('-')
            month = self.take_digits(2)
            self.expect('-')
            day = self.take_digits(2)
            src, p = self.src, self.pos
            has_time = False
            if p < self.n and src[p] in 'Tt':
                has_time = True
            elif (src.startswith(' ', p) and p + 3 < self.n
                  and src[p + 1] in _DIGITS and src[p + 2] in _DIGITS
                  and src[p + 3] == ':'):
                has_time = True
            if not has_time:
                if not self.ends_value(p):
                    raise _BadDatetime
                return datetime.date(year, month, day)
            self.pos = p + 1
            hour, minute, second, micro = self.parse_time_part()
            tz = None
            c = self.peek()
            if c in ('Z', 'z') and c:
                self.pos += 1
                tz = datetime.timezone.utc
            elif c in ('+', '-') and c:
                self.pos += 1
                off_h = self.take_digits(2)
                self.expect(':')
                off_m = self.take_digits(2)
                delta = datetime.timedelta(hours=off_h, minutes=off_m)
                if c == '-':
                    delta = -delta
                tz = datetime.timezone(delta)
            if not self.ends_value(self.pos):
                raise _BadDatetime
            return datetime.datetime(year, month, day, hour, minute, second,
                                     micro, tzinfo=tz)
        except (_BadDatetime, ValueError):
            raise self.error("Invalid date or datetime", start) from None


class _BadDatetime(Exception):
    pass


def _valid_underscored(text, allowed):
    """Digits from *allowed*, with single underscores between digits."""
    if not text or text[0] == '_' or text[-1] == '_':
        return False
    previous = ''
    for ch in text:
        if ch == '_':
            if previous == '_':
                return False
        elif ch not in allowed:
            return False
        previous = ch
    return True
