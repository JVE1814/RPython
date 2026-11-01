"""Open URLs (http, https, ftp, file, data) through a chain of handlers.

urlopen(url) uses a default OpenerDirector built by build_opener().  An
OpenerDirector dispatches each Request to handler objects: methods named
<scheme>_open open a URL, <scheme>_request / <scheme>_response pre- and
post-process requests and responses, and http_error_<code> methods react
to HTTP errors (redirects, authentication challenges, ...).  Handlers run
in the order given by their handler_order attribute.
"""

import base64
import bisect
import contextlib
import email
import hashlib
import http.client
import io
import os
import re
import socket
import string
import sys
import tempfile
import time
import warnings

from urllib.error import URLError, HTTPError, ContentTooShortError
from urllib.parse import (
    urlparse, urlsplit, urljoin, unwrap, quote, unquote,
    _splittype, _splithost, _splitport, _splituser, _splitpasswd,
    _splitattr, _splitvalue, _splittag,
    unquote_to_bytes, urlunparse)
from urllib.response import addinfourl, addclosehook

try:
    import ssl
except ImportError:
    _have_ssl = False
else:
    _have_ssl = True

__all__ = [
    # classes
    'Request', 'OpenerDirector', 'BaseHandler', 'HTTPDefaultErrorHandler',
    'HTTPRedirectHandler', 'HTTPCookieProcessor', 'ProxyHandler',
    'HTTPPasswordMgr', 'HTTPPasswordMgrWithDefaultRealm',
    'HTTPPasswordMgrWithPriorAuth', 'AbstractBasicAuthHandler',
    'HTTPBasicAuthHandler', 'ProxyBasicAuthHandler',
    'AbstractDigestAuthHandler', 'HTTPDigestAuthHandler',
    'ProxyDigestAuthHandler', 'HTTPHandler', 'FileHandler', 'FTPHandler',
    'CacheFTPHandler', 'DataHandler', 'UnknownHandler', 'HTTPErrorProcessor',
    # functions
    'urlopen', 'install_opener', 'build_opener',
    'pathname2url', 'url2pathname', 'getproxies',
    'urlretrieve', 'urlcleanup',
]

# Used in the default User-agent header.
__version__ = '%d.%d' % sys.version_info[:2]

_opener = None


def urlopen(url, data=None, timeout=socket._GLOBAL_DEFAULT_TIMEOUT, *,
            context=None):
    """Open *url* (a string or a Request) and return a response object.

    *data* is sent as the request body (making an HTTP request a POST).
    *context* is an ssl.SSLContext used for HTTPS.  The default opener
    is created on first use and can be replaced with install_opener().
    """
    global _opener
    if context:
        opener = build_opener(HTTPSHandler(context=context))
    elif _opener is None:
        _opener = opener = build_opener()
    else:
        opener = _opener
    return opener.open(url, data, timeout)


def install_opener(opener):
    """Make *opener* the opener used by urlopen()."""
    global _opener
    _opener = opener


_url_tempfiles = []


def urlretrieve(url, filename=None, reporthook=None, data=None):
    """Copy the resource at *url* to a local file.

    Returns (filename, headers).  Without *filename* a temporary file is
    created (removed again by urlcleanup()); local files are not copied.
    *reporthook(blocknum, blocksize, totalsize)* is called once when the
    connection is made and then after each block.
    """
    url_type, path = _splittype(url)
    with contextlib.closing(urlopen(url, data)) as fp:
        headers = fp.info()
        if url_type == 'file' and not filename:
            return os.path.normpath(path), headers

        if filename:
            out = open(filename, 'wb')
        else:
            out = tempfile.NamedTemporaryFile(delete=False)
            filename = out.name
            _url_tempfiles.append(filename)

        with out:
            result = filename, headers
            blocksize = 1024 * 8
            size = -1
            received = 0
            blocknum = 0
            if 'content-length' in headers:
                size = int(headers['Content-Length'])
            if reporthook:
                reporthook(blocknum, blocksize, size)
            while True:
                block = fp.read(blocksize)
                if not block:
                    break
                received += len(block)
                out.write(block)
                blocknum += 1
                if reporthook:
                    reporthook(blocknum, blocksize, size)

    if size >= 0 and received < size:
        raise ContentTooShortError(
            f'retrieval incomplete: got only {received} out of {size} bytes',
            result)
    return result


def urlcleanup():
    """Remove temporary files left by urlretrieve() and reset the default
    opener."""
    for name in _url_tempfiles:
        try:
            os.unlink(name)
        except OSError:
            pass
    del _url_tempfiles[:]
    global _opener
    _opener = None


_cut_port_re = re.compile(r':\d+$', re.ASCII)


def request_host(request):
    """The lowercased host of *request*, without the port (RFC 2965)."""
    host = urlparse(request.full_url)[1]
    if host == '':
        host = request.get_header('Host', '')
    return _cut_port_re.sub('', host, 1).lower()


class Request:
    """A URL request: URL, body, headers and some metadata."""

    def __init__(self, url, data=None, headers={}, origin_req_host=None,
                 unverifiable=False, method=None):
        self.full_url = url
        self.headers = {}
        self.unredirected_hdrs = {}
        self._data = None
        self.data = data
        self._tunnel_host = None
        for key, value in headers.items():
            self.add_header(key, value)
        if origin_req_host is None:
            origin_req_host = request_host(self)
        self.origin_req_host = origin_req_host
        self.unverifiable = unverifiable
        if method:
            self.method = method

    @property
    def full_url(self):
        if self.fragment:
            return f'{self._full_url}#{self.fragment}'
        return self._full_url

    @full_url.setter
    def full_url(self, url):
        self._full_url = unwrap(url)
        self._full_url, self.fragment = _splittag(self._full_url)
        self._parse()

    @full_url.deleter
    def full_url(self):
        self._full_url = None
        self.fragment = None
        self.selector = ''

    @property
    def data(self):
        return self._data

    @data.setter
    def data(self, data):
        if data != self._data:
            self._data = data
            # a length computed for the old body would be wrong now
            if self.has_header('Content-length'):
                self.remove_header('Content-length')

    @data.deleter
    def data(self):
        self.data = None

    def _parse(self):
        self.type, rest = _splittype(self._full_url)
        if self.type is None:
            raise ValueError(f'unknown url type: {self.full_url!r}')
        self.host, self.selector = _splithost(rest)
        if self.host:
            self.host = unquote(self.host)

    def get_method(self):
        """The HTTP method: the *method* given, else POST if there is a
        body, else GET."""
        default = 'POST' if self.data is not None else 'GET'
        return getattr(self, 'method', default)

    def get_full_url(self):
        return self.full_url

    def set_proxy(self, host, type):
        if self.type == 'https' and not self._tunnel_host:
            self._tunnel_host = self.host
        else:
            self.type = type
            self.selector = self.full_url
        self.host = host

    def has_proxy(self):
        return self.selector == self.full_url

    def add_header(self, key, val):
        self.headers[key.capitalize()] = val

    def add_unredirected_header(self, key, val):
        self.unredirected_hdrs[key.capitalize()] = val

    def has_header(self, header_name):
        return (header_name in self.headers or
                header_name in self.unredirected_hdrs)

    def get_header(self, header_name, default=None):
        return self.headers.get(
            header_name, self.unredirected_hdrs.get(header_name, default))

    def remove_header(self, header_name):
        self.headers.pop(header_name, None)
        self.unredirected_hdrs.pop(header_name, None)

    def header_items(self):
        merged = {**self.unredirected_hdrs, **self.headers}
        return list(merged.items())


