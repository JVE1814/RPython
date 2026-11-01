"""Base classes for running WSGI applications under a server or CGI."""

import os
import sys
import time

from .headers import Headers
from .util import FileWrapper, guess_scheme, is_hop_by_hop

__all__ = [
    'BaseHandler', 'SimpleHandler', 'BaseCGIHandler', 'CGIHandler',
    'IISCGIHandler', 'read_environ',
]

_WEEKDAYS = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun']
_MONTHS = [None, 'Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun',
           'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec']


def format_date_time(timestamp):
    """An HTTP date for *timestamp*."""
    tm = time.gmtime(timestamp)
    return '%s, %02d %3s %4d %02d:%02d:%02d GMT' % (
        _WEEKDAYS[tm.tm_wday], tm.tm_mday, _MONTHS[tm.tm_mon], tm.tm_year,
        tm.tm_hour, tm.tm_min, tm.tm_sec)


_REQUEST_VARS = frozenset((
    'SCRIPT_NAME', 'PATH_INFO', 'QUERY_STRING', 'REQUEST_METHOD',
    'AUTH_TYPE', 'CONTENT_TYPE', 'CONTENT_LENGTH', 'HTTPS', 'REMOTE_USER',
    'REMOTE_IDENT',
))


def _needs_transcode(key):
    if key in _REQUEST_VARS or key.startswith(('HTTP_', 'SSL_')):
        return True
    return key.startswith('REDIRECT_') and _needs_transcode(key[9:])


def read_environ():
    """os.environ, with the request variables re-decoded as latin-1 bytes
    (PEP 3333 "bytes as str")."""
    enc = sys.getfilesystemencoding()
    environ = {}
    for key, value in os.environ.items():
        if _needs_transcode(key) and sys.platform != 'win32':
            value = value.encode(enc, 'surrogateescape').decode('iso-8859-1')
        environ[key] = value
    return environ


_C0 = frozenset(chr(c) for c in range(0x20)) | {'\x7f'}


class BaseHandler:
    """Run a WSGI application and send its response.

    Subclasses provide the I/O: _write(), _flush(), get_stdin(),
    get_stderr() and add_cgi_vars().
    """

    wsgi_version = (1, 0)
    wsgi_multithread = True
    wsgi_multiprocess = True
    wsgi_run_once = False

    origin_server = True        # send a status line (not a CGI Status:)
    http_version = '1.0'
    server_software = None

    os_environ = read_environ()

    wsgi_file_wrapper = FileWrapper
    headers_class = Headers

    traceback_limit = None
    error_status = '500 Internal Server Error'
    error_headers = [('Content-Type', 'text/plain')]
    error_body = b'A server error occurred.  Please contact the administrator.'

    status = result = None
    headers_sent = False
    headers = None
    bytes_sent = 0

    def run(self, application):
        """Run *application* and send its response."""
        try:
            self.setup_environ()
            self.result = application(self.environ, self.start_response)
            self.finish_response()
        except (ConnectionAbortedError, BrokenPipeError,
                ConnectionResetError):
            # the client went away; nothing more can be sent
            return
        except:
            try:
                self.handle_error()
            except:
                self.close()
                raise

    def setup_environ(self):
        env = self.environ = self.os_environ.copy()
        self.add_cgi_vars()

        env['wsgi.input'] = self.get_stdin()
        env['wsgi.errors'] = self.get_stderr()
        env['wsgi.version'] = self.wsgi_version
        env['wsgi.run_once'] = self.wsgi_run_once
        env['wsgi.url_scheme'] = self.get_scheme()
        env['wsgi.multithread'] = self.wsgi_multithread
        env['wsgi.multiprocess'] = self.wsgi_multiprocess

        if self.wsgi_file_wrapper is not None:
            env['wsgi.file_wrapper'] = self.wsgi_file_wrapper

        if self.origin_server and self.server_software:
            env.setdefault('SERVER_SOFTWARE', self.server_software)

    def finish_response(self):
        """Send the body (after the headers) and close the result."""
        try:
            if not self.result_is_file() or not self.sendfile():
                for data in self.result:
                    self.write(data)
                self.finish_content()
        except:
            if hasattr(self.result, 'close'):
                self.result.close()
            raise
        else:
            self.close()

    def get_scheme(self):
        return guess_scheme(self.environ)

    def set_content_length(self):
        """Add Content-Length for single-block results."""
        try:
            blocks = len(self.result)
        except (TypeError, AttributeError, NotImplementedError):
            return
        if blocks == 1:
            self.headers['Content-Length'] = str(self.bytes_sent)

    def cleanup_headers(self):
        """Last changes to the headers before they are sent."""
        if 'Content-Length' not in self.headers:
            self.set_content_length()

    def _convert_string_type(self, value, title):
        if type(value) is str:
            return value
        raise AssertionError(f'{title} must be of type str (got {value!r})')

    def _validate_status(self, status):
        if not _C0.isdisjoint(status):
            raise ValueError('Control characters are not allowed in status')
        if len(status) < 4:
            raise AssertionError('Status must be at least 4 characters')
        if not status[:3].isdigit():
            raise AssertionError('Status message must begin w/3-digit code')
        if status[3] != ' ':
            raise AssertionError('Status message must have a space after '
                                 'code')

    def start_response(self, status, headers, exc_info=None):
        """The start_response() callable passed to the application."""
        if exc_info:
            try:
                if self.headers_sent:
                    raise exc_info[1].with_traceback(exc_info[2])
            finally:
                exc_info = None
        elif self.headers is not None:
            raise AssertionError('Headers already set!')

        status = self._convert_string_type(status, 'Status')
        self._validate_status(status)
        self.status = status
        self.headers = self.headers_class(headers)
        if __debug__:
            for name, value in headers:
                name = self._convert_string_type(name, 'Header name')
                value = self._convert_string_type(value, 'Header value')
                assert not is_hop_by_hop(name), \
                    f"Hop-by-hop header, '{name}: {value}', not allowed"
        return self.write

    def send_preamble(self):
        """Send the status line (or CGI Status header) and the Date and
        Server headers."""
        if self.origin_server:
            if self.client_is_modern():
                self._write(f'HTTP/{self.http_version} {self.status}\r\n'
                            .encode('iso-8859-1'))
                if 'Date' not in self.headers:
                    self._write(f'Date: {format_date_time(time.time())}\r\n'
                                .encode('iso-8859-1'))
                if self.server_software and 'Server' not in self.headers:
                    self._write(f'Server: {self.server_software}\r\n'
                                .encode('iso-8859-1'))
        else:
            self._write(f'Status: {self.status}\r\n'.encode('iso-8859-1'))

    def write(self, data):
        """The write() callable returned by start_response()."""
        assert type(data) is bytes, 'write() argument must be a bytes instance'
        if not self.status:
            raise AssertionError('write() before start_response()')
        if not self.headers_sent:
            self.bytes_sent = len(data)
            self.send_headers()
        else:
            self.bytes_sent += len(data)
        self._write(data)
        self._flush()

    def sendfile(self):
        """Send a FileWrapper result efficiently; return False to fall
        back to iterating over it."""
        return False

    def finish_content(self):
        if not self.headers_sent:
            self.headers.setdefault('Content-Length', '0')
            self.send_headers()

    def close(self):
        """Close the result and reset the per-request state."""
        try:
            if hasattr(self.result, 'close'):
                self.result.close()
        finally:
            self.result = self.headers = self.status = self.environ = None
            self.bytes_sent = 0
            self.headers_sent = False

    def send_headers(self):
        self.cleanup_headers()
        self.headers_sent = True
        if not self.origin_server or self.client_is_modern():
            self.send_preamble()
            self._write(bytes(self.headers))

    def result_is_file(self):
        wrapper = self.wsgi_file_wrapper
        return wrapper is not None and isinstance(self.result, wrapper)

    def client_is_modern(self):
        """False for HTTP/0.9 clients, which get no headers."""
        return self.environ['SERVER_PROTOCOL'].upper() != 'HTTP/0.9'

    def log_exception(self, exc_info):
        """Print the traceback of *exc_info* to the error stream."""
        try:
            from traceback import print_exception
            stderr = self.get_stderr()
            print_exception(exc_info[0], exc_info[1], exc_info[2],
                            self.traceback_limit, stderr)
            stderr.flush()
        finally:
            exc_info = None

    def handle_error(self):
        """Log the current exception and send an error page if nothing has
        been sent yet."""
        self.log_exception(sys.exc_info())
        if not self.headers_sent:
            self.result = self.error_output(self.environ,
                                            self.start_response)
            self.finish_response()

    def error_output(self, environ, start_response):
        """A WSGI application producing the error page."""
        start_response(self.error_status, self.error_headers[:],
                       sys.exc_info())
        return [self.error_body]

    # I/O, provided by subclasses

    def _write(self, data):
        raise NotImplementedError

    def _flush(self):
        raise NotImplementedError

    def get_stdin(self):
        raise NotImplementedError

    def get_stderr(self):
        raise NotImplementedError

    def add_cgi_vars(self):
        raise NotImplementedError


