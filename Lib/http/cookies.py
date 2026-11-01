"""HTTP state management (cookies) for servers.

A SimpleCookie is a dictionary of Morsel objects keyed by cookie name.
Each Morsel holds the cookie value and its attributes (path, domain,
expires, ...).  load() parses a Cookie: header value; output() produces
Set-Cookie: header lines.

    >>> from http.cookies import SimpleCookie
    >>> c = SimpleCookie()
    >>> c['flavour'] = 'oatmeal'
    >>> c['flavour']['path'] = '/'
    >>> print(c)
    Set-Cookie: flavour=oatmeal; Path=/
"""

import re
import string
import types

__all__ = ['CookieError', 'BaseCookie', 'SimpleCookie']


class CookieError(Exception):
    pass


# Characters allowed in a cookie name and in an unquoted value.
_LegalChars = string.ascii_letters + string.digits + "!#$%&'*+-.^_`|~:"
_UnescapedChars = _LegalChars + ' ()/<=>?@[]{}'

_C0_CONTROLS = frozenset(chr(c) for c in range(0x20)) | {'\x7f'}


def _has_control_character(*values):
    for value in values:
        if value is None:
            continue
        if not isinstance(value, str):
            value = str(value)
        if not _C0_CONTROLS.isdisjoint(value):
            return True
    return False


def _escape_char(ch):
    if ch == '"' or ch == '\\':
        return '\\' + ch
    if ch in _UnescapedChars:
        return ch
    return '\\%03o' % ord(ch)


def _quote(text):
    """Quote *text* for use as a cookie value if it needs quoting."""
    if text is None or all(ch in _LegalChars for ch in text):
        return text
    return '"' + ''.join(map(_escape_char, text)) + '"'


_UNQUOTE_RE = re.compile(r'\\(?:([0-3][0-7][0-7])|(.))', re.DOTALL)


def _unescape(match):
    octal, other = match.groups()
    if octal is not None:
        return chr(int(octal, 8))
    return other


def _unquote(text):
    """Undo _quote(): strip the quotes and resolve backslash escapes."""
    if text is None or len(text) < 2:
        return text
    if text[0] != '"' or text[-1] != '"':
        return text
    body = text[1:-1]
    if '\\' not in body:
        return body
    return _UNQUOTE_RE.sub(_unescape, body)


_WEEKDAYS = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun']
_MONTHS = [None, 'Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun',
           'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec']


def _getdate(future=0, weekdayname=_WEEKDAYS, monthname=_MONTHS):
    """An HTTP date *future* seconds from now."""
    from time import gmtime, time
    tm = gmtime(time() + future)
    return '%s, %02d %3s %4d %02d:%02d:%02d GMT' % (
        weekdayname[tm.tm_wday], tm.tm_mday, monthname[tm.tm_mon],
        tm.tm_year, tm.tm_hour, tm.tm_min, tm.tm_sec)


