"""Miscellaneous helpers: addresses, dates, message ids, RFC 2231 params."""

__all__ = [
    'collapse_rfc2231_value',
    'decode_params',
    'decode_rfc2231',
    'encode_rfc2231',
    'formataddr',
    'formatdate',
    'format_datetime',
    'getaddresses',
    'make_msgid',
    'mktime_tz',
    'parseaddr',
    'parsedate',
    'parsedate_tz',
    'parsedate_to_datetime',
    'quote',
    'unquote',
    'localtime',
]

import os
import re
import time

from email._parseaddr import quote
from email._parseaddr import AddressList as _AddressList
from email._parseaddr import mktime_tz
from email._parseaddr import parsedate, parsedate_tz, _parsedate_tz
from email.charset import Charset

COMMASPACE = ', '
EMPTYSTRING = ''
UEMPTYSTRING = ''
CRLF = '\r\n'
TICK = "'"

specialsre = re.compile(r'[][\\()<>@,:;".]')
escapesre = re.compile(r'[\\"]')


def _has_surrogates(s):
    """True if *s* contains surrogate escapes (undecodable bytes)."""
    try:
        s.encode()
        return False
    except UnicodeEncodeError:
        return True


def _sanitize(string):
    # Turn surrogate escapes into U+FFFD (after a utf-8 round trip).
    original_bytes = string.encode('utf-8', 'surrogateescape')
    return original_bytes.decode('utf-8', 'replace')


# -- addresses --------------------------------------------------------------

def formataddr(pair, charset='utf-8'):
    """Format a (realname, email_address) pair for an address header.

    Non-ASCII real names are RFC 2047 encoded with *charset* (a name or
    any object with a header_encode() method); names with special
    characters are quoted.
    """
    name, address = pair
    address.encode('ascii')
    if not name:
        return address
    try:
        name.encode('ascii')
    except UnicodeEncodeError:
        if isinstance(charset, str):
            charset = Charset(charset)
        return "%s <%s>" % (charset.header_encode(name), address)
    quotes = '"' if specialsre.search(name) else ''
    name = escapesre.sub(r'\\\g<0>', name)
    return '%s%s%s <%s>' % (quotes, name, quotes, address)


def _iter_escaped_chars(addr):
    """Yield (index, char) pairs, a backslash pair counting as one item
    reported at the index of the escaped character."""
    pos = 0
    n = len(addr)
    while pos < n:
        ch = addr[pos]
        if ch == '\\' and pos + 1 < n:
            yield pos + 1, addr[pos:pos + 2]
            pos += 2
        else:
            yield pos, ch
            pos += 1


def _strip_quoted_realnames(addr):
    """Remove complete double-quoted strings from *addr*."""
    if '"' not in addr:
        return addr
    out = []
    start = 0
    open_at = None
    for pos, ch in _iter_escaped_chars(addr):
        if ch == '"':
            if open_at is None:
                open_at = pos
            else:
                out.append(addr[start:open_at])
                start = pos + 1
                open_at = None
    out.append(addr[start:])
    return EMPTYSTRING.join(out)


def _check_parenthesis(addr):
    # Comments outside quoted strings must be balanced.
    depth = 0
    for pos, ch in _iter_escaped_chars(_strip_quoted_realnames(addr)):
        if ch == '(':
            depth += 1
        elif ch == ')':
            depth -= 1
            if depth < 0:
                return False
    return depth == 0


def _pre_parse_validation(fields):
    return [v if _check_parenthesis(v) else "('', '')" for v in fields]


def _post_parse_validation(pairs):
    # A '[' left in an address means a domain literal failed to parse.
    return [('', '') if '[' in pair[1] else pair for pair in pairs]


supports_strict_parsing = True


def getaddresses(fieldvalues, *, strict=True):
    """Return a list of (realname, address) pairs for the given header
    values.  In strict mode, malformed input gives [('', '')]."""
    if not strict:
        combined = COMMASPACE.join(str(v) for v in fieldvalues)
        return _AddressList(combined).addresslist
    values = _pre_parse_validation([str(v) for v in fieldvalues])
    pairs = _post_parse_validation(
        _AddressList(COMMASPACE.join(values)).addresslist)
    expected = sum(1 + _strip_quoted_realnames(v).count(',') for v in values)
    if len(pairs) != expected:
        return [('', '')]
    return pairs


