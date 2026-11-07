"""HTTP servers.

HTTPServer (and its threading/TLS variants) accept connections and hand
each one to a request handler class.  BaseHTTPRequestHandler parses the
request line and headers and dispatches to do_<METHOD>() methods;
SimpleHTTPRequestHandler serves files and directory listings from a
directory.  ``python -m http.server`` runs a file server.
"""

_VERSION = '0.6'


def __getattr__(name):
    if name == '__version__':
        import warnings
        warnings._deprecated('__version__', remove=(3, 20))
        return _VERSION
    raise AttributeError(f'module {__name__!r} has no attribute {name!r}')

__all__ = [
    'HTTPServer', 'ThreadingHTTPServer',
    'HTTPSServer', 'ThreadingHTTPSServer',
    'BaseHTTPRequestHandler', 'SimpleHTTPRequestHandler',
]

import contextlib
import html
import http.client
import io
import itertools
import mimetypes
import os
import posixpath
import shutil
import socket
import socketserver
import sys
import time
import urllib.parse

from http import HTTPStatus


class HTTPServer(socketserver.TCPServer):

    allow_reuse_address = True
    allow_reuse_port = False

    def server_bind(self):
        """Bind the socket and remember the server's name and port."""
        socketserver.TCPServer.server_bind(self)
        host, port = self.server_address[:2]
        self.server_name = socket.getfqdn(host)
        self.server_port = port


class ThreadingHTTPServer(socketserver.ThreadingMixIn, HTTPServer):
    daemon_threads = True


class HTTPSServer(HTTPServer):
    """An HTTPServer that speaks TLS (certificate given at creation)."""

    def __init__(self, server_address, RequestHandlerClass,
                 bind_and_activate=True, *, certfile, keyfile=None,
                 password=None, alpn_protocols=None):
        try:
            import ssl
        except ImportError:
            raise RuntimeError('SSL module is missing; '
                               'HTTPS support is unavailable')
        self.ssl = ssl
        self.certfile = certfile
        self.keyfile = keyfile
        self.password = password
        if alpn_protocols is None:
            alpn_protocols = ['http/1.1']
        self.alpn_protocols = alpn_protocols
        super().__init__(server_address, RequestHandlerClass,
                         bind_and_activate)

    def server_activate(self):
        context = self._create_context()
        self.socket = context.wrap_socket(self.socket, server_side=True)
        super().server_activate()

    def _create_context(self):
        context = self.ssl.create_default_context(
            self.ssl.Purpose.CLIENT_AUTH)
        context.load_cert_chain(self.certfile, self.keyfile, self.password)
        context.set_alpn_protocols(self.alpn_protocols)
        return context


class ThreadingHTTPSServer(socketserver.ThreadingMixIn, HTTPSServer):
    daemon_threads = True


# Default error page; %(code)d, %(message)s and %(explain)s are filled in
# (message and explain are HTML-escaped).
DEFAULT_ERROR_MESSAGE = """\
<!DOCTYPE HTML>
<html lang="en">
    <head>
        <meta charset="utf-8">
        <title>Error %(code)d</title>
    </head>
    <body>
        <h1>Error %(code)d: %(message)s</h1>
        <p>%(explain)s</p>
    </body>
</html>
"""

DEFAULT_ERROR_CONTENT_TYPE = 'text/html;charset=utf-8'


def _make_log_table():
    table = {c: f'\\x{c:02x}' for c in itertools.chain(range(0x20),
                                                       range(0x7f, 0xa0))}
    table[ord('\\')] = '\\\\'
    return table


class _Trusted(str):
    """A log message fragment that is already escaped (and may contain
    terminal colour codes)."""