class Morsel(dict):
    """One cookie: key, value, coded (quoted) value plus attributes.

    The attributes are the dictionary items; only the reserved names
    (case-insensitive) may be used.
    """

    _reserved = {
        'expires': 'expires',
        'path': 'Path',
        'comment': 'Comment',
        'domain': 'Domain',
        'max-age': 'Max-Age',
        'secure': 'Secure',
        'httponly': 'HttpOnly',
        'version': 'Version',
        'samesite': 'SameSite',
        'partitioned': 'Partitioned',
    }

    _flags = {'secure', 'httponly', 'partitioned'}

    def __init__(self):
        self._key = self._value = self._coded_value = None
        for name in self._reserved:
            dict.__setitem__(self, name, '')

    @property
    def key(self):
        return self._key

    @property
    def value(self):
        return self._value

    @property
    def coded_value(self):
        return self._coded_value

    def _check_attribute(self, key, value):
        key = key.lower()
        if key not in self._reserved:
            raise CookieError(f'Invalid attribute {key!r}')
        if _has_control_character(value):
            raise CookieError(f'Control characters are not allowed in '
                              f'cookies {key!r} {value!r}')
        return key

    def __setitem__(self, key, value):
        dict.__setitem__(self, self._check_attribute(key, value), value)

    def setdefault(self, key, val=None):
        return dict.setdefault(self, self._check_attribute(key, val), val)

    def __eq__(self, other):
        if not isinstance(other, Morsel):
            return NotImplemented
        return (dict.__eq__(self, other) and
                self._value == other._value and
                self._key == other._key and
                self._coded_value == other._coded_value)

    __ne__ = object.__ne__
    __hash__ = None

    def copy(self):
        clone = Morsel()
        dict.update(clone, self)
        clone.__dict__.update(self.__dict__)
        return clone

    def update(self, values):
        checked = {}
        for key, value in dict(values).items():
            checked[self._check_attribute(key, value)] = value
        dict.update(self, checked)

    def __ior__(self, values):
        self.update(values)
        return self

    def isReservedKey(self, K):
        return K.lower() in self._reserved

    def set(self, key, val, coded_val):
        if key.lower() in self._reserved:
            raise CookieError(f'Attempt to set a reserved key {key!r}')
        if not key or not all(ch in _LegalChars for ch in key):
            raise CookieError(f'Illegal key {key!r}')
        if _has_control_character(key, val, coded_val):
            raise CookieError(f'Control characters are not allowed in '
                              f'cookies {key!r} {val!r} {coded_val!r}')
        self._key = key
        self._value = val
        self._coded_value = coded_val

    def __getstate__(self):
        return {'key': self._key, 'value': self._value,
                'coded_value': self._coded_value}

    def __setstate__(self, state):
        key, value, coded = state['key'], state['value'], state['coded_value']
        if _has_control_character(key, value, coded):
            raise CookieError(f'Control characters are not allowed in '
                              f'cookies {key!r} {value!r} {coded!r}')
        self._key = key
        self._value = value
        self._coded_value = coded

    def _check_output(self):
        if _has_control_character(self._key, self._coded_value):
            raise CookieError('Control characters are not allowed in '
                              'cookies')

    def output(self, attrs=None, header='Set-Cookie:'):
        self._check_output()
        return f'{header} {self.OutputString(attrs)}'

    __str__ = output

    def __repr__(self):
        return f'<{self.__class__.__name__}: {self.OutputString()}>'

    def _js_output(self, attrs=None):
        self._check_output()
        from urllib.parse import quote
        encoded = quote(self.OutputString(attrs), safe='', encoding='utf-8')
        return """
        <script type="text/javascript">
        <!-- begin hiding
        document.cookie = decodeURIComponent("%s");
        // end hiding -->
        </script>
        """ % (encoded,)

    def js_output(self, attrs=None):
        import warnings
        warnings.warn('Morsel.js_output is deprecated and slated for '
                      'removal in Python 3.19', DeprecationWarning,
                      stacklevel=2)
        return self._js_output(attrs)

    def OutputString(self, attrs=None):
        """The cookie as used in a Set-Cookie header (without the
        header name)."""
        parts = [f'{self.key}={self.coded_value}']
        for key, value in sorted(self.items()):
            if value == '':
                continue
            if attrs and key not in attrs:
                continue
            name = self._reserved[key]
            if key == 'expires' and isinstance(value, int):
                parts.append(f'{name}={_getdate(value)}')
            elif key == 'max-age' and isinstance(value, int):
                parts.append('%s=%d' % (name, value))
            elif key == 'comment' and isinstance(value, str):
                parts.append(f'{name}={_quote(value)}')
            elif key in self._flags:
                if value:
                    parts.append(str(name))
            else:
                parts.append(f'{name}={value}')
        return '; '.join(parts)

    __class_getitem__ = classmethod(types.GenericAlias)


# --------------------------------------------------------------------------
# parsing of Cookie: header values

_KEY_CHARS = frozenset("!#$%&'~_`><@,:/$*+-.^|)(?}{")
_VALUE_EXTRA = frozenset('[]=')

_DATE_RE = re.compile(
    r'[A-Za-z]{3,9},\s\d{1,2}[\- ][A-Za-z]{3}[\- ]\d{2,4}\s'
    r'\d{1,2}:\d{1,2}:\d{1,2}\s[A-Za-z]{2,4}')
_QUOTED_RE = re.compile(r'"(?:[^\\"]|\\.)*"', re.DOTALL)


