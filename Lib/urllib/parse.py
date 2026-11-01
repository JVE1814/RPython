"""Split, join, quote and unquote URLs (RFC 3986 and friends).

The main entry points are urlsplit()/urlunsplit(), urlparse()/urlunparse(),
urljoin(), urldefrag(), the quoting helpers quote()/unquote() and their
"plus" variants, and the query-string helpers parse_qs()/parse_qsl() and
urlencode().  Every function accepts either str or bytes (ASCII) arguments;
the result has the type of the input.
"""

import functools
import re
import types
import warnings

__all__ = ["urlparse", "urlunparse", "urljoin", "urldefrag",
           "urlsplit", "urlunsplit", "urlencode", "parse_qs",
           "parse_qsl", "quote", "quote_plus", "quote_from_bytes",
           "unquote", "unquote_plus", "unquote_to_bytes",
           "DefragResult", "ParseResult", "SplitResult",
           "DefragResultBytes", "ParseResultBytes", "SplitResultBytes"]

# Schemes for which relative references are resolved by urljoin().
uses_relative = ['', 'ftp', 'http', 'gopher', 'nntp', 'imap',
                 'wais', 'file', 'https', 'shttp', 'mms',
                 'prospero', 'rtsp', 'rtsps', 'rtspu', 'sftp',
                 'svn', 'svn+ssh', 'ws', 'wss']

# Schemes whose URLs carry a network location ("//host").
uses_netloc = ['', 'ftp', 'http', 'gopher', 'nntp', 'telnet',
               'imap', 'wais', 'file', 'mms', 'https', 'shttp',
               'snews', 'prospero', 'rtsp', 'rtsps', 'rtspu', 'rsync',
               'svn', 'svn+ssh', 'sftp', 'nfs', 'git', 'git+ssh',
               'ws', 'wss', 'itms-services']

# Schemes that allow ";params" on the last path segment.
uses_params = ['', 'ftp', 'hdl', 'prospero', 'http', 'imap',
               'https', 'shttp', 'rtsp', 'rtsps', 'rtspu', 'sip',
               'sips', 'mms', 'sftp', 'tel']

# Kept for backwards compatibility; not used by this module.
non_hierarchical = ['gopher', 'hdl', 'mailto', 'news',
                    'telnet', 'wais', 'imap', 'snews', 'sip', 'sips']
uses_query = ['', 'http', 'wais', 'imap', 'https', 'shttp', 'mms',
              'gopher', 'rtsp', 'rtsps', 'rtspu', 'sip', 'sips']
uses_fragment = ['', 'ftp', 'hdl', 'http', 'gopher', 'news',
                 'nntp', 'wais', 'https', 'shttp', 'snews',
                 'file', 'prospero']

scheme_chars = ('abcdefghijklmnopqrstuvwxyz'
                'ABCDEFGHIJKLMNOPQRSTUVWXYZ'
                '0123456789'
                '+-.')

# Leading characters stripped from URLs (C0 controls and space, per WHATWG).
_WHATWG_C0_CONTROL_OR_SPACE = ''.join(map(chr, range(0x21)))
# Characters removed from anywhere in a URL.
_UNSAFE_URL_BYTES_TO_REMOVE = ['\t', '\r', '\n']

_UNSET = object()


def clear_cache():
    """Drop the internal parsing and quoting caches."""
    _split_cached.cache_clear()
    _byte_quoter_factory.cache_clear()


def __getattr__(name):
    if name == 'Quoter':
        warnings.warn('urllib.parse.Quoter will be removed in Python 3.14. '
                      'It was not intended to be a public API.',
                      DeprecationWarning, stacklevel=2)
        return _Quoter
    raise AttributeError(f'module {__name__!r} has no attribute {name!r}')


# --------------------------------------------------------------------------
# str/bytes coercion

def _noop(obj):
    return obj


def _encode_result(obj, encoding='ascii', errors='strict'):
    return obj.encode(encoding, errors)


def _decode_args(args, encoding='ascii', errors='strict'):
    return tuple(x.decode(encoding, errors) if x else '' for x in args)


def _coerce_args(*args):
    """Return the arguments as str plus a function converting results back.

    Empty arguments (including None) may be of either type; all non-empty
    arguments must agree.
    """
    first_is_str = isinstance(args[0], str)
    for arg in args[1:]:
        if arg and isinstance(arg, str) != first_is_str:
            raise TypeError('Cannot mix str and non-str arguments')
    if first_is_str:
        return args + (_noop,)
    out = []
    for arg in args:
        if arg:
            out.append(arg.decode('ascii', 'strict'))
        else:
            out.append('')
    return tuple(out) + (_encode_result,)