class OpenerDirector:
    """Run a Request through the registered handlers."""

    def __init__(self):
        self.addheaders = [('User-agent', f'Python-urllib/{__version__}')]
        self.handlers = []
        self.handle_open = {}
        self.handle_error = {}
        self.process_response = {}
        self.process_request = {}

    # names that look like handler methods but are not
    _NOT_HANDLER_METHODS = frozenset(('redirect_request', 'do_open',
                                      'proxy_open'))

    def add_handler(self, handler):
        if not hasattr(handler, 'add_parent'):
            raise TypeError(f'expected BaseHandler instance, '
                            f'got {type(handler)!r}')
        added = False
        for name in dir(handler):
            if name in self._NOT_HANDLER_METHODS:
                continue
            protocol, sep, condition = name.partition('_')
            if not protocol or not sep:
                continue
            if condition.startswith('error'):
                kind = condition.partition('_')[2] or condition
                try:
                    kind = int(kind)
                except ValueError:
                    pass
                table = self.handle_error.setdefault(protocol, {})
            elif condition == 'open':
                kind, table = protocol, self.handle_open
            elif condition == 'response':
                kind, table = protocol, self.process_response
            elif condition == 'request':
                kind, table = protocol, self.process_request
            else:
                continue
            chain = table.setdefault(kind, [])
            if chain:
                bisect.insort(chain, handler)
            else:
                chain.append(handler)
            added = True
        if added:
            bisect.insort(self.handlers, handler)
            handler.add_parent(self)

    def close(self):
        # kept for backwards compatibility; nothing to release
        pass

    def _call_chain(self, chain, kind, meth_name, *args):
        for handler in chain.get(kind, ()):
            result = getattr(handler, meth_name)(*args)
            if result is not None:
                return result
        return None

    def open(self, fullurl, data=None,
             timeout=socket._GLOBAL_DEFAULT_TIMEOUT):
        """Open *fullurl* (a URL string or a Request)."""
        if isinstance(fullurl, str):
            req = Request(fullurl, data)
        else:
            req = fullurl
            if data is not None:
                req.data = data
        req.timeout = timeout
        protocol = req.type

        meth_name = protocol + '_request'
        for processor in self.process_request.get(protocol, []):
            req = getattr(processor, meth_name)(req)

        sys.audit('urllib.Request', req.full_url, req.data, req.headers,
                  req.get_method())
        response = self._open(req, data)

        meth_name = protocol + '_response'
        for processor in self.process_response.get(protocol, []):
            response = getattr(processor, meth_name)(req, response)
        return response

    def _open(self, req, data=None):
        result = self._call_chain(self.handle_open, 'default',
                                  'default_open', req)
        if result:
            return result
        protocol = req.type
        result = self._call_chain(self.handle_open, protocol,
                                  protocol + '_open', req)
        if result:
            return result
        return self._call_chain(self.handle_open, 'unknown',
                                'unknown_open', req)

    def error(self, proto, *args):
        """Dispatch an error to the http_error_<code> (or <proto>_error)
        handler methods; for HTTP fall back to http_error_default."""
        if proto in ('http', 'https'):
            table = self.handle_error.get('http', {})
            code = args[2]
            result = self._call_chain(table, code, f'http_error_{code}',
                                      *args)
            if result:
                return result
            return self._call_chain(table, 'default', 'http_error_default',
                                    *args)
        return self._call_chain(self.handle_error, proto, proto + '_error',
                                *args)


def build_opener(*handlers):
    """Create an OpenerDirector with the default handlers plus *handlers*.

    *handlers* may be classes or instances; a handler that is (an instance
    of) a subclass of a default handler replaces that default.
    """
    opener = OpenerDirector()
    defaults = [ProxyHandler, UnknownHandler, HTTPHandler,
                HTTPDefaultErrorHandler, HTTPRedirectHandler,
                FTPHandler, FileHandler, HTTPErrorProcessor, DataHandler]
    if hasattr(http.client, 'HTTPSConnection'):
        defaults.append(HTTPSHandler)
    replaced = set()
    for klass in defaults:
        for given in handlers:
            if isinstance(given, type):
                if issubclass(given, klass):
                    replaced.add(klass)
            elif isinstance(given, klass):
                replaced.add(klass)
    for klass in defaults:
        if klass not in replaced:
            opener.add_handler(klass())
    for given in handlers:
        if isinstance(given, type):
            given = given()
        opener.add_handler(given)
    return opener


class BaseHandler:
    handler_order = 500

    def add_parent(self, parent):
        self.parent = parent

    def close(self):
        pass

    def __lt__(self, other):
        if not hasattr(other, 'handler_order'):
            # keep the original order for objects that are not handlers
            return True
        return self.handler_order < other.handler_order


class HTTPErrorProcessor(BaseHandler):
    """Turn non-2xx responses into calls of the error handlers."""
    handler_order = 1000

    def http_response(self, request, response):
        code, msg, hdrs = response.code, response.msg, response.info()
        if not 200 <= code < 300:
            response = self.parent.error('http', request, response, code,
                                         msg, hdrs)
        return response

    https_response = http_response


