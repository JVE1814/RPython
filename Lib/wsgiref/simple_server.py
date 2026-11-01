"""A simple single-threaded HTTP server for WSGI applications.

    httpd = make_server('', 8000, app)
    httpd.serve_forever()

Not meant for production use.
"""

import sys
import urllib.parse
from http.server import BaseHTTPRequestHandler, HTTPServer
from platform import python_implementation

from wsgiref.handlers import SimpleHandler

__all__ = ['WSGIServer', 'WSGIRequestHandler', 'demo_app', 'make_server']

_VERSION = '0.2'


def __getattr__(name):
    if name == '__version__':
        import warnings
        warnings._deprecated('__version__', remove=(3, 20))
        return _VERSION
    raise AttributeError(f'module {__name__!r} has no attribute {name!r}')


server_version = 'WSGIServer'
sys_version = python_implementation() + '/' + sys.version.split()[0]
software_version = server_version + ' ' + sys_version


class ServerHandler(SimpleHandler):

    server_software = software_version

    def close(self):
        try:
            self.request_handler.log_request(
                self.status.split(' ', 1)[0], self.bytes_sent)
        finally:
            SimpleHandler.close(self)


class WSGIServer(HTTPServer):
    """An HTTPServer that runs a WSGI application."""

    application = None

    def server_bind(self):
        HTTPServer.server_bind(self)
        self.setup_environ()

    def setup_environ(self):
        env = self.base_environ = {}
        env['SERVER_NAME'] = self.server_name
        env['GATEWAY_INTERFACE'] = 'CGI/1.1'
        env['SERVER_PORT'] = str(self.server_port)
        env['REMOTE_HOST'] = ''
        env['CONTENT_LENGTH'] = ''
        env['SCRIPT_NAME'] = ''

    def get_app(self):
        return self.application

    def set_app(self, application):
        self.application = application


class WSGIRequestHandler(BaseHTTPRequestHandler):

    server_version = 'WSGIServer/' + _VERSION

    def get_environ(self):
        """The WSGI environment for the current request."""
        env = self.server.base_environ.copy()
        env['SERVER_PROTOCOL'] = self.request_version
        env['SERVER_SOFTWARE'] = self.server_version
        env['REQUEST_METHOD'] = self.command

        path, _, query = self.path.partition('?')
        env['PATH_INFO'] = urllib.parse.unquote(path, 'iso-8859-1')
        env['QUERY_STRING'] = query

        host = self.address_string()
        if host != self.client_address[0]:
            env['REMOTE_HOST'] = host
        env['REMOTE_ADDR'] = self.client_address[0]

        if self.headers.get('content-type') is None:
            env['CONTENT_TYPE'] = self.headers.get_content_type()
        else:
            env['CONTENT_TYPE'] = self.headers['content-type']

        length = self.headers.get('content-length')
        if length:
            env['CONTENT_LENGTH'] = length

        for name, value in self.headers.items():
            key = name.replace('-', '_').upper()
            value = value.strip()
            if key in env:
                continue        # CONTENT_TYPE / CONTENT_LENGTH
            if 'HTTP_' + key in env:
                env['HTTP_' + key] += ',' + value
            else:
                env['HTTP_' + key] = value
        return env

    def get_stderr(self):
        return sys.stderr

    def handle(self):
        """Handle one request with the server's WSGI application."""
        self.raw_requestline = self.rfile.readline(65537)
        if len(self.raw_requestline) > 65536:
            self.requestline = ''
            self.request_version = ''
            self.command = ''
            self.send_error(414)
            return

        if not self.parse_request():
            return

        handler = ServerHandler(self.rfile, self.wfile, self.get_stderr(),
                                self.get_environ(), multithread=False)
        handler.request_handler = self
        handler.run(self.server.get_app())


def demo_app(environ, start_response):
    """A WSGI application that shows its environment."""
    from io import StringIO
    out = StringIO()
    print('Hello world!', file=out)
    print(file=out)
    for key, value in sorted(environ.items()):
        print(key, '=', repr(value), file=out)
    start_response('200 OK', [('Content-Type', 'text/plain; charset=utf-8')])
    return [out.getvalue().encode('utf-8')]


def make_server(host, port, app, server_class=WSGIServer,
                handler_class=WSGIRequestHandler):
    """Create a server for *app* on *host*:*port*."""
    server = server_class((host, port), handler_class)
    server.set_app(app)
    return server


if __name__ == '__main__':
    with make_server('', 8000, demo_app) as httpd:
        sa = httpd.socket.getsockname()
        print('Serving HTTP on', sa[0], 'port', sa[1], '...')
        import webbrowser
        webbrowser.open('http://localhost:8000/xyz?abc')
        httpd.handle_request()