def _decode_opt(value, encoding='ascii', errors='strict'):
    return None if value is None else value.decode(encoding, errors)


def _encode_opt(value, encoding='ascii', errors='strict'):
    return None if value is None else value.encode(encoding, errors)


# --------------------------------------------------------------------------
# result types

class _ResultMixinStr:
    """Results whose fields are str; encode() gives the bytes twin."""
    __slots__ = ()

    def encode(self, encoding='ascii', errors='strict'):
        result = self._encoded_counterpart(
            *(_encode_opt(x, encoding, errors) for x in self))
        return _copy_keep_empty(self, result)


class _ResultMixinBytes:
    """Results whose fields are bytes; decode() gives the str twin."""
    __slots__ = ()

    def decode(self, encoding='ascii', errors='strict'):
        result = self._decoded_counterpart(
            *(_decode_opt(x, encoding, errors) for x in self))
        return _copy_keep_empty(self, result)


def _copy_keep_empty(src, dst):
    if src._keep_empty:
        dst._keep_empty = True
    return dst


class _KeepEmpty:
    """State shared by all results: whether empty components are kept.

    Results from parsing with missing_as_none=True use None for missing
    components and '' for empty ones; recombining them keeps the empty
    delimiters ('?', '#', '//').
    """
    _keep_empty = False

    def __getstate__(self):
        if self._keep_empty:
            return (None, {'_keep_empty': True})
        return None

    def _replace(self, /, **changes):
        new = super()._replace(**changes)
        return _copy_keep_empty(self, new)

    def __replace__(self, /, **changes):
        return self._replace(**changes)


class _NetlocResultMixinBase:
    """Accessors for the pieces of the netloc component."""
    __slots__ = ()

    @property
    def username(self):
        return self._userinfo[0]

    @property
    def password(self):
        return self._userinfo[1]

    @property
    def hostname(self):
        host = self._hostinfo[0]
        if not host:
            return None
        sep = '%' if isinstance(host, str) else b'%'
        addr, pct, zone = host.partition(sep)
        return addr.lower() + pct + zone

    @property
    def port(self):
        text = self._hostinfo[1]
        if text is None:
            return None
        if not (text.isdigit() and text.isascii()):
            raise ValueError(
                f'Port could not be cast to integer value as {text!r}')
        value = int(text)
        if not 0 <= value <= 65535:
            raise ValueError('Port out of range 0-65535')
        return value

    __class_getitem__ = classmethod(types.GenericAlias)


class _NetlocResultMixinStr(_NetlocResultMixinBase, _ResultMixinStr):
    __slots__ = ()

    @property
    def _userinfo(self):
        netloc = self.netloc
        if not netloc:
            return None, None
        userinfo, at, _ = netloc.rpartition('@')
        if not at:
            return None, None
        user, colon, password = userinfo.partition(':')
        return user, (password if colon else None)

    @property
    def _hostinfo(self):
        netloc = self.netloc
        if not netloc:
            return None, None
        hostport = netloc.rpartition('@')[2]
        _, bracket, rest = hostport.partition('[')
        if bracket:
            host, _, tail = rest.partition(']')
            port = tail.partition(':')[2]
        else:
            host, _, port = hostport.partition(':')
        return host, (port or None)


class _NetlocResultMixinBytes(_NetlocResultMixinBase, _ResultMixinBytes):
    __slots__ = ()

    @property
    def _userinfo(self):
        netloc = self.netloc
        if not netloc:
            return None, None
        userinfo, at, _ = netloc.rpartition(b'@')
        if not at:
            return None, None
        user, colon, password = userinfo.partition(b':')
        return user, (password if colon else None)

    @property
    def _hostinfo(self):
        netloc = self.netloc
        if not netloc:
            return None, None
        hostport = netloc.rpartition(b'@')[2]
        _, bracket, rest = hostport.partition(b'[')
        if bracket:
            host, _, tail = rest.partition(b']')
            port = tail.partition(b':')[2]
        else:
            host, _, port = hostport.partition(b':')
        return host, (port or None)


from collections import namedtuple

_DefragResultBase = namedtuple('_DefragResultBase', 'url fragment')
_SplitResultBase = namedtuple(
    '_SplitResultBase', 'scheme netloc path query fragment')