class HTTPDefaultErrorHandler(BaseHandler):
    def http_error_default(self, req, fp, code, msg, hdrs):
        raise HTTPError(req.full_url, code, msg, hdrs, fp)


class HTTPRedirectHandler(BaseHandler):
    # a URL may be visited at most max_repeats times, and a chain may have
    # at most max_redirections hops
    max_repeats = 4
    max_redirections = 10

    inf_msg = ('The HTTP server returned a redirect error that would '
               'lead to an infinite loop.\n'
               'The last 30x error message was:\n')

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        """Return the Request to send for a redirect, or raise HTTPError
        if this redirect must not be followed automatically."""
        method = req.get_method()
        if not ((code in (301, 302, 303, 307, 308) and
                 method in ('GET', 'HEAD')) or
                (code in (301, 302, 303) and method == 'POST')):
            raise HTTPError(req.full_url, code, msg, headers, fp)

        # A POST becomes a GET without a body; never carry headers that
        # describe the old body.
        newurl = newurl.replace(' ', '%20')
        body_headers = ('content-length', 'content-type')
        newheaders = {key: value for key, value in req.headers.items()
                      if key.lower() not in body_headers}
        return Request(newurl,
                       method='HEAD' if method == 'HEAD' else 'GET',
                       headers=newheaders,
                       origin_req_host=req.origin_req_host,
                       unverifiable=True)

    def http_error_302(self, req, fp, code, msg, headers):
        if 'location' in headers:
            newurl = headers['location']
        elif 'uri' in headers:
            newurl = headers['uri']
        else:
            return None

        parts = urlparse(newurl)
        if parts.scheme not in ('http', 'https', 'ftp', ''):
            raise HTTPError(
                newurl, code,
                f"{msg} - Redirection to url '{newurl}' is not allowed",
                headers, fp)
        if not parts.path and parts.netloc:
            parts = list(parts)
            parts[2] = '/'
        newurl = urlunparse(parts)
        # characters already percent-encoded (and all punctuation) are kept
        newurl = quote(newurl, encoding='iso-8859-1',
                       safe=string.punctuation)
        newurl = urljoin(req.full_url, newurl)

        new = self.redirect_request(req, fp, code, msg, headers, newurl)
        if new is None:
            return None

        if hasattr(req, 'redirect_dict'):
            visited = new.redirect_dict = req.redirect_dict
            if (visited.get(newurl, 0) >= self.max_repeats or
                    len(visited) >= self.max_redirections):
                raise HTTPError(req.full_url, code, self.inf_msg + msg,
                                headers, fp)
        else:
            visited = new.redirect_dict = req.redirect_dict = {}
        visited[newurl] = visited.get(newurl, 0) + 1

        # the old response must be consumed so the connection can be reused
        fp.read()
        fp.close()
        return self.parent.open(new, timeout=req.timeout)

    http_error_301 = http_error_303 = http_error_307 = http_error_308 = \
        http_error_302


def _parse_proxy(proxy):
    """Split a proxy setting into (scheme, user, password, host[:port]).

    The setting may be a bare authority ("host:port", "user:pw@host") or a
    URL ("http://user:pw@host:port/").
    """
    scheme, rest = _splittype(proxy)
    if not rest.startswith('/'):
        scheme = None
        authority = proxy
    else:
        if not rest.startswith('//'):
            raise ValueError(f'proxy URL with no authority: {proxy!r}')
        at = rest.find('@')
        end = rest.find('/', at if at >= 0 else 2)
        authority = rest[2:] if end < 0 else rest[2:end]
    userinfo, hostport = _splituser(authority)
    if userinfo is not None:
        user, password = _splitpasswd(userinfo)
    else:
        user = password = None
    return scheme, user, password, hostport


class ProxyHandler(BaseHandler):
    # must run before the handlers that open the URL
    handler_order = 100

    def __init__(self, proxies=None):
        if proxies is None:
            proxies = getproxies()
        assert hasattr(proxies, 'keys'), 'proxies must be a mapping'
        self.proxies = proxies
        for kind, url in proxies.items():
            kind = kind.lower()
            setattr(self, f'{kind}_open',
                    lambda r, proxy=url, kind=kind, meth=self.proxy_open:
                        meth(r, proxy, kind))

    def proxy_open(self, req, proxy, type):
        orig_type = req.type
        proxy_type, user, password, hostport = _parse_proxy(proxy)
        if proxy_type is None:
            proxy_type = orig_type
        if req.host and proxy_bypass(req.host):
            return None
        if user and password:
            creds = f'{unquote(user)}:{unquote(password)}'
            token = base64.b64encode(creds.encode()).decode('ascii')
            req.add_header('Proxy-authorization', 'Basic ' + token)
        req.set_proxy(unquote(hostport), proxy_type)
        if orig_type == proxy_type or orig_type == 'https':
            # let the handler for the scheme open it through the proxy
            return None
        return self.parent.open(req, timeout=req.timeout)


class HTTPPasswordMgr:
    """Store (user, password) per realm and URI prefix."""

    def __init__(self):
        self.passwd = {}

    def add_password(self, realm, uri, user, passwd):
        if isinstance(uri, str):
            uri = [uri]
        realm_table = self.passwd.setdefault(realm, {})
        for default_port in (True, False):
            key = tuple(self._reduce(u, default_port) for u in uri)
            realm_table[key] = (user, passwd)

    def find_user_password(self, realm, authuri):
        realm_table = self.passwd.get(realm, {})
        for default_port in (True, False):
            wanted = self._reduce(authuri, default_port)
            for uris, authinfo in realm_table.items():
                for uri in uris:
                    if self._matches(uri, wanted):
                        return authinfo
        return None, None

    @staticmethod
    def _scheme_and_parts(uri):
        parts = urlsplit(uri)
        if parts[1]:
            return parts[0].lower() or None, parts[1], parts[2] or '/'
        return None, uri, '/'

    def _reduce(self, uri, default_port=True):
        """(scheme or None, authority, path) for *uri*."""
        scheme, authority, path = self._scheme_and_parts(uri)
        if default_port and scheme is not None:
            host, port = _splitport(authority)
            if port is None:
                dport = {'http': 80, 'https': 443}.get(scheme)
                if dport is not None:
                    authority = f'{host}:{dport}'
        return scheme, authority, path

    def reduce_uri(self, uri, default_port=True):
        """Return (authority, path) for *uri*; with *default_port* the
        port implied by the scheme is made explicit."""
        return self._reduce(uri, default_port)[1:]

    def is_suburi(self, base, test):
        """True if the (authority, path) pair *test* is at or below
        *base*."""
        if base is test:
            return True
        if base[0] != test[0]:
            return False
        prefix = base[1]
        if test[1] == prefix:
            return True
        if prefix[-1:] != '/':
            prefix += '/'
        return test[1].startswith(prefix)

    def _matches(self, stored, wanted):
        if stored[0] and wanted[0] and stored[0] != wanted[0]:
            return False
        return self.is_suburi(stored[1:], wanted[1:])