class SimpleHandler(BaseHandler):
    """A handler using given streams and a given base environment."""

    def __init__(self, stdin, stdout, stderr, environ,
                 multithread=True, multiprocess=False):
        self.stdin = stdin
        self.stdout = stdout
        self.stderr = stderr
        self.base_env = environ
        self.wsgi_multithread = multithread
        self.wsgi_multiprocess = multiprocess

    def get_stdin(self):
        return self.stdin

    def get_stderr(self):
        return self.stderr

    def add_cgi_vars(self):
        self.environ.update(self.base_env)

    def _write(self, data):
        written = self.stdout.write(data)
        if written is None or written == len(data):
            return
        from warnings import warn
        warn('SimpleHandler.stdout.write() should not do partial writes',
             DeprecationWarning)
        while True:
            data = data[written:]
            if not data:
                break
            written = self.stdout.write(data)

    def _flush(self):
        self.stdout.flush()
        self._flush = self.stdout.flush


class BaseCGIHandler(SimpleHandler):
    """A SimpleHandler for CGI-like protocols: no status line, but a
    Status: header."""

    origin_server = False


class CGIHandler(BaseCGIHandler):
    """Run an application as a CGI script (stdin/stdout/environment)."""

    wsgi_run_once = True
    os_environ = {}

    def __init__(self):
        BaseCGIHandler.__init__(
            self, sys.stdin.buffer, sys.stdout.buffer, sys.stderr,
            read_environ(), multithread=False, multiprocess=True)


class IISCGIHandler(BaseCGIHandler):
    """CGIHandler for IIS, which puts SCRIPT_NAME in front of PATH_INFO."""

    wsgi_run_once = True
    os_environ = {}

    def __init__(self):
        environ = read_environ()
        path = environ.get('PATH_INFO', '')
        script = environ.get('SCRIPT_NAME', '')
        if (path + '/').startswith(script + '/'):
            environ['PATH_INFO'] = path[len(script):]
        BaseCGIHandler.__init__(
            self, sys.stdin.buffer, sys.stdout.buffer, sys.stderr,
            environ, multithread=False, multiprocess=True)