_ParseResultBase = namedtuple(
    '_ParseResultBase', 'scheme netloc path params query fragment')

_DefragResultBase.__doc__ = 'A URL with its fragment split off.'
_DefragResultBase.url.__doc__ = 'The URL without the fragment.'
_DefragResultBase.fragment.__doc__ = 'The fragment identifier, or None/empty.'
_SplitResultBase.__doc__ = 'The five components returned by urlsplit().'
_SplitResultBase.scheme.__doc__ = 'The scheme (lowercased), e.g. "https"'
_SplitResultBase.netloc.__doc__ = 'The authority: [user[:password]@]host[:port]'
_SplitResultBase.path.__doc__ = 'The path'
_SplitResultBase.query.__doc__ = 'The query string (after "?")'
_SplitResultBase.fragment.__doc__ = 'The fragment (after "#")'
_ParseResultBase.__doc__ = 'The six components returned by urlparse().'
_ParseResultBase.scheme.__doc__ = _SplitResultBase.scheme.__doc__
_ParseResultBase.netloc.__doc__ = _SplitResultBase.netloc.__doc__
_ParseResultBase.path.__doc__ = _SplitResultBase.path.__doc__
_ParseResultBase.params.__doc__ = 'The ";" parameters of the last path segment'
_ParseResultBase.query.__doc__ = _SplitResultBase.query.__doc__
_ParseResultBase.fragment.__doc__ = _SplitResultBase.fragment.__doc__


def _defrag_geturl(self):
    frag = self.fragment
    if frag or (self._keep_empty and frag is not None):
        sep = '#' if isinstance(self.url, str) else b'#'
        return self.url + sep + frag
    return self.url


class DefragResult(_KeepEmpty, _DefragResultBase, _ResultMixinStr):
    # no __slots__: instances need a __dict__ for the _keep_empty flag
    geturl = _defrag_geturl


class SplitResult(_KeepEmpty, _SplitResultBase, _NetlocResultMixinStr):

    def geturl(self):
        return urlunsplit(self)


class ParseResult(_KeepEmpty, _ParseResultBase, _NetlocResultMixinStr):

    def geturl(self):
        return urlunparse(self)


class DefragResultBytes(_KeepEmpty, _DefragResultBase, _ResultMixinBytes):
    geturl = _defrag_geturl


class SplitResultBytes(_KeepEmpty, _SplitResultBase, _NetlocResultMixinBytes):

    def geturl(self):
        return urlunsplit(self)


class ParseResultBytes(_KeepEmpty, _ParseResultBase, _NetlocResultMixinBytes):

    def geturl(self):
        return urlunparse(self)


for _s, _b in ((DefragResult, DefragResultBytes),
               (SplitResult, SplitResultBytes),
               (ParseResult, ParseResultBytes)):
    _s._encoded_counterpart = _b
    _b._decoded_counterpart = _s
del _s, _b


def _make_result(cls, values, keep_empty):
    result = cls(*values)
    if keep_empty:
        result._keep_empty = True
    return result


# --------------------------------------------------------------------------
# splitting

def _clean_url(url):
    url = url.lstrip(_WHATWG_C0_CONTROL_OR_SPACE)
    for ch in _UNSAFE_URL_BYTES_TO_REMOVE:
        if ch in url:
            url = url.replace(ch, '')
    return url


def _clean_scheme(scheme):
    scheme = scheme.strip(_WHATWG_C0_CONTROL_OR_SPACE)
    for ch in _UNSAFE_URL_BYTES_TO_REMOVE:
        scheme = scheme.replace(ch, '')
    return scheme


def _splitnetloc(url, start=0):
    end = len(url)
    for delim in '/?#':
        pos = url.find(delim, start)
        if 0 <= pos < end:
            end = pos
    return url[start:end], url[end:]


def _checknetloc(netloc):
    if not netloc or netloc.isascii():
        return
    import unicodedata
    stripped = netloc
    for ch in '@:#?':
        stripped = stripped.replace(ch, '')
    normalized = unicodedata.normalize('NFKC', stripped)
    if normalized == stripped:
        return
    for ch in '/?#@:':
        if ch in normalized:
            raise ValueError(f"netloc '{netloc}' contains invalid "
                             f"characters under NFKC normalization")


_IPVFUTURE = re.compile(r'\A[vV][a-fA-F0-9]+\..+\Z', re.DOTALL)