class HTTPPasswordMgrWithDefaultRealm(HTTPPasswordMgr):
    """Also try the passwords stored for realm None."""

    def find_user_password(self, realm, authuri):
        user, password = HTTPPasswordMgr.find_user_password(self, realm,
                                                            authuri)
        if user is not None:
            return user, password
        return HTTPPasswordMgr.find_user_password(self, None, authuri)


class HTTPPasswordMgrWithPriorAuth(HTTPPasswordMgrWithDefaultRealm):
    """Also remember which URIs are known to need authentication, so that
    credentials can be sent without waiting for a 401."""

    def __init__(self):
        self.authenticated = {}
        super().__init__()

    def add_password(self, realm, uri, user, passwd, is_authenticated=False):
        self.update_authenticated(uri, is_authenticated)
        if realm is not None:
            super().add_password(None, uri, user, passwd)
        super().add_password(realm, uri, user, passwd)

    def update_authenticated(self, uri, is_authenticated=False):
        if isinstance(uri, str):
            uri = [uri]
        for default_port in (True, False):
            for u in uri:
                self.authenticated[self._reduce(u, default_port)] = \
                    is_authenticated

    def is_authenticated(self, authuri):
        for default_port in (True, False):
            wanted = self._reduce(authuri, default_port)
            for uri, flag in self.authenticated.items():
                if self._matches(uri, wanted):
                    return flag
        return None


_QUOTED_PARAM_WARNING = 'Basic Auth Realm was unquoted'


def _parse_challenges(header):
    """Yield (scheme, params) for the challenges in an authentication
    header value (several challenges may share one header)."""
    current = None
    for item in parse_http_list(header):
        word, space, rest = item.partition(' ')
        if '=' not in word and (space or not current):
            # a new challenge: "<scheme> [param or token]"
            current = (word, {})
            rest = rest.strip()
            yield current
            item = rest
            if not item:
                continue
        if current is None:
            continue
        name, eq, value = item.partition('=')
        if eq:
            current[1][name.strip().lower()] = value.strip()


class AbstractBasicAuthHandler:
    def __init__(self, password_mgr=None):
        if password_mgr is None:
            password_mgr = HTTPPasswordMgr()
        self.passwd = password_mgr
        self.add_password = self.passwd.add_password

    def _parse_realm(self, header):
        for scheme, params in _parse_challenges(header):
            realm = params.get('realm')
            if realm is not None:
                if realm[:1] in ('"', "'") and realm[-1:] == realm[:1]:
                    realm = realm[1:-1]
                else:
                    warnings.warn(_QUOTED_PARAM_WARNING, UserWarning, 3)
            yield scheme, realm

    def http_error_auth_reqed(self, authreq, host, req, headers):
        values = headers.get_all(authreq)
        if not values:
            return None
        unsupported = None
        for value in values:
            for scheme, realm in self._parse_realm(value):
                if scheme.lower() != 'basic':
                    unsupported = scheme
                    continue
                if realm is not None:
                    return self.retry_http_basic_auth(host, req, realm)
        if unsupported is not None:
            raise ValueError('AbstractBasicAuthHandler does not support the '
                             f'following scheme: {unsupported!r}')
        return None

    def retry_http_basic_auth(self, host, req, realm):
        user, password = self.passwd.find_user_password(realm, host)
        if password is None:
            return None
        token = base64.b64encode(f'{user}:{password}'.encode()).decode('ascii')
        auth = 'Basic ' + token
        if req.get_header(self.auth_header, None) == auth:
            return None
        req.add_unredirected_header(self.auth_header, auth)
        return self.parent.open(req, timeout=req.timeout)

    def http_request(self, req):
        if (not hasattr(self.passwd, 'is_authenticated') or
                not self.passwd.is_authenticated(req.full_url)):
            return req
        if not req.has_header('Authorization'):
            user, password = self.passwd.find_user_password(None, req.full_url)
            token = base64.standard_b64encode(
                f'{user}:{password}'.encode()).decode()
            req.add_unredirected_header('Authorization',
                                        f'Basic {token.strip()}')
        return req

    def http_response(self, req, response):
        if hasattr(self.passwd, 'is_authenticated'):
            self.passwd.update_authenticated(req.full_url,
                                             200 <= response.code < 300)
        return response

    https_request = http_request
    https_response = http_response


class HTTPBasicAuthHandler(AbstractBasicAuthHandler, BaseHandler):
    auth_header = 'Authorization'

    def http_error_401(self, req, fp, code, msg, headers):
        return self.http_error_auth_reqed('www-authenticate', req.full_url,
                                          req, headers)


class ProxyBasicAuthHandler(AbstractBasicAuthHandler, BaseHandler):
    auth_header = 'Proxy-authorization'

    def http_error_407(self, req, fp, code, msg, headers):
        # the proxy is authenticated, so look up the password by its
        # authority (req.host is the proxy after set_proxy())
        return self.http_error_auth_reqed('proxy-authenticate', req.host,
                                          req, headers)


def _randombytes(n):
    return os.urandom(n)