def _is_key_char(ch):
    return ch in _KEY_CHARS or ch.isalnum() or ch == '_'


def _is_value_char(ch):
    return _is_key_char(ch) or ch in _VALUE_EXTRA


def _tokenize(text):
    """Yield (key, value-or-None) pairs from a Cookie header value; stop
    at the first thing that is not a key."""
    n = len(text)
    i = 0
    while True:
        while i < n and (text[i].isspace() or text[i] == ';'):
            i += 1
        if i >= n:
            return
        start = i
        while i < n and _is_key_char(text[i]):
            i += 1
        if i == start:
            return
        key = text[start:i]
        j = i
        while j < n and text[j].isspace():
            j += 1
        value = None
        if j < n and text[j] == '=':
            j += 1
            while j < n and text[j].isspace():
                j += 1
            if j < n and text[j] == '"':
                match = _QUOTED_RE.match(text, j)
                if match:
                    value = match.group()
                    j = match.end()
                else:
                    return
            else:
                match = _DATE_RE.match(text, j)
                if match:
                    value = match.group()
                    j = match.end()
                else:
                    start = j
                    while j < n and _is_value_char(text[j]):
                        j += 1
                    value = text[start:j]
            i = j
        yield key, value


class BaseCookie(dict):
    """A dictionary of Morsels with the (de)coding of values left to the
    subclass (see value_decode/value_encode)."""

    def value_decode(self, val):
        """Return (real_value, coded_value) for a value read from a header."""
        return val, val

    def value_encode(self, val):
        """Return (real_value, coded_value) for a value set by the user."""
        text = str(val)
        return text, text

    def __init__(self, input=None):
        if input:
            self.load(input)

    def __set(self, key, real_value, coded_value):
        morsel = self.get(key, Morsel())
        morsel.set(key, real_value, coded_value)
        dict.__setitem__(self, key, morsel)

    def __setitem__(self, key, value):
        if isinstance(value, Morsel):
            dict.__setitem__(self, key, value)
        else:
            real, coded = self.value_encode(value)
            self.__set(key, real, coded)

    def output(self, attrs=None, header='Set-Cookie:', sep='\r\n'):
        """Set-Cookie header lines for all cookies, joined by *sep*."""
        return sep.join(morsel.output(attrs, header)
                        for key, morsel in sorted(self.items()))

    __str__ = output

    def __repr__(self):
        items = ' '.join(f'{key}={morsel.value!r}'
                         for key, morsel in sorted(self.items()))
        return f'<{self.__class__.__name__}: {items}>'

    def js_output(self, attrs=None):
        import warnings
        warnings.warn('BaseCookie.js_output is deprecated and slated for '
                      'removal in Python 3.19', DeprecationWarning,
                      stacklevel=2)
        return ''.join(morsel._js_output(attrs)
                       for key, morsel in sorted(self.items()))

    def load(self, rawdata):
        """Load cookies from a Cookie header value (str) or a mapping."""
        if isinstance(rawdata, str):
            self.__parse_string(rawdata)
        else:
            for key, value in rawdata.items():
                self[key] = value

    def __parse_string(self, text):
        # Everything is parsed first; an invalid header changes nothing.
        parsed = []
        seen_cookie = False
        for key, value in _tokenize(text):
            lower = key.lower()
            if key[0] == '$':
                # "$Path" etc. (RFC 2109 attributes sent back by clients)
                if not seen_cookie:
                    continue
                parsed.append((False, key[1:], value))
            elif lower in Morsel._reserved:
                if not seen_cookie:
                    return
                if value is None:
                    if lower not in Morsel._flags:
                        return
                    parsed.append((False, key, True))
                else:
                    parsed.append((False, key, _unquote(value)))
            elif value is not None:
                parsed.append((True, key, self.value_decode(value)))
                seen_cookie = True
            else:
                return
        morsel = None
        for is_cookie, key, value in parsed:
            if is_cookie:
                real, coded = value
                self.__set(key, real, coded)
                morsel = self[key]
            else:
                morsel[key] = value


class SimpleCookie(BaseCookie):
    """Cookies whose values are str, quoted as needed."""

    def value_decode(self, val):
        return _unquote(val), val

    def value_encode(self, val):
        text = str(val)
        return text, _quote(text)