def _check_bracketed_host(host):
    if host[:1] in ('v', 'V'):
        if not _IPVFUTURE.match(host):
            raise ValueError('IPvFuture address is invalid')
        return
    import ipaddress
    addr = ipaddress.ip_address(host)       # ValueError if not an address
    if isinstance(addr, ipaddress.IPv4Address):
        raise ValueError('An IPv4 address cannot be in brackets')


def _check_bracketed_netloc(netloc):
    hostport = netloc.rpartition('@')[2]
    before, bracket, rest = hostport.partition('[')
    if not bracket:
        return
    if before:
        raise ValueError('Invalid IPv6 URL')
    host, _, after = rest.partition(']')
    if after and not after.startswith(':'):
        raise ValueError('Invalid IPv6 URL')
    _check_bracketed_host(host)


def _split_scheme(url):
    colon = url.find(':')
    if colon > 0 and url[0].isascii() and url[0].isalpha():
        candidate = url[:colon]
        for ch in candidate:
            if ch not in scheme_chars:
                return None, url
        return candidate.lower(), url[colon + 1:]
    return None, url


@functools.lru_cache(typed=True)
def _split_cached(url, allow_fragments):
    """Split a cleaned str URL; missing components are None."""
    scheme, rest = _split_scheme(url)
    netloc = query = fragment = None
    if rest[:2] == '//':
        netloc, rest = _splitnetloc(rest, 2)
        if (('[' in netloc and ']' not in netloc) or
                (']' in netloc and '[' not in netloc)):
            raise ValueError('Invalid IPv6 URL')
        if '[' in netloc and ']' in netloc:
            _check_bracketed_netloc(netloc)
    if allow_fragments and '#' in rest:
        rest, fragment = rest.split('#', 1)
    if '?' in rest:
        rest, query = rest.split('?', 1)
    _checknetloc(netloc)
    return scheme, netloc, rest, query, fragment


def _split_impl(url, scheme, allow_fragments):
    """Split *url* (str); returns 5 components with None for missing."""
    url = _clean_url(url)
    parts = _split_cached(url, bool(allow_fragments))
    if parts[0] is None and scheme is not None:
        parts = (_clean_scheme(scheme),) + parts[1:]
    return parts


def urlsplit(url, scheme=None, allow_fragments=True, *, missing_as_none=False):
    """Split a URL into (scheme, netloc, path, query, fragment).

    The parameters (';...') of the last path segment are left in the path.
    With missing_as_none=True, components that do not occur in the URL are
    None, otherwise they are empty strings.  *scheme* is the default scheme
    used when the URL has none.
    """
    url, scheme_arg, coerce = _coerce_args(url, scheme)
    scheme = None if scheme is None else scheme_arg
    parts = _split_impl(url, scheme, allow_fragments)
    if not missing_as_none:
        parts = tuple('' if p is None else p for p in parts)
    if coerce is _noop:
        return _make_result(SplitResult, parts, missing_as_none)
    return _make_result(SplitResultBytes,
                        (_encode_opt(p) for p in parts), missing_as_none)


def _splitparams(path):
    slash = path.rfind('/')
    if slash >= 0:
        semi = path.find(';', slash)
        if semi < 0:
            return path, None
    else:
        semi = path.find(';')
    return path[:semi], path[semi + 1:]


def urlparse(url, scheme=None, allow_fragments=True, *, missing_as_none=False):
    """Split a URL into (scheme, netloc, path, params, query, fragment).

    Like urlsplit(), but the ';parameters' of the last path segment are
    returned separately for schemes that use them.
    """
    url, scheme_arg, coerce = _coerce_args(url, scheme)
    scheme = None if scheme is None else scheme_arg
    s, netloc, path, query, fragment = _split_impl(url, scheme,
                                                   allow_fragments)
    params = None
    if (s or '') in uses_params and ';' in path:
        path, params = _splitparams(path)
    parts = (s, netloc, path, params, query, fragment)
    if not missing_as_none:
        parts = tuple('' if p is None else p for p in parts)
    if coerce is _noop:
        return _make_result(ParseResult, parts, missing_as_none)
    return _make_result(ParseResultBytes,
                        (_encode_opt(p) for p in parts), missing_as_none)


# --------------------------------------------------------------------------
# joining components back together

def _keep_empty_for(components, keep_empty):
    flag = getattr(components, '_keep_empty', None)
    if keep_empty is _UNSET:
        return bool(flag)
    if keep_empty and flag is False and isinstance(components, _KeepEmpty):
        raise ValueError('Cannot distinguish between empty and not defined '
                         'URI components in the result of parsing URL with '
                         'missing_as_none=False')
    return bool(keep_empty)