class AbstractDigestAuthHandler:
    # RFC 2617 digest authentication (no auth-int, no MD5-sess).

    def __init__(self, passwd=None):
        if passwd is None:
            passwd = HTTPPasswordMgr()
        self.passwd = passwd
        self.add_password = self.passwd.add_password
        self.retried = 0
        self.nonce_count = 0
        self.last_nonce = None

    def reset_retry_count(self):
        self.retried = 0

    def http_error_auth_reqed(self, auth_header, host, req, headers):
        authreq = headers.get(auth_header, None)
        if self.retried > 5:
            # don't loop forever on a server that keeps refusing
            raise HTTPError(req.full_url, 401, 'digest auth failed',
                            headers, None)
        self.retried += 1
        if authreq:
            scheme = authreq.split()[0]
            if scheme.lower() == 'digest':
                return self.retry_http_digest_auth(req, authreq)
            if scheme.lower() != 'basic':
                raise ValueError('AbstractDigestAuthHandler does not support '
                                 f"the following scheme: '{scheme}'")
        return None

    def retry_http_digest_auth(self, req, auth):
        token, challenge = auth.split(' ', 1)
        chal = parse_keqv_list(filter(None, parse_http_list(challenge)))
        auth = self.get_authorization(req, chal)
        if not auth:
            return None
        auth_val = f'Digest {auth}'
        if req.headers.get(self.auth_header, None) == auth_val:
            return None
        req.add_unredirected_header(self.auth_header, auth_val)
        return self.parent.open(req, timeout=req.timeout)

    def get_cnonce(self, nonce):
        seed = f'{self.nonce_count}:{nonce}:{time.ctime()}:'.encode('ascii')
        return hashlib.sha1(seed + _randombytes(8)).hexdigest()[:16]

    def get_authorization(self, req, chal):
        try:
            realm = chal['realm']
            nonce = chal['nonce']
            qop = chal.get('qop')
            algorithm = chal.get('algorithm', 'MD5')
            opaque = chal.get('opaque', None)
        except KeyError:
            return None

        H, KD = self.get_algorithm_impls(algorithm)
        if H is None:
            return None
        user, pw = self.passwd.find_user_password(realm, req.full_url)
        if user is None:
            return None

        entdig = None
        if req.data is not None:
            entdig = self.get_entity_digest(req.data, chal)

        A1 = f'{user}:{realm}:{pw}'
        A2 = f'{req.get_method()}:{req.selector}'
        if qop is None:
            respdig = KD(H(A1), f'{nonce}:{H(A2)}')
        elif 'auth' in qop.split(','):
            if nonce == self.last_nonce:
                self.nonce_count += 1
            else:
                self.nonce_count = 1
                self.last_nonce = nonce
            ncvalue = '%08x' % self.nonce_count
            cnonce = self.get_cnonce(nonce)
            respdig = KD(H(A1), f'{nonce}:{ncvalue}:{cnonce}:auth:{H(A2)}')
        else:
            raise URLError(f"qop '{qop}' is not supported.")

        base = (f'username="{user}", realm="{realm}", nonce="{nonce}", '
                f'uri="{req.selector}", response="{respdig}"')
        if opaque:
            base += f', opaque="{opaque}"'
        if entdig:
            base += f', digest="{entdig}"'
        base += f', algorithm="{algorithm}"'
        if qop:
            base += f', qop=auth, nc={ncvalue}, cnonce="{cnonce}"'
        return base

    def get_algorithm_impls(self, algorithm):
        """Return the (H, KD) functions for a digest *algorithm*."""
        constructors = {'MD5': hashlib.md5, 'SHA': hashlib.sha1,
                        'SHA-256': hashlib.sha256}
        try:
            make = constructors[algorithm]
        except KeyError:
            raise ValueError('Unsupported digest authentication algorithm '
                             f'{algorithm!r}') from None

        def H(text):
            return make(text.encode('ascii')).hexdigest()

        def KD(secret, data):
            return H(f'{secret}:{data}')
        return H, KD

    def get_entity_digest(self, data, chal):
        return None


class HTTPDigestAuthHandler(BaseHandler, AbstractDigestAuthHandler):
    """Digest authentication against the origin server."""

    auth_header = 'Authorization'
    # runs before basic auth: the stronger scheme is preferred
    handler_order = 490

    def http_error_401(self, req, fp, code, msg, headers):
        host = urlparse(req.full_url)[1]
        retry = self.http_error_auth_reqed('www-authenticate', host, req,
                                           headers)
        self.reset_retry_count()
        return retry


class ProxyDigestAuthHandler(BaseHandler, AbstractDigestAuthHandler):
    auth_header = 'Proxy-Authorization'
    handler_order = 490

    def http_error_407(self, req, fp, code, msg, headers):
        retry = self.http_error_auth_reqed('proxy-authenticate', req.host,
                                           req, headers)
        self.reset_retry_count()
        return retry


class AbstractHTTPHandler(BaseHandler):

    def __init__(self, debuglevel=None):
        if debuglevel is None:
            debuglevel = http.client.HTTPConnection.debuglevel
        self._debuglevel = debuglevel

    def set_http_debuglevel(self, level):
        self._debuglevel = level

    def _get_content_length(self, request):
        return http.client.HTTPConnection._get_content_length(
            request.data, request.get_method())

    def do_request_(self, request):
        host = request.host
        if not host:
            raise URLError('no host given')

        if request.data is not None:
            data = request.data
            if isinstance(data, str):
                raise TypeError('POST data should be bytes, an iterable of '
                                'bytes, or a file object. It cannot be of '
                                'type str.')
            if not request.has_header('Content-type'):
                request.add_unredirected_header(
                    'Content-type', 'application/x-www-form-urlencoded')
            if (not request.has_header('Content-length') and
                    not request.has_header('Transfer-encoding')):
                length = self._get_content_length(request)
                if length is not None:
                    request.add_unredirected_header('Content-length',
                                                    str(length))
                else:
                    request.add_unredirected_header('Transfer-encoding',
                                                    'chunked')

        sel_host = host
        if request.has_proxy():
            scheme, sel = _splittype(request.selector)
            sel_host, sel_path = _splithost(sel)
        if not request.has_header('Host'):
            request.add_unredirected_header('Host', sel_host)
        for name, value in self.parent.addheaders:
            name = name.capitalize()
            if not request.has_header(name):
                request.add_unredirected_header(name, value)
        return request

    def do_open(self, http_class, req, **http_conn_args):
        """Send *req* with a new *http_class* connection and return the
        HTTPResponse."""
        host = req.host
        if not host:
            raise URLError('no host given')

        conn = http_class(host, timeout=req.timeout, **http_conn_args)
        conn.set_debuglevel(self._debuglevel)

        headers = dict(req.unredirected_hdrs)
        headers.update(req.headers)
        # One request per connection: persistent connections would need a
        # pool, and the response must be readable after we return.
        headers['Connection'] = 'close'
        headers = {name.title(): value for name, value in headers.items()}

        if req._tunnel_host:
            tunnel_headers = {}
            proxy_auth = 'Proxy-Authorization'
            if proxy_auth in headers:
                tunnel_headers[proxy_auth] = headers.pop(proxy_auth)
            conn.set_tunnel(req._tunnel_host, headers=tunnel_headers)

        try:
            try:
                conn.request(req.get_method(), req.selector, req.data,
                             headers,
                             encode_chunked=req.has_header('Transfer-encoding'))
            except OSError as err:
                raise URLError(err)
            response = conn.getresponse()
        except:
            conn.close()
            raise

        # The response keeps its own reference to the socket's file.
        if conn.sock:
            conn.sock.close()
            conn.sock = None

        response.url = req.get_full_url()
        response.msg = response.reason
        return response