def parseaddr(addr, *, strict=True):
    """Parse one address into (realname, email_address); ('', '') if it
    cannot be parsed."""
    if not strict:
        pairs = _AddressList(addr).addresslist
        return pairs[0] if pairs else ('', '')
    if isinstance(addr, list):
        addr = addr[0]
    if not isinstance(addr, str):
        return ('', '')
    addr = _pre_parse_validation([addr])[0]
    pairs = _post_parse_validation(_AddressList(addr).addresslist)
    if len(pairs) != 1:
        return ('', '')
    return pairs[0]


def unquote(str):
    """Remove one level of double quotes (or angle brackets)."""
    if len(str) > 1:
        if str.startswith('"') and str.endswith('"'):
            return str[1:-1].replace('\\\\', '\\').replace('\\"', '"')
        if str.startswith('<') and str.endswith('>'):
            return str[1:-1]
    return str


# -- dates --------------------------------------------------------------------

_DAYNAMES = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun']
_MONTHNAMES = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug',
               'Sep', 'Oct', 'Nov', 'Dec']


def _format_timetuple_and_zone(timetuple, zone):
    return '%s, %02d %s %04d %02d:%02d:%02d %s' % (
        _DAYNAMES[timetuple[6]], timetuple[2], _MONTHNAMES[timetuple[1] - 1],
        timetuple[0], timetuple[3], timetuple[4], timetuple[5], zone)