def _coerce_components(components):
    items = tuple(components)
    first = None
    for item in items:
        if item:
            first = item
            break
    if first is None:
        first = next((x for x in items if x is not None), "")
    if isinstance(first, str):
        for item in items:
            if item and not isinstance(item, str):
                raise TypeError('Cannot mix str and non-str arguments')
        return items, _noop
    for item in items:
        if item and isinstance(item, str):
            raise TypeError('Cannot mix str and non-str arguments')
    return tuple(None if x is None else x.decode('ascii') for x in items), \
        _encode_result


def _unsplit(scheme, netloc, url, query, fragment, keep_empty):
    if keep_empty:
        present = lambda x: x is not None
    else:
        present = bool
    url = url or ''
    if present(netloc):
        if url and url[:1] != '/':
            url = '/' + url
        url = '//' + netloc + url
    elif url[:2] == '//':
        url = '//' + url
    elif (not keep_empty and scheme and scheme in uses_netloc and
          (not url or url[:1] == '/')):
        url = '//' + url
    if scheme:
        url = scheme + ':' + url
    if present(query):
        url = url + '?' + query
    if present(fragment):
        url = url + '#' + fragment
    return url


def urlunsplit(components, *, keep_empty=_UNSET):
    """Combine the five components of urlsplit() into a URL string.

    With keep_empty=True empty components ('' rather than None) keep their
    delimiters, so results of urlsplit(..., missing_as_none=True) round-trip
    exactly.  By default results remember how they were parsed.
    """
    keep = _keep_empty_for(components, keep_empty)
    (scheme, netloc, url, query, fragment), coerce = \
        _coerce_components(components)
    return coerce(_unsplit(scheme, netloc, url, query, fragment, keep))


def urlunparse(components, *, keep_empty=_UNSET):
    """Combine the six components of urlparse() into a URL string."""
    keep = _keep_empty_for(components, keep_empty)
    (scheme, netloc, url, params, query, fragment), coerce = \
        _coerce_components(components)
    url = url or ''
    if params or (keep and params is not None):
        url = url + ';' + params
    return coerce(_unsplit(scheme, netloc, url, query, fragment, keep))


def _remove_dot_segments(path):
    segments = path.split('/')
    out = []
    last = len(segments) - 1
    for i, seg in enumerate(segments):
        if seg == '.':
            if i == last:
                out.append('')
            continue
        if seg == '..':
            if out and not (len(out) == 1 and out[0] == ''):
                out.pop()
            if i == last:
                out.append('')
            continue
        out.append(seg)
    if path.startswith('/') and (not out or out[0] != ''):
        out.insert(0, '')
    result = '/'.join(out)
    if path.startswith('/') and not result:
        result = '/'
    return result


def urljoin(base, url, allow_fragments=True):
    """Resolve *url* relative to the absolute URL *base*."""
    if not base:
        return url
    if not url:
        return base
    base, url, coerce = _coerce_args(base, url)
    bscheme, bnetloc, bpath, bquery, bfragment = _split_impl(
        base, None, allow_fragments)
    scheme, netloc, path, query, fragment = _split_impl(
        url, None, allow_fragments)
    if scheme is None:
        scheme = bscheme
    if (scheme or '') != (bscheme or '') or (scheme or '') not in uses_relative:
        return coerce(url)
    if (scheme or '') in uses_netloc:
        if netloc:
            return coerce(_unsplit(scheme, netloc, path, query, fragment,
                                   True))
        netloc = bnetloc

    if not path:
        if query is None and fragment is None:
            return coerce(base)
        path = bpath
        if query is None:
            query = bquery
        return coerce(_unsplit(scheme, netloc, path, query, fragment, True))

    if not path.startswith('/'):
        if bpath:
            directory = bpath[:bpath.rfind('/') + 1]
        else:
            directory = '/' if (netloc is not None or scheme) else ''
        path = directory + path
    path = _remove_dot_segments(path)
    return coerce(_unsplit(scheme, netloc, path, query, fragment, True))