class HTTPHandler(AbstractHTTPHandler):

    def http_open(self, req):
        return self.do_open(http.client.HTTPConnection, req)

    http_request = AbstractHTTPHandler.do_request_


if hasattr(http.client, 'HTTPSConnection'):

    class HTTPSHandler(AbstractHTTPHandler):

        def __init__(self, debuglevel=None, context=None,
                     check_hostname=None):
            if debuglevel is None:
                debuglevel = http.client.HTTPSConnection.debuglevel
            AbstractHTTPHandler.__init__(self, debuglevel)
            if context is None:
                context = http.client._create_https_context(
                    http.client.HTTPSConnection._http_vsn)
            if check_hostname is not None:
                context.check_hostname = check_hostname
            self._context = context

        def https_open(self, req):
            return self.do_open(http.client.HTTPSConnection, req,
                                context=self._context)

        https_request = AbstractHTTPHandler.do_request_

    __all__.append('HTTPSHandler')


class HTTPCookieProcessor(BaseHandler):
    """Add cookies from a CookieJar to requests and store cookies set by
    responses."""

    def __init__(self, cookiejar=None):
        import http.cookiejar
        if cookiejar is None:
            cookiejar = http.cookiejar.CookieJar()
        self.cookiejar = cookiejar

    def http_request(self, request):
        self.cookiejar.add_cookie_header(request)
        return request

    def http_response(self, request, response):
        self.cookiejar.extract_cookies(response, request)
        return response

    https_request = http_request
    https_response = http_response


class UnknownHandler(BaseHandler):
    def unknown_open(self, req):
        raise URLError(f'unknown url type: {req.type}')


def parse_keqv_list(l):
    """Parse a list of 'key=value' strings into a dict (values unquoted)."""
    parsed = {}
    for elt in l:
        key, value = elt.split('=', 1)
        if value[0] == '"' and value[-1] == '"':
            value = value[1:-1]
        parsed[key] = value
    return parsed


def parse_http_list(s):
    """Split a comma separated HTTP header value into its elements.

    Commas inside quoted strings do not split; backslash escapes inside
    quoted strings are resolved.  Elements are stripped.
    """
    items = []
    part = []
    escaped = False
    quoted = False
    for ch in s:
        if escaped:
            part.append(ch)
            escaped = False
        elif quoted:
            if ch == '\\':
                escaped = True
            else:
                if ch == '"':
                    quoted = False
                part.append(ch)
        elif ch == ',':
            items.append(''.join(part))
            part = []
        else:
            if ch == '"':
                quoted = True
            part.append(ch)
    if part:
        items.append(''.join(part))
    return [item.strip() for item in items]


def _http_date(timestamp):
    """RFC 7231 date (like email.utils.formatdate(..., usegmt=True))."""
    tm = time.gmtime(timestamp)
    return '%s, %02d %s %04d %02d:%02d:%02d GMT' % (
        ('Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun')[tm.tm_wday],
        tm.tm_mday,
        ('Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep',
         'Oct', 'Nov', 'Dec')[tm.tm_mon - 1],
        tm.tm_year, tm.tm_hour, tm.tm_min, tm.tm_sec)


class FileHandler(BaseHandler):
    """Open local files (file: URLs)."""

    names = None

    def file_open(self, req):
        return self.open_local_file(req)

    def get_names(self):
        if FileHandler.names is None:
            try:
                FileHandler.names = tuple(
                    socket.gethostbyname_ex('localhost')[2] +
                    socket.gethostbyname_ex(socket.gethostname())[2])
            except socket.gaierror:
                FileHandler.names = (socket.gethostbyname('localhost'),)
        return FileHandler.names

    def open_local_file(self, req):
        import mimetypes
        localfile = url2pathname(req.full_url, require_scheme=True,
                                 resolve_host=True)
        try:
            stats = os.stat(localfile)
            size = stats.st_size
            modified = _http_date(stats.st_mtime)
            mtype = mimetypes.guess_file_type(localfile)[0]
            headers = email.message_from_string(
                f'Content-type: {mtype or "text/plain"}\n'
                f'Content-length: {size}\n'
                f'Last-modified: {modified}\n')
            origurl = pathname2url(localfile, add_scheme=True)
            return addinfourl(open(localfile, 'rb'), headers, origurl)
        except OSError as exc:
            raise URLError(exc, exc.filename)



def _safe_gethostbyname(host):
    try:
        return socket.gethostbyname(host)
    except socket.gaierror:
        return None