class BaseHTTPRequestHandler(socketserver.StreamRequestHandler):
    """Parse HTTP requests and dispatch them to do_<COMMAND>() methods.

    After parse_request() the attributes command, path, request_version
    and headers (an http.client.HTTPMessage) describe the request; rfile
    is positioned at the start of the body.  Responses are written with
    send_response(), send_header(), end_headers() and wfile.
    """

    sys_version = 'Python/' + sys.version.split()[0]
    server_version = 'BaseHTTP/' + _VERSION

    error_message_format = DEFAULT_ERROR_MESSAGE
    error_content_type = DEFAULT_ERROR_CONTENT_TYPE

    # Version assumed for requests without a version (HTTP/0.9 style).
    default_request_version = 'HTTP/0.9'

    # Persistent connections are only offered with HTTP/1.1.
    protocol_version = 'HTTP/1.0'

    MessageClass = http.client.HTTPMessage

    responses = {status: (status.phrase, status.description)
                 for status in HTTPStatus.__members__.values()}

    weekdayname = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun']
    monthname = [None, 'Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun',
                 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec']

    _log_escape_table = _make_log_table()

    def parse_request(self):
        """Parse self.raw_requestline and the headers.

        Returns False (after sending an error response where appropriate)
        if the request cannot be served.
        """
        self.command = None
        self.request_version = version = self.default_request_version
        self.close_connection = True
        requestline = str(self.raw_requestline, 'iso-8859-1')
        requestline = requestline.rstrip('\r\n')
        self.requestline = requestline
        words = requestline.split()
        if not words:
            return False

        if len(words) >= 3:
            version = words[-1]
            try:
                if not version.startswith('HTTP/'):
                    raise ValueError
                number = version.split('/', 1)[1]
                parts = number.split('.')
                if len(parts) != 2:
                    raise ValueError
                if not all(p.isascii() and p.isdigit() for p in parts):
                    raise ValueError('non digit in http version')
                if any(len(p) > 10 for p in parts):
                    raise ValueError('unreasonable length http version')
                vnum = int(parts[0]), int(parts[1])
            except (ValueError, IndexError):
                self.send_error(HTTPStatus.BAD_REQUEST,
                                f'Bad request version ({version!r})')
                return False
            if vnum >= (1, 1) and self.protocol_version >= 'HTTP/1.1':
                self.close_connection = False
            if vnum >= (2, 0):
                self.send_error(HTTPStatus.HTTP_VERSION_NOT_SUPPORTED,
                                f'Invalid HTTP version ({number})')
                return False
            self.request_version = version

        if not 2 <= len(words) <= 3:
            self.send_error(HTTPStatus.BAD_REQUEST,
                            f'Bad request syntax ({requestline!r})')
            return False
        command, path = words[:2]
        if len(words) == 2:
            self.close_connection = True
            if command != 'GET':
                self.send_error(HTTPStatus.BAD_REQUEST,
                                f'Bad HTTP/0.9 request type ({command!r})')
                return False
        self.command, self.path = command, path

        # A path starting with '//' could be taken for a network location
        # when it is reused in a URL (e.g. in a Location header).
        if self.path.startswith('//'):
            self.path = '/' + self.path.lstrip('/')

        if len(words) == 2:
            # HTTP/0.9 requests have no headers
            self.headers = self.MessageClass()
            return True

        try:
            self.headers = http.client.parse_headers(self.rfile,
                                                     _class=self.MessageClass)
        except http.client.LineTooLong as err:
            self.send_error(HTTPStatus.REQUEST_HEADER_FIELDS_TOO_LARGE,
                            'Line too long', str(err))
            return False
        except http.client.HTTPException as err:
            self.send_error(HTTPStatus.REQUEST_HEADER_FIELDS_TOO_LARGE,
                            'Too many headers', str(err))
            return False

        conntype = self.headers.get('Connection', '').lower()
        if conntype == 'close':
            self.close_connection = True
        elif conntype == 'keep-alive' and self.protocol_version >= 'HTTP/1.1':
            self.close_connection = False

        expect = self.headers.get('Expect', '')
        if (expect.lower() == '100-continue' and
                self.protocol_version >= 'HTTP/1.1' and
                self.request_version >= 'HTTP/1.1'):
            if not self.handle_expect_100():
                return False
        return True

    def handle_expect_100(self):
        """Answer "Expect: 100-continue"; return False to reject the
        request (after sending an error)."""
        self.send_response_only(HTTPStatus.CONTINUE)
        self.end_headers()
        return True

    def handle_one_request(self):
        """Read and handle one request."""
        try:
            self.raw_requestline = self.rfile.readline(65537)
            if len(self.raw_requestline) > 65536:
                self.requestline = ''
                self.request_version = ''
                self.command = ''
                self.send_error(HTTPStatus.REQUEST_URI_TOO_LONG)
                return
            if not self.raw_requestline:
                self.close_connection = True
                return
            if not self.parse_request():
                return
            mname = 'do_' + self.command
            if not hasattr(self, mname):
                self.send_error(HTTPStatus.NOT_IMPLEMENTED,
                                f'Unsupported method ({self.command!r})')
                return
            getattr(self, mname)()
            self.wfile.flush()
        except TimeoutError as exc:
            self.log_error('Request timed out: %r', exc)
            self.close_connection = True

    def handle(self):
        """Handle requests until the connection is to be closed."""
        self.close_connection = True
        self.handle_one_request()
        while not self.close_connection:
            self.handle_one_request()

    def send_error(self, code, message=None, explain=None):
        """Send an error response with an HTML body (unless the status
        forbids a body) and close the connection."""
        try:
            short, long = self.responses[code]
        except KeyError:
            short, long = '???', '???'
        if message is None:
            message = short
        if explain is None:
            explain = long
        self.log_error('code %d, message %s', code, message)
        # Error responses always get a status line, even for requests in
        # HTTP/0.9 style.
        if getattr(self, 'request_version', None) == 'HTTP/0.9':
            self.request_version = self.protocol_version
        self.send_response(code, message)
        self.send_header('Connection', 'close')

        body = None
        if code >= 200 and code not in (HTTPStatus.NO_CONTENT,
                                        HTTPStatus.RESET_CONTENT,
                                        HTTPStatus.NOT_MODIFIED):
            content = self.error_message_format % {
                'code': code,
                'message': html.escape(message, quote=False),
                'explain': html.escape(explain, quote=False),
            }
            body = content.encode('UTF-8', 'replace')
            self.send_header('Content-Type', self.error_content_type)
            self.send_header('Content-Length', str(len(body)))
        self.end_headers()

        if self.command != 'HEAD' and body:
            self.wfile.write(body)

    def send_response(self, code, message=None):
        """Log the request and queue the status line plus the Server and
        Date headers."""
        self.log_request(code)
        self.send_response_only(code, message)
        self.send_header('Server', self.version_string())
        self.send_header('Date', self.date_time_string())

    def send_response_only(self, code, message=None):
        """Queue the status line only."""
        if self.request_version != 'HTTP/0.9':
            if message is None:
                message = self.responses[code][0] if code in self.responses \
                    else ''
            if not hasattr(self, '_headers_buffer'):
                self._headers_buffer = []
            self._headers_buffer.append(
                f'{self.protocol_version} {code:d} {message}\r\n'
                .encode('latin-1', 'strict'))

    def send_header(self, keyword, value):
        """Queue a header line."""
        if self.request_version != 'HTTP/0.9':
            if not hasattr(self, '_headers_buffer'):
                self._headers_buffer = []
            self._headers_buffer.append(
                f'{keyword}: {value}\r\n'.encode('latin-1', 'strict'))

        if keyword.lower() == 'connection':
            if value.lower() == 'close':
                self.close_connection = True
            elif value.lower() == 'keep-alive':
                self.close_connection = False

    def end_headers(self):
        """Finish the headers and write them out."""
        if self.request_version != 'HTTP/0.9':
            self._headers_buffer.append(b'\r\n')
            self.flush_headers()

    def flush_headers(self):
        if hasattr(self, '_headers_buffer'):
            self.wfile.write(b''.join(self._headers_buffer))
            self._headers_buffer = []

    # -- logging ----------------------------------------------------------

    def _theme(self):
        from _colorize import get_theme
        return get_theme(tty_file=sys.stderr).http_server

    def _escape(self, text):
        return str(text).translate(self._log_escape_table)

    def log_request(self, code='-', size='-'):
        """Log an accepted request (called by send_response())."""
        if isinstance(code, HTTPStatus):
            code = code.value
        theme = self._theme()
        status_color = ''
        if isinstance(code, int):
            if code < 200:
                status_color = theme.status_informational
            elif code < 300:
                status_color = theme.status_ok
            elif code < 400:
                status_color = theme.status_redirect
            elif code < 500:
                status_color = theme.status_client_error
            else:
                status_color = theme.status_server_error
        line = getattr(self, 'requestline', '')
        words = line.split(' ')
        if len(words) == 3 and theme.path:
            line = (f'{self._escape(words[0])} {theme.path}'
                    f'{self._escape(words[1])}{theme.reset} '
                    f'{self._escape(words[2])}')
        else:
            line = self._escape(line)
        reset = theme.reset if status_color else ''
        self.log_message('"%s" %s %s', _Trusted(line),
                         _Trusted(f'{status_color}{self._escape(code)}{reset}'),
                         _Trusted(self._escape(size)))

    def log_error(self, format, *args):
        """Log an error; by default the same as log_message()."""
        theme = self._theme()
        if theme.error:
            text = self._escape(format % args)
            self.log_message('%s', _Trusted(f'{theme.error}{text}'
                                            f'{theme.reset}'))
        else:
            self.log_message(format, *args)

    def log_message(self, format, *args):
        """Write a log line to stderr: client address, time and the
        message (non-printable characters are escaped)."""
        safe = tuple(arg if isinstance(arg, _Trusted)
                     else self._escape(arg) if isinstance(arg, str) else arg
                     for arg in args)
        message = self._escape(format) % safe
        sys.stderr.write(f'{self.address_string()} - - '
                         f'[{self.log_date_time_string()}] {message}\n')

    def version_string(self):
        """The value of the Server header."""
        return self.server_version + ' ' + self.sys_version

    def date_time_string(self, timestamp=None):
        """An HTTP date (RFC 9110) for *timestamp* (default: now)."""
        if timestamp is None:
            timestamp = time.time()
        tm = time.gmtime(timestamp)
        return '%s, %02d %3s %4d %02d:%02d:%02d GMT' % (
            self.weekdayname[tm.tm_wday], tm.tm_mday,
            self.monthname[tm.tm_mon], tm.tm_year,
            tm.tm_hour, tm.tm_min, tm.tm_sec)

    def log_date_time_string(self):
        """The current local time, formatted for logging."""
        tm = time.localtime(time.time())
        return '%02d/%3s/%04d %02d:%02d:%02d' % (
            tm.tm_mday, self.monthname[tm.tm_mon], tm.tm_year,
            tm.tm_hour, tm.tm_min, tm.tm_sec)

    def address_string(self):
        """The client's address (no reverse DNS lookup)."""
        return self.client_address[0]


# Response headers that may legitimately occur more than once; extra
# response headers with these names are added even if already present.
_REPEATABLE_HEADERS = frozenset(('set-cookie', 'link', 'warning', 'via',
                                 'www-authenticate'))


def _days_from_civil(year, month, day):
    """Days since 1970-01-01 of a proleptic Gregorian date."""
    year -= month <= 2
    era = year // 400
    yoe = year - era * 400
    doy = (153 * (month + (-3 if month > 2 else 9)) + 2) // 5 + day - 1
    doe = yoe * 365 + yoe // 4 - yoe // 100 + doy
    return era * 146097 + doe - 719468


def _parse_http_date(text):
    """Seconds since the epoch for an HTTP date in UTC/GMT, else None."""
    try:
        import email.utils
        parsed = email.utils.parsedate_tz(text)
    except (TypeError, IndexError, OverflowError, ValueError):
        return None
    if parsed is None or parsed[9] not in (None, 0):
        return None
    year, month, day, hour, minute, second = parsed[:6]
    return _days_from_civil(year, month, day) * 86400 + \
        hour * 3600 + minute * 60 + second


class SimpleHTTPRequestHandler(BaseHTTPRequestHandler):
    """Serve GET and HEAD requests from a directory (the current directory
    by default): files as they are, directories as an HTML listing (or
    their index.html)."""

    server_version = 'SimpleHTTP/' + _VERSION
    index_pages = ('index.html', 'index.htm')
    extensions_map = _encodings_map_default = {
        '.gz': 'application/gzip',
        '.Z': 'application/octet-stream',
        '.bz2': 'application/x-bzip2',
        '.xz': 'application/x-xz',
    }
    default_content_type = 'application/octet-stream'

    def __init__(self, *args, directory=None, extra_response_headers=None,
                 **kwargs):
        if directory is None:
            directory = os.getcwd()
        self.directory = os.fspath(directory)
        self.extra_response_headers = extra_response_headers
        super().__init__(*args, **kwargs)

    def do_GET(self):
        f = self.send_head()
        if f:
            try:
                self.copyfile(f, self.wfile)
            finally:
                f.close()

    def do_HEAD(self):
        f = self.send_head()
        if f:
            f.close()

    def _send_extra_response_headers(self):
        extra = self.extra_response_headers
        if not extra:
            return
        sent = set()
        for line in getattr(self, '_headers_buffer', ()):
            name = line.split(b':', 1)[0].decode('latin-1').lower()
            sent.add(name)
        for name, value in extra:
            lower = name.lower()
            if lower in sent and lower not in _REPEATABLE_HEADERS:
                continue
            self.send_header(name, value)

    def send_head(self):
        """Send the status and headers for the requested path.

        Returns a file object positioned at the body (to be copied to
        wfile and closed by the caller), or None.
        """
        path = self.translate_path(self.path)
        f = None
        if os.path.isdir(path):
            parts = urllib.parse.urlsplit(self.path)
            if not parts.path.endswith(('/', '%2f', '%2F')):
                # redirect the browser to the directory with a slash
                self.send_response(HTTPStatus.MOVED_PERMANENTLY)
                new_url = urllib.parse.urlunsplit(
                    (parts[0], parts[1], parts[2] + '/', parts[3], parts[4]))
                self.send_header('Location', new_url)
                self.send_header('Content-Length', '0')
                self.end_headers()
                return None
            for index in self.index_pages:
                index = os.path.join(path, index)
                if os.path.isfile(index):
                    path = index
                    break
            else:
                return self.list_directory(path)

        ctype = self.guess_type(path)
        # A trailing slash names a directory; a file with it is missing.
        if path.endswith('/'):
            self.send_error(HTTPStatus.NOT_FOUND, 'File not found')
            return None
        try:
            f = open(path, 'rb')
        except OSError:
            self.send_error(HTTPStatus.NOT_FOUND, 'File not found')
            return None

        try:
            fs = os.fstat(f.fileno())
            if ('If-Modified-Since' in self.headers and
                    'If-None-Match' not in self.headers):
                since = _parse_http_date(self.headers['If-Modified-Since'])
                if since is not None and int(fs.st_mtime) <= since:
                    self.send_response(HTTPStatus.NOT_MODIFIED)
                    self.end_headers()
                    f.close()
                    return None

            self.send_response(HTTPStatus.OK)
            self.send_header('Content-type', ctype)
            self.send_header('Content-Length', str(fs.st_size))
            self.send_header('Last-Modified',
                             self.date_time_string(fs.st_mtime))
            self._send_extra_response_headers()
            self.end_headers()
            return f
        except:
            f.close()
            raise

    def list_directory(self, path):
        """Send headers for an HTML listing of *path*; return the body as
        a file object (or None after an error)."""
        try:
            names = os.listdir(path)
        except OSError:
            self.send_error(HTTPStatus.NOT_FOUND,
                            'No permission to list directory')
            return None
        names.sort(key=lambda name: name.lower())

        displaypath = self.path.split('#', 1)[0].split('?', 1)[0]
        try:
            displaypath = urllib.parse.unquote(displaypath,
                                               errors='surrogatepass')
        except UnicodeDecodeError:
            displaypath = urllib.parse.unquote(displaypath)
        displaypath = html.escape(displaypath, quote=False)
        enc = sys.getfilesystemencoding()
        title = f'Directory listing for {displaypath}'

        lines = ['<!DOCTYPE HTML>',
                 '<html lang="en">',
                 '<head>',
                 f'<meta charset="{enc}">',
                 '<style type="text/css">\n:root {\ncolor-scheme: light dark;\n}\n</style>',
                 f'<title>{title}</title>',
                 '</head>',
                 '<body>',
                 f'<h1>{title}</h1>',
                 '<hr>',
                 '<ul>']
        for name in names:
            fullname = os.path.join(path, name)
            displayname = linkname = name
            if os.path.isdir(fullname):
                displayname = name + '/'
                linkname = name + '/'
            if os.path.islink(fullname):
                displayname = name + '@'
            href = urllib.parse.quote(linkname, errors='surrogatepass')
            lines.append(f'<li><a href="{href}">'
                         f'{html.escape(displayname, quote=False)}</a></li>')
        lines.extend(['</ul>', '<hr>', '</body>', '</html>', ''])
        encoded = '\n'.join(lines).encode(enc, 'surrogateescape')

        self.send_response(HTTPStatus.OK)
        self.send_header('Content-type', f'text/html; charset={enc}')
        self.send_header('Content-Length', str(len(encoded)))
        self._send_extra_response_headers()
        self.end_headers()
        return io.BytesIO(encoded)

    def translate_path(self, path):
        """Map a URL path to a file system path below self.directory.

        Query and fragment are dropped, '.' / '..' components and
        components with a directory part (such as drive letters) are
        ignored.
        """
        path = path.split('#', 1)[0].split('?', 1)[0]
        try:
            path = urllib.parse.unquote(path, errors='surrogatepass')
        except UnicodeDecodeError:
            path = urllib.parse.unquote(path)
        trailing_slash = path.rstrip().endswith('/')
        path = posixpath.normpath(path)
        result = self.directory
        for word in path.split('/'):
            if not word:
                continue
            if os.path.dirname(word) or word in (os.curdir, os.pardir):
                continue
            result = os.path.join(result, word)
        if trailing_slash:
            result += '/'
        return result

    def copyfile(self, source, outputfile):
        """Copy the file object *source* to *outputfile*."""
        shutil.copyfileobj(source, outputfile)

    def guess_type(self, path):
        """The Content-type to send for *path*."""
        base, ext = posixpath.splitext(path)
        if ext in self.extensions_map:
            return self.extensions_map[ext]
        ext = ext.lower()
        if ext in self.extensions_map:
            return self.extensions_map[ext]
        guess, _ = mimetypes.guess_file_type(path)
        if guess:
            return guess
        return self.default_content_type


def _pick_address_family(*address):
    infos = socket.getaddrinfo(*address, type=socket.SOCK_STREAM,
                               flags=socket.AI_PASSIVE)
    family, type, proto, canonname, sockaddr = next(iter(infos))
    return family, sockaddr


def test(HandlerClass=BaseHTTPRequestHandler,
         ServerClass=ThreadingHTTPServer,
         protocol='HTTP/1.0', port=8000, bind=None,
         tls_cert=None, tls_key=None, tls_password=None,
         content_type=None):
    """Run an HTTP server on *port* until interrupted."""
    ServerClass.address_family, addr = _pick_address_family(bind, port)
    HandlerClass.protocol_version = protocol
    if content_type is not None:
        HandlerClass.default_content_type = content_type

    if tls_cert:
        server = ServerClass(addr, HandlerClass, certfile=tls_cert,
                             keyfile=tls_key, password=tls_password)
    else:
        server = ServerClass(addr, HandlerClass)

    with server as httpd:
        host, port = httpd.socket.getsockname()[:2]
        url_host = f'[{host}]' if ':' in host else host
        scheme = 'HTTPS' if tls_cert else 'HTTP'
        print(f'Serving {scheme} on {host} port {port} '
              f'({scheme.lower()}://{url_host}:{port}/) ...')
        try:
            httpd.serve_forever()
        except KeyboardInterrupt:
            print('\nKeyboard interrupt received, exiting.')
            sys.exit(0)


def _main(args=None):
    import argparse

    parser = argparse.ArgumentParser(color=True)
    parser.add_argument('-b', '--bind', metavar='ADDRESS',
                        help='bind to this address '
                             '(default: all interfaces)')
    parser.add_argument('-d', '--directory', default=os.getcwd(),
                        help='serve this directory '
                             '(default: current directory)')
    parser.add_argument('-p', '--protocol', metavar='VERSION',
                        default='HTTP/1.0',
                        help='conform to this HTTP version '
                             '(default: %(default)s)')
    parser.add_argument('--tls-cert', metavar='PATH',
                        help='path to the TLS certificate chain file')
    parser.add_argument('--tls-key', metavar='PATH',
                        help='path to the TLS key file')
    parser.add_argument('--tls-password-file', metavar='PATH',
                        help='path to the password file for the TLS key')
    parser.add_argument('--content-type', metavar='TYPE',
                        default='application/octet-stream',
                        help='Content-Type for files of unknown type '
                             '(default: %(default)s)')
    parser.add_argument('-H', '--header', nargs=2, action='append',
                        metavar=('HEADER', 'VALUE'),
                        help='add a response header (may be repeated)')
    parser.add_argument('port', default=8000, type=int, nargs='?',
                        help='bind to this port (default: %(default)s)')
    args = parser.parse_args(args)

    if not args.tls_cert and args.tls_key:
        parser.error('--tls-key requires --tls-cert to be set')

    tls_key_password = None
    if args.tls_password_file:
        if not args.tls_cert:
            parser.error('--tls-password-file requires --tls-cert to be set')
        try:
            with open(args.tls_password_file, 'r', encoding='utf-8') as f:
                tls_key_password = f.read().strip()
        except OSError as exc:
            parser.error(f'Failed to read TLS password file: {exc}')

    class DualStackServerMixin:
        def server_bind(self):
            # accept IPv4 connections on an IPv6 socket where possible
            with contextlib.suppress(Exception):
                self.socket.setsockopt(socket.IPPROTO_IPV6,
                                       socket.IPV6_V6ONLY, 0)
            return super().server_bind()

        def finish_request(self, request, client_address):
            self.RequestHandlerClass(request, client_address, self,
                                     directory=args.directory,
                                     extra_response_headers=args.header)

    class HTTPDualStackServer(DualStackServerMixin, ThreadingHTTPServer):
        pass

    class HTTPSDualStackServer(DualStackServerMixin, ThreadingHTTPSServer):
        pass

    test(HandlerClass=SimpleHTTPRequestHandler,
         ServerClass=HTTPSDualStackServer if args.tls_cert
         else HTTPDualStackServer,
         port=args.port,
         bind=args.bind,
         protocol=args.protocol,
         tls_cert=args.tls_cert,
         tls_key=args.tls_key,
         tls_password=tls_key_password,
         content_type=args.content_type)


if __name__ == '__main__':
    _main()