def urldefrag(url, *, missing_as_none=False):
    """Split *url* into the URL without fragment and the fragment.

    The fragment is '' (or None with missing_as_none=True) if there is none.
    """
    url, coerce = _coerce_args(url)
    url = _clean_url(url)
    base, hash_, frag = url.partition('#')
    if not hash_:
        frag = None
    if not missing_as_none and frag is None:
        frag = ''
    if coerce is _noop:
        return _make_result(DefragResult, (base, frag), missing_as_none)
    return _make_result(DefragResultBytes,
                        (_encode_opt(base), _encode_opt(frag)),
                        missing_as_none)


# --------------------------------------------------------------------------
# percent-encoding

_HEXDIG = '0123456789ABCDEFabcdef'
_hextobyte = None


def _hex_table():
    global _hextobyte
    if _hextobyte is None:
        _hextobyte = {(a + b).encode(): bytes.fromhex(a + b)
                      for a in _HEXDIG for b in _HEXDIG}
    return _hextobyte


def unquote_to_bytes(string):
    """Replace %xx escapes by the bytes they stand for."""
    if not string:
        string.split        # TypeError/AttributeError for unsuitable types
        return b''
    if isinstance(string, str):
        string = string.encode('utf-8')
    pieces = string.split(b'%')
    if len(pieces) == 1:
        return string
    table = _hex_table()
    out = [pieces[0]]
    for piece in pieces[1:]:
        decoded = table.get(piece[:2])
        if decoded is None:
            out.append(b'%')
            out.append(piece)
        else:
            out.append(decoded)
            out.append(piece[2:])
    return b''.join(out)


_ASCII_RUN = re.compile('([\x00-\x7f]+)')


def unquote(string, encoding='utf-8', errors='replace'):
    """Replace %xx escapes by their characters.

    Runs of escapes are decoded with *encoding*/*errors* (UTF-8 and
    'replace' by default).  bytes input is accepted as well.
    """
    if isinstance(string, bytes):
        return unquote_to_bytes(string).decode(encoding or 'utf-8',
                                                errors or 'replace')
    if '%' not in string:
        string.split
        return string
    if encoding is None:
        encoding = 'utf-8'
    if errors is None:
        errors = 'replace'
    parts = _ASCII_RUN.split(string)
    out = [parts[0]]
    for i in range(1, len(parts), 2):
        out.append(unquote_to_bytes(parts[i]).decode(encoding, errors))
        out.append(parts[i + 1])
    return ''.join(out)


def unquote_plus(string, encoding='utf-8', errors='replace'):
    """Like unquote(), but also turn '+' into spaces (form encoding)."""
    string = string.replace('+', ' ')
    return unquote(string, encoding, errors)


_ALWAYS_SAFE = frozenset(b'ABCDEFGHIJKLMNOPQRSTUVWXYZ'
                         b'abcdefghijklmnopqrstuvwxyz'
                         b'0123456789'
                         b'_.-~')
_ALWAYS_SAFE_BYTES = bytes(_ALWAYS_SAFE)


class _Quoter(dict):
    """Lazy table mapping a byte value to its quoted text."""

    def __init__(self, safe):
        self.safe = _ALWAYS_SAFE.union(safe)

    def __repr__(self):
        return f'<Quoter {dict(self)!r}>'

    def __missing__(self, b):
        text = chr(b) if b in self.safe else '%{:02X}'.format(b)
        self[b] = text
        return text


@functools.lru_cache
def _byte_quoter_factory(safe):
    return _Quoter(safe).__getitem__


def quote(string, safe='/', encoding=None, errors=None):
    """Percent-encode *string* for use in a URL.

    Letters, digits and '_.-~' are never quoted; characters in *safe* are
    left alone too ('/' by default).  str input is encoded first with
    *encoding*/*errors* (UTF-8/'strict' by default).
    """
    if isinstance(string, str):
        if not string:
            return string
        if encoding is None:
            encoding = 'utf-8'
        if errors is None:
            errors = 'strict'
        string = string.encode(encoding, errors)
    else:
        if encoding is not None:
            raise TypeError("quote() doesn't support 'encoding' for bytes")
        if errors is not None:
            raise TypeError("quote() doesn't support 'errors' for bytes")
    return quote_from_bytes(string, safe)


def quote_plus(string, safe='', encoding=None, errors=None):
    """Like quote(), but encode spaces as '+' (form encoding)."""
    if ((isinstance(string, str) and ' ' not in string) or
            (isinstance(string, bytes) and b' ' not in string)):
        return quote(string, safe, encoding, errors)
    space = ' ' if isinstance(safe, str) else b' '
    return quote(string, safe + space, encoding, errors).replace(' ', '+')