class FTPHandler(BaseHandler):
    """Fetch ftp: URLs (anonymous unless the URL has user:password)."""

    def ftp_open(self, req):
        import ftplib
        import mimetypes
        host = req.host
        if not host:
            raise URLError('ftp error: no host given')
        host, port = _splitport(host)
        if port is None:
            port = ftplib.FTP_PORT
        else:
            port = int(port)

        user, host = _splituser(host)
        if user:
            user, passwd = _splitpasswd(user)
        else:
            passwd = None
        host = unquote(host)
        user = unquote(user or '')
        passwd = unquote(passwd or '')

        try:
            host = socket.gethostbyname(host)
        except OSError as msg:
            raise URLError(msg)
        path, attrs = _splitattr(req.selector)
        dirs = [unquote(d) for d in path.split('/')]
        dirs, filename = dirs[:-1], dirs[-1]
        if dirs and not dirs[0]:
            dirs = dirs[1:]
        try:
            fw = self.connect_ftp(user, passwd, host, port, dirs, req.timeout)
            kind = 'I' if filename else 'D'
            for attr in attrs:
                name, value = _splitvalue(attr)
                if name.lower() == 'type' and value in ('a', 'A', 'i', 'I',
                                                        'd', 'D'):
                    kind = value.upper()
            fp, retrlen = fw.retrfile(filename, kind)
            header_text = ''
            mtype = mimetypes.guess_type(req.full_url)[0]
            if mtype:
                header_text += f'Content-type: {mtype}\n'
            if retrlen is not None and retrlen >= 0:
                header_text += f'Content-length: {retrlen}\n'
            headers = email.message_from_string(header_text)
            return addinfourl(fp, headers, req.full_url)
        except ftplib.all_errors as exc:
            raise URLError(f'ftp error: {exc}') from exc

    def connect_ftp(self, user, passwd, host, port, dirs, timeout):
        return ftpwrapper(user, passwd, host, port, dirs, timeout,
                          persistent=False)


class CacheFTPHandler(FTPHandler):
    """An FTPHandler that keeps connections open for reuse."""

    def __init__(self):
        self.cache = {}
        self.timeout = {}
        self.soonest = 0
        self.delay = 60
        self.max_conns = 16

    def setTimeout(self, t):
        self.delay = t

    def setMaxConns(self, m):
        self.max_conns = m

    def connect_ftp(self, user, passwd, host, port, dirs, timeout):
        key = user, host, port, '/'.join(dirs), timeout
        if key in self.cache:
            self.timeout[key] = time.time() + self.delay
        else:
            self.cache[key] = ftpwrapper(user, passwd, host, port, dirs,
                                         timeout)
            self.timeout[key] = time.time() + self.delay
        self.check_cache()
        return self.cache[key]

    def check_cache(self):
        now = time.time()
        if self.soonest <= now:
            for key, deadline in list(self.timeout.items()):
                if deadline < now:
                    self.cache[key].close()
                    del self.cache[key]
                    del self.timeout[key]
        self.soonest = min(self.timeout.values(), default=0)
        if len(self.cache) == self.max_conns:
            for key, deadline in list(self.timeout.items()):
                if deadline == self.soonest:
                    del self.cache[key]
                    del self.timeout[key]
                    break
            self.soonest = min(self.timeout.values(), default=0)

    def clear_cache(self):
        for conn in self.cache.values():
            conn.close()
        self.cache.clear()
        self.timeout.clear()


_C0_CONTROLS = frozenset(chr(c) for c in range(0x20)) | {'\x7f'}


class DataHandler(BaseHandler):
    """Open data: URLs (RFC 2397)."""

    def data_open(self, req):
        url = req.full_url
        scheme, data = url.split(':', 1)
        mediatype, sep, data = data.partition(',')
        if not sep:
            raise ValueError("data URL must contain a ','")
        if not _C0_CONTROLS.isdisjoint(mediatype):
            raise ValueError('data URL media type must not contain control '
                             'characters')
        if mediatype.endswith(';base64'):
            data = base64.decodebytes(unquote_to_bytes(data))
            mediatype = mediatype[:-len(';base64')]
        else:
            data = unquote_to_bytes(data)
        if not mediatype:
            mediatype = 'text/plain;charset=US-ASCII'
        headers = email.message_from_string(
            f'Content-type: {mediatype}\nContent-length: {len(data)}\n')
        return addinfourl(io.BytesIO(data), headers, url)


# --------------------------------------------------------------------------
# local file names <-> URLs

def _fs_quote(path):
    return quote(path, encoding=sys.getfilesystemencoding(),
                 errors=sys.getfilesystemencodeerrors())


_thishost = None
_localhost = None


def localhost():
    """The IP address of 'localhost'."""
    global _localhost
    if _localhost is None:
        _localhost = socket.gethostbyname('localhost')
    return _localhost


def thishost():
    """The IP addresses of this host."""
    global _thishost
    if _thishost is None:
        try:
            _thishost = tuple(
                socket.gethostbyname_ex(socket.gethostname())[2])
        except socket.gaierror:
            _thishost = tuple(socket.gethostbyname_ex('localhost')[2])
    return _thishost


def _is_local_authority(authority, resolve):
    if authority.lower() == 'localhost':
        return True
    try:
        hostname = socket.gethostname()
    except (AttributeError, OSError):
        hostname = None
    if hostname and authority == hostname:
        return True
    if not resolve:
        return False
    try:
        address = socket.gethostbyname(authority)
    except (socket.gaierror, AttributeError, UnicodeEncodeError):
        return False
    if address in thishost():
        return True
    try:
        local = socket.gethostbyname_ex('localhost')[2]
    except socket.gaierror:
        local = ()
    return address in local


if os.name == 'nt':
    from nturl2path import url2pathname as _nt_url2pathname
    from nturl2path import pathname2url as _nt_pathname2url


def url2pathname(url, *, require_scheme=False, resolve_host=False):
    """Convert a file: URL (or its path part) to a local file name.

    With *require_scheme* the URL must start with 'file:'.  A host other
    than this machine raises URLError; with *resolve_host* the host is
    looked up to decide that.
    """
    if require_scheme:
        scheme, colon, url = url.partition(':')
        if not colon or scheme.lower() != 'file':
            raise URLError("URL is missing a 'file:' scheme")
    if os.name == 'nt':
        return _nt_url2pathname(url, resolve_host=resolve_host)
    url = url.partition('#')[0].partition('?')[0]
    if url[:2] == '//':
        end = url.find('/', 2)
        if end < 0:
            authority, url = url[2:], ''
        else:
            authority, url = url[2:end], url[end:]
        if authority and not _is_local_authority(authority, resolve_host):
            raise URLError('file:// scheme is supported only on localhost')
        if not url:
            url = '/'
    return unquote(url, encoding=sys.getfilesystemencoding(),
                   errors=sys.getfilesystemencodeerrors())


