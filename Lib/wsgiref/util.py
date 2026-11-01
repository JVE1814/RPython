"""Small helpers for WSGI servers and applications."""

__all__ = [
    'FileWrapper', 'guess_scheme', 'application_uri', 'request_uri',
    'shift_path_info', 'setup_testing_defaults', 'is_hop_by_hop',
]


class FileWrapper:
    """Turn a file-like object into an iterable of blocks."""

    def __init__(self, filelike, blksize=8192):
        self.filelike = filelike
        self.blksize = blksize
        if hasattr(filelike, 'close'):
            self.close = filelike.close

    def __iter__(self):
        return self

    def __next__(self):
        data = self.filelike.read(self.blksize)
        if data:
            return data
        raise StopIteration


def guess_scheme(environ):
    """'https' if the HTTPS variable says so, else 'http'."""
    if environ.get('HTTPS') in ('yes', 'on', '1'):
        return 'https'
    return 'http'


def application_uri(environ):
    """The base URI of the application (without PATH_INFO and query)."""
    from urllib.parse import quote
    url = environ['wsgi.url_scheme'] + '://'
    host = environ.get('HTTP_HOST')
    if host:
        url += host
    else:
        url += environ['SERVER_NAME']
        port = environ['SERVER_PORT']
        default = '443' if environ['wsgi.url_scheme'] == 'https' else '80'
        if port != default:
            url += ':' + port
    url += quote(environ.get('SCRIPT_NAME') or '/', encoding='latin1')
    return url


def request_uri(environ, include_query=True):
    """The full URI of the request (optionally with the query string)."""
    from urllib.parse import quote
    url = application_uri(environ)
    path = quote(environ.get('PATH_INFO', ''), safe='/;=,', encoding='latin1')
    if not environ.get('SCRIPT_NAME'):
        url += path[1:]
    else:
        url += path
    if include_query and environ.get('QUERY_STRING'):
        url += '?' + environ['QUERY_STRING']
    return url


def shift_path_info(environ):
    """Move the first segment of PATH_INFO to the end of SCRIPT_NAME and
    return it (None if PATH_INFO is empty).

    '.' and '..' segments are resolved and empty segments dropped, except
    that a trailing slash is kept.
    """
    path_info = environ.get('PATH_INFO', '')
    if not path_info:
        return None

    segments = path_info.split('/')
    segments[1:-1] = [s for s in segments[1:-1] if s and s != '.']
    name = segments[1]
    del segments[1]

    script_name = environ.get('SCRIPT_NAME', '')
    import posixpath
    script_name = posixpath.normpath(script_name + '/' + name)
    if script_name.endswith('/'):
        script_name = script_name[:-1]
    if not name and not script_name.endswith('/'):
        script_name += '/'

    environ['SCRIPT_NAME'] = script_name
    environ['PATH_INFO'] = '/'.join(segments)

    # A lone '.' left over at the end means "no further segment".
    if name == '.':
        name = None
    return name


def setup_testing_defaults(environ):
    """Fill in the WSGI variables a test request needs (in place)."""
    environ.setdefault('SERVER_NAME', '127.0.0.1')
    environ.setdefault('SERVER_PROTOCOL', 'HTTP/1.0')

    environ.setdefault('HTTP_HOST', environ['SERVER_NAME'])
    environ.setdefault('REQUEST_METHOD', 'GET')

    if 'SCRIPT_NAME' not in environ and 'PATH_INFO' not in environ:
        environ.setdefault('SCRIPT_NAME', '')
        environ.setdefault('PATH_INFO', '/')

    environ.setdefault('wsgi.version', (1, 0))
    environ.setdefault('wsgi.run_once', 0)
    environ.setdefault('wsgi.multithread', 0)
    environ.setdefault('wsgi.multiprocess', 0)

    from io import StringIO, BytesIO
    environ.setdefault('wsgi.input', BytesIO())
    environ.setdefault('wsgi.errors', StringIO())
    environ.setdefault('wsgi.url_scheme', guess_scheme(environ))

    if environ['wsgi.url_scheme'] == 'http':
        environ.setdefault('SERVER_PORT', '80')
    elif environ['wsgi.url_scheme'] == 'https':
        environ.setdefault('SERVER_PORT', '443')


_HOP_BY_HOP = frozenset((
    'connection', 'keep-alive', 'proxy-authenticate', 'proxy-authorization',
    'te', 'trailers', 'transfer-encoding', 'upgrade',
))


def is_hop_by_hop(header_name):
    """True for headers that apply to one connection only (RFC 2616)."""
    return header_name.lower() in _HOP_BY_HOP