def quote_from_bytes(bs, safe='/'):
    """Percent-encode a bytes object (like quote(), without encoding)."""
    if not isinstance(bs, (bytes, bytearray)):
        raise TypeError('quote_from_bytes() expected bytes')
    if not bs:
        return ''
    if isinstance(safe, str):
        safe = safe.encode('ascii', 'ignore')
    else:
        safe = bytes(c for c in safe if c < 128)
    if not bs.rstrip(_ALWAYS_SAFE_BYTES + safe):
        return bs.decode()
    quoter = _byte_quoter_factory(safe)
    return ''.join(map(quoter, bs))


def urlencode(query, doseq=False, safe='', encoding=None, errors=None,
              quote_via=quote_plus):
    """Encode a mapping or a sequence of pairs as a query string.

    With doseq true, sequence values produce one "key=value" item per
    element.  Keys and values are quoted with *quote_via*.
    """
    if hasattr(query, 'items'):
        query = query.items()
    else:
        try:
            if len(query) and not isinstance(query[0], tuple):
                raise TypeError
        except TypeError as exc:
            raise TypeError('not a valid non-string sequence '
                            'or mapping object') from exc

    def enc(value):
        if isinstance(value, bytes):
            return quote_via(value, safe)
        if not isinstance(value, str):
            value = str(value)
        return quote_via(value, safe, encoding, errors)

    items = []
    for key, value in query:
        k = enc(key)
        if not doseq or isinstance(value, (str, bytes)):
            items.append(k + '=' + enc(value))
            continue
        try:
            len(value)
        except TypeError:
            items.append(k + '=' + enc(value))
            continue
        for element in value:
            items.append(k + '=' + enc(element))
    return '&'.join(items)


# --------------------------------------------------------------------------
# query strings

def parse_qs(qs, keep_blank_values=False, strict_parsing=False,
             encoding='utf-8', errors='replace', max_num_fields=None,
             separator='&'):
    """Parse a query string into a dict mapping names to lists of values."""
    result = {}
    pairs = _parse_qsl(qs, keep_blank_values, strict_parsing, encoding,
                       errors, max_num_fields, separator, _stacklevel=3)
    for name, value in pairs:
        result.setdefault(name, []).append(value)
    return result


def parse_qsl(qs, keep_blank_values=False, strict_parsing=False,
              encoding='utf-8', errors='replace', max_num_fields=None,
              separator='&'):
    """Parse a query string into a list of (name, value) pairs.

    Blank values are dropped unless keep_blank_values is true; with
    strict_parsing errors raise ValueError.  bytes input gives bytes pairs.
    """
    return _parse_qsl(qs, keep_blank_values, strict_parsing, encoding,
                      errors, max_num_fields, separator, _stacklevel=3)


def _parse_qsl(qs, keep_blank_values, strict_parsing, encoding, errors,
               max_num_fields, separator, _stacklevel):
    if not separator or not isinstance(separator, (str, bytes)):
        raise ValueError('Separator must be of type string or bytes.')
    if isinstance(qs, str):
        if not isinstance(separator, str):
            separator = str(separator, 'ascii')
        equals, plus, space = '=', '+', ' '
    elif qs:
        if not isinstance(qs, (bytes, bytearray)):
            try:
                qs = memoryview(qs)
            except TypeError:
                raise TypeError(
                    f'a bytes-like object is required, not '
                    f'{type(qs).__name__!r}') from None
        qs = bytes(qs)
        if isinstance(separator, str):
            separator = bytes(separator, 'ascii')
        equals, plus, space = b'=', b'+', b' '
    else:
        if qs is not None and not isinstance(qs, (bytes, bytearray,
                                                  memoryview)):
            warnings.warn(f'Accepting {type(qs).__name__} objects with '
                          f'false value in urllib.parse.parse_qsl() is '
                          f'deprecated as of 3.14',
                          DeprecationWarning, stacklevel=_stacklevel)
        return []
    if not qs:
        return []

    if max_num_fields is not None:
        count = 1 + qs.count(separator)
        if max_num_fields < count:
            raise ValueError('Max number of fields exceeded')

    is_str = isinstance(qs, str)
    pairs = []
    for field in qs.split(separator):
        if not field and not strict_parsing:
            continue
        name, eq, value = field.partition(equals)
        if not eq:
            if strict_parsing:
                raise ValueError(f'bad query field: {field!r}')
            if not keep_blank_values:
                continue
        if value or keep_blank_values:
            name = name.replace(plus, space)
            value = value.replace(plus, space)
            if is_str:
                name = unquote(name, encoding=encoding, errors=errors)
                value = unquote(value, encoding=encoding, errors=errors)
            else:
                name = unquote_to_bytes(name)
                value = unquote_to_bytes(value)
            pairs.append((name, value))
    return pairs