def pathname2url(pathname, *, add_scheme=False):
    """Convert a local file name to the path part of a file: URL (or a
    whole URL with *add_scheme*)."""
    if os.name == 'nt':
        url = _nt_pathname2url(pathname)
    else:
        url = _fs_quote(pathname)
        if pathname.startswith('/'):
            url = '//' + url
    if add_scheme:
        url = 'file:' + url
    return url


# --------------------------------------------------------------------------
# proxies

def getproxies_environment():
    """Proxy settings from <scheme>_proxy environment variables.

    Lowercase variable names win over other spellings; an empty lowercase
    variable removes a setting.  HTTP_PROXY is ignored in CGI scripts (it
    may come from the client's Proxy: header).
    """
    proxies = {}
    for name, value in os.environ.items():
        lower = name.lower()
        if value and lower[-6:] == '_proxy':
            proxies[lower[:-6]] = value
    if 'REQUEST_METHOD' in os.environ:
        proxies.pop('http', None)
    for name, value in os.environ.items():
        if name[-6:] == '_proxy':
            lower = name.lower()
            if value:
                proxies[lower[:-6]] = value
            else:
                proxies.pop(lower[:-6], None)
    return proxies


def proxy_bypass_environment(host, proxies=None):
    """True if *host* matches the no_proxy setting."""
    if proxies is None:
        proxies = getproxies_environment()
    try:
        no_proxy = proxies['no']
    except KeyError:
        return False
    if no_proxy == '*':
        return True
    host = host.lower()
    hostonly, port = _splitport(host)
    for name in no_proxy.split(','):
        name = name.strip().lstrip('.').lower()
        if not name:
            continue
        if hostonly == name or host == name:
            return True
        suffix = '.' + name
        if hostonly.endswith(suffix) or host.endswith(suffix):
            return True
    return False


def _proxy_bypass_macosx_sysconf(host, proxy_settings):
    """Decide a proxy bypass from macOS system settings (a dict with
    'exclude_simple' and 'exceptions')."""
    from fnmatch import fnmatch
    from ipaddress import AddressValueError, IPv4Address

    hostonly, port = _splitport(host)

    def ip2num(ipaddr):
        parts = list(map(int, ipaddr.split('.')))
        parts += [0] * (4 - len(parts))
        return parts[0] << 24 | parts[1] << 16 | parts[2] << 8 | parts[3]

    if '.' not in host and proxy_settings['exclude_simple']:
        return True

    hostIP = None
    try:
        hostIP = int(IPv4Address(hostonly))
    except AddressValueError:
        pass

    for value in proxy_settings.get('exceptions', ()):
        if not value:
            continue
        match = re.match(r'(\d+(?:\.\d+)*)(/\d+)?', value)
        if match is not None and hostIP is not None:
            base = ip2num(match.group(1))
            mask = match.group(2)
            if mask is None:
                mask = 8 * (match.group(1).count('.') + 1)
            else:
                mask = int(mask[1:])
            if mask < 0 or mask > 32:
                continue
            shift = 32 - mask
            if (hostIP >> shift) == (base >> shift):
                return True
        elif fnmatch(host, value):
            return True
    return False


def _proxy_bypass_winreg_override(host, override):
    """Decide a proxy bypass from a Windows ProxyOverride string."""
    from fnmatch import fnmatch
    host, _ = _splitport(host)
    for test in override.split(';'):
        test = test.strip()
        if test == '<local>':
            if '.' not in host:
                return True
        elif fnmatch(host, test):
            return True
    return False


def getproxies():
    """Proxy settings: the environment, else the system configuration."""
    return getproxies_environment()


def proxy_bypass(host):
    """True if *host* should be accessed without a proxy."""
    proxies = getproxies_environment()
    if proxies:
        return proxy_bypass_environment(host, proxies)
    return False


# --------------------------------------------------------------------------
# FTP connections

class ftpwrapper:
    """An FTP connection used by FTPHandler, opened for a directory."""

    def __init__(self, user, passwd, host, port, dirs, timeout=None,
                 persistent=True):
        self.user = user
        self.passwd = passwd
        self.host = host
        self.port = port
        self.dirs = dirs
        self.timeout = timeout
        self.refcount = 0
        self.keepalive = persistent
        try:
            self.init()
        except:
            self.close()
            raise

    def init(self):
        import ftplib
        self.busy = 0
        self.ftp = ftplib.FTP()
        self.ftp.connect(self.host, self.port, self.timeout)
        self.ftp.login(self.user, self.passwd)
        self.ftp.cwd('/'.join(self.dirs))

    def retrfile(self, file, type):
        import ftplib
        self.endtransfer()
        if type in ('d', 'D'):
            cmd = 'TYPE A'
            isdir = 1
        else:
            cmd = 'TYPE ' + type
            isdir = 0
        try:
            self.ftp.voidcmd(cmd)
        except ftplib.all_errors:
            self.init()
            self.ftp.voidcmd(cmd)
        conn = None
        if file and not isdir:
            try:
                conn, retrlen = self.ftp.ntransfercmd('RETR ' + file)
            except ftplib.error_perm as reason:
                if str(reason)[:3] != '550':
                    raise URLError(f'ftp error: {reason}') from reason
        if not conn:
            self.ftp.voidcmd('TYPE A')
            if file:
                pwd = self.ftp.pwd()
                try:
                    try:
                        self.ftp.cwd(file)
                    except ftplib.error_perm as reason:
                        raise URLError(f'ftp error: {reason}') from reason
                finally:
                    self.ftp.cwd(pwd)
                cmd = 'LIST ' + file
            else:
                cmd = 'LIST'
            conn, retrlen = self.ftp.ntransfercmd(cmd)
        self.busy = 1
        ftpobj = addclosehook(conn.makefile('rb'), self.file_close)
        self.refcount += 1
        conn.close()
        return ftpobj, retrlen

    def endtransfer(self):
        if not self.busy:
            return
        self.busy = 0
        try:
            self.ftp.voidresp()
        except Exception:
            pass

    def close(self):
        self.keepalive = False
        if self.refcount <= 0:
            self.real_close()

    def file_close(self):
        self.endtransfer()
        self.refcount -= 1
        if self.refcount <= 0 and not self.keepalive:
            self.real_close()

    def real_close(self):
        self.endtransfer()
        try:
            self.ftp.close()
        except Exception:
            pass