def formatdate(timeval=None, localtime=False, usegmt=False):
    """Format a POSIX timestamp as an RFC 2822 date.

    With *localtime* the local time zone is used; otherwise UTC, written as
    'GMT' if *usegmt* else '-0000'.
    """
    if timeval is None:
        timeval = time.time()
    if localtime:
        t = time.localtime(timeval)
        offset = t.tm_gmtoff
        if offset is None:
            offset = -(time.altzone if t.tm_isdst > 0 else time.timezone)
        sign = '-' if offset < 0 else '+'
        hours, minutes = divmod(abs(offset) // 60, 60)
        zone = '%s%02d%02d' % (sign, hours, minutes)
    else:
        t = time.gmtime(timeval)
        zone = 'GMT' if usegmt else '-0000'
    return _format_timetuple_and_zone(t, zone)


def format_datetime(dt, usegmt=False):
    """Format a datetime as an RFC 2822 date.  A naive datetime is taken to
    be UTC with no zone information ('-0000')."""
    now = dt.timetuple()
    if usegmt:
        import datetime
        if dt.tzinfo is None or dt.tzinfo != datetime.timezone.utc:
            raise ValueError("usegmt option requires a UTC datetime")
        zone = 'GMT'
    elif dt.tzinfo is None:
        zone = '-0000'
    else:
        zone = dt.strftime("%z")
    return _format_timetuple_and_zone(now, zone)


def parsedate_to_datetime(data):
    """Parse an RFC 2822 date into a datetime (naive if the zone is
    '-0000')."""
    import datetime
    parsed = _parsedate_tz(data)
    if parsed is None:
        raise ValueError('Invalid date value or format "%s"' % str(data))
    *fields, offset = parsed
    if offset is None:
        return datetime.datetime(*fields[:6])
    return datetime.datetime(*fields[:6], tzinfo=datetime.timezone(
        datetime.timedelta(seconds=offset)))


def localtime(dt=None, isdst=None):
    """Return *dt* (default: now) as an aware datetime in the local zone."""
    import datetime
    if isdst is not None:
        import warnings
        warnings._deprecated(
            "The 'isdst' parameter to 'localtime'",
            message='{name} is deprecated and slated for removal in Python {remove}',
            remove=(3, 14))
    if dt is None:
        dt = datetime.datetime.now()
    return dt.astimezone()


# -- message ids -----------------------------------------------------------

def make_msgid(idstring=None, domain=None):
    """Return a unique Message-ID like '<1428.6.2847@host.example>'."""
    import random
    timeval = int(time.time() * 100)
    pid = os.getpid()
    randint = random.getrandbits(64)
    idstring = '' if idstring is None else '.' + idstring
    if domain is None:
        import socket
        domain = socket.getfqdn()
    return '<%d.%d.%d%s@%s>' % (timeval, pid, randint, idstring, domain)


# -- RFC 2231 -------------------------------------------------------------------

_UNRESERVED = frozenset(b'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz'
                        b'0123456789_.-~')


def _percent_encode(s, encoding, errors='strict'):
    return ''.join(chr(b) if b in _UNRESERVED else '%%%02X' % b
                   for b in s.encode(encoding, errors))


def _percent_decode_to_bytes(s):
    # Like urllib.parse.unquote_to_bytes (str input is utf-8 encoded first,
    # so surrogates raise UnicodeEncodeError).
    raw = s.encode('utf-8')
    if b'%' not in raw:
        return raw
    out = bytearray()
    pos = 0
    n = len(raw)
    while pos < n:
        if raw[pos] == 0x25 and _ishex(raw[pos + 1:pos + 3].decode('latin-1')):
            out.append(int(raw[pos + 1:pos + 3], 16))
            pos += 3
        else:
            out.append(raw[pos])
            pos += 1
    return bytes(out)


def _percent_decode(s, encoding):
    if '%' not in s:
        return s
    out = []
    raw = bytearray()
    pos = 0
    n = len(s)
    while pos < n:
        ch = s[pos]
        if ch == '%' and _ishex(s[pos + 1:pos + 3]):
            raw.append(int(s[pos + 1:pos + 3], 16))
            pos += 3
            continue
        if raw:
            out.append(raw.decode(encoding, 'replace'))
            raw = bytearray()
        out.append(ch)
        pos += 1
    if raw:
        out.append(raw.decode(encoding, 'replace'))
    return ''.join(out)


def _ishex(text):
    return len(text) == 2 and all(c in '0123456789abcdefABCDEF' for c in text)


def decode_rfc2231(s):
    """Split an RFC 2231 extended value into (charset, language, value)."""
    parts = s.split(TICK, 2)
    if len(parts) <= 2:
        return None, None, s
    return parts


def encode_rfc2231(s, charset=None, language=None):
    """Percent-encode *s* for an RFC 2231 parameter value, with charset and
    language prefix when either is given."""
    s = _percent_encode(s, charset or 'ascii')
    if charset is None and language is None:
        return s
    if language is None:
        language = ''
    return "%s'%s'%s" % (charset, language, s)


rfc2231_continuation = re.compile(r'^(?P<name>\w+)\*((?P<num>[0-9]+)\*?)?$',
                                  re.ASCII)


def decode_params(params):
    """Decode a list of (name, value) parameters, joining RFC 2231
    continuations.  The first pair (the main value) is kept as is."""
    result = [params[0]]
    pieces = {}
    for name, value in params[1:]:
        extended = name.endswith('*')
        value = unquote(value)
        m = rfc2231_continuation.match(name)
        if m:
            name, num = m.group('name', 'num')
            if num is not None:
                num = int(num)
            pieces.setdefault(name, []).append((num, value, extended))
        else:
            result.append((name, '"%s"' % quote(value)))
    for name, parts in pieces.items():
        parts.sort(key=lambda p: (p[0] is not None, p[0] or 0, p[1], p[2]))
        texts = []
        any_extended = False
        for num, text, extended in parts:
            if extended:
                text = _percent_decode(text, 'latin-1')
                any_extended = True
            texts.append(text)
        value = quote(EMPTYSTRING.join(texts))
        if any_extended:
            charset, language, value = decode_rfc2231(value)
            result.append((name, (charset, language, '"%s"' % value)))
        else:
            result.append((name, '"%s"' % value))
    return result


def collapse_rfc2231_value(value, errors='replace',
                           fallback_charset='us-ascii'):
    """Turn a (charset, language, value) triple into a str; plain values
    are just unquoted."""
    if not isinstance(value, tuple) or len(value) != 3:
        return unquote(value)
    charset, language, text = value
    if charset is None:
        charset = fallback_charset
    raw = bytes(text, 'raw-unicode-escape')
    try:
        return str(raw, charset, errors)
    except LookupError:
        return unquote(text)