# --------------------------------------------------------------------------
# legacy helpers (deprecated public names, private implementations)

def to_bytes(url):
    warnings.warn('urllib.parse.to_bytes() is deprecated as of 3.8',
                  DeprecationWarning, stacklevel=2)
    return _to_bytes(url)


def _to_bytes(url):
    """Ensure *url* only contains ASCII characters (historical name)."""
    if isinstance(url, str):
        try:
            url.encode('ascii')
        except UnicodeError:
            raise UnicodeError('URL ' + repr(url) +
                               ' contains non-ASCII characters') from None
    return url


def unwrap(url):
    """Strip '<URL:...>' style wrapping from a URL."""
    url = str(url).strip()
    if url[:1] == '<' and url[-1:] == '>':
        url = url[1:-1].strip()
    if url[:4] == 'URL:':
        url = url[4:].strip()
    return url


def _deprecated_split(name, replacement='urllib.parse.urlsplit()'):
    impl = globals()['_' + name]

    def wrapper(*args, **kwargs):
        warnings.warn(f'urllib.parse.{name}() is deprecated as of 3.8, '
                      f'use {replacement} instead',
                      DeprecationWarning, stacklevel=2)
        return impl(*args, **kwargs)
    wrapper.__name__ = wrapper.__qualname__ = name
    wrapper.__doc__ = impl.__doc__
    return wrapper


def _splittype(url):
    """'type:rest' -> ('type', 'rest'); (None, url) if there is no type."""
    colon = url.find(':')
    if colon > 0:
        scheme = url[:colon]
        if '/' not in scheme:
            return scheme.lower(), url[colon + 1:]
    return None, url


_HOST_RE = re.compile('//([^/#?]*)(.*)', re.DOTALL)


def _splithost(url):
    """'//host[:port]/path' -> ('host[:port]', '/path')."""
    match = _HOST_RE.match(url)
    if match:
        host, path = match.groups()
        if path and path[0] != '/':
            path = '/' + path
        return host, path
    return None, url


def _splituser(host):
    """'user[:passwd]@host[:port]' -> ('user[:passwd]', 'host[:port]')."""
    user, at, host = host.rpartition('@')
    return (user if at else None), host


def _splitpasswd(user):
    """'user:passwd' -> ('user', 'passwd')."""
    user, colon, passwd = user.partition(':')
    return user, (passwd if colon else None)


_PORT_RE = re.compile('(.*):([0-9]*)', re.DOTALL)


def _splitport(host):
    """'host:port' -> ('host', 'port')."""
    match = _PORT_RE.fullmatch(host)
    if match:
        host, port = match.groups()
        if port:
            return host, port
    return host, None


def _splitnport(host, defport=-1):
    """'host:port' -> ('host', port as int); None if port is invalid."""
    name, colon, port = host.rpartition(':')
    if not colon:
        return host, defport
    if port:
        if port.isdigit() and port.isascii():
            return name, int(port)
        return name, None
    return name, defport


def _splitquery(url):
    """'/path?query' -> ('/path', 'query')."""
    path, mark, query = url.rpartition('?')
    if mark:
        return path, query
    return url, None


def _splittag(url):
    """'/path#tag' -> ('/path', 'tag')."""
    path, mark, tag = url.rpartition('#')
    if mark:
        return path, tag
    return url, None


def _splitattr(url):
    """'/path;attr1=value1;attr2=value2' -> ('/path', ['attr1=value1', ...])."""
    words = url.split(';')
    return words[0], words[1:]


def _splitvalue(attr):
    """'attr=value' -> ('attr', 'value')."""
    attr, eq, value = attr.partition('=')
    return attr, (value if eq else None)


splittype = _deprecated_split('splittype')
splithost = _deprecated_split('splithost')
splituser = _deprecated_split('splituser')
splitpasswd = _deprecated_split('splitpasswd')
splitport = _deprecated_split('splitport')
splitnport = _deprecated_split('splitnport')
splitquery = _deprecated_split('splitquery')
splittag = _deprecated_split('splittag')
splitattr = _deprecated_split('splitattr')
splitvalue = _deprecated_split('splitvalue', 'urllib.parse.parse_qsl()')
