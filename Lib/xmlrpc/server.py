"""XML-RPC servers.

SimpleXMLRPCServer serves functions and/or an instance over HTTP,
MultiPathXMLRPCServer serves several dispatchers under different paths,
CGIXMLRPCRequestHandler answers a request in a CGI script, and the Doc*
variants also answer GET requests with HTML documentation of the
registered methods.

    server = SimpleXMLRPCServer(("localhost", 8000))
    server.register_function(pow)
    server.register_instance(MyFunctions())
    server.serve_forever()

An instance may define _dispatch(method, params) to take over all calls,
_listMethods() and _methodHelp(name) for introspection.
"""

from xmlrpc.client import Fault, dumps, loads, gzip_encode, gzip_decode
from http.server import BaseHTTPRequestHandler
from functools import partial
from inspect import signature
import html
import http.server
import socketserver
import sys
import os
import re
import traceback

try:
    import fcntl
except ImportError:
    fcntl = None


def resolve_dotted_attribute(obj, attr, allow_dotted_names=True):
    """Look up attr (dotted if allowed) on obj; names starting with an
    underscore are refused."""
    names = attr.split('.') if allow_dotted_names else [attr]
    for name in names:
        if name.startswith('_'):
            raise AttributeError('attempt to access private attribute "%s"'
                                 % name)
        obj = getattr(obj, name)
    return obj


def list_public_methods(obj):
    """Names of the public callable attributes of obj."""
    return [name for name in dir(obj)
            if not name.startswith('_') and callable(getattr(obj, name))]


def _method_doc(method):
    import inspect
    return (inspect.getdoc(method) or '').rstrip()


class SimpleXMLRPCDispatcher:
    """Registry of functions and an optional instance, with marshalled
    dispatch.  Mixed into the server classes."""

    def __init__(self, allow_none=False, encoding=None,
                 use_builtin_types=False):
        self.funcs = {}
        self.instance = None
        self.allow_none = allow_none
        self.encoding = encoding or 'utf-8'
        self.use_builtin_types = use_builtin_types

    def register_instance(self, instance, allow_dotted_names=False):
        """Serve the public methods of *instance* (replacing any earlier
        instance).  Functions registered by name take precedence.

        allow_dotted_names lets clients reach attributes of attributes;
        only enable it on a trusted network.
        """
        self.instance = instance
        self.allow_dotted_names = allow_dotted_names

    def register_function(self, function=None, name=None):
        """Serve *function* as *name* (default: its __name__).  Can be
        used as a decorator, with or without the name argument."""
        if function is None:
            return partial(self.register_function, name=name)
        if name is None:
            name = function.__name__
        self.funcs[name] = function
        return function

    def register_introspection_functions(self):
        """Serve system.listMethods, system.methodHelp and
        system.methodSignature."""
        self.funcs['system.listMethods'] = self.system_listMethods
        self.funcs['system.methodSignature'] = self.system_methodSignature
        self.funcs['system.methodHelp'] = self.system_methodHelp

    def register_multicall_functions(self):
        """Serve system.multicall."""
        self.funcs['system.multicall'] = self.system_multicall

    def _marshaled_dispatch(self, data, dispatch_method=None, path=None):
        """Decode a request, run it and return the encoded response.
        Errors are returned to the client as faults."""
        try:
            params, method = loads(data,
                                   use_builtin_types=self.use_builtin_types)
            if dispatch_method is not None:
                result = dispatch_method(method, params)
            else:
                result = self._dispatch(method, params)
            response = dumps((result,), methodresponse=1,
                             allow_none=self.allow_none,
                             encoding=self.encoding)
        except Fault as fault:
            response = dumps(fault, allow_none=self.allow_none,
                             encoding=self.encoding)
        except BaseException as exc:
            response = dumps(Fault(1, "%s:%s" % (type(exc), exc)),
                             allow_none=self.allow_none,
                             encoding=self.encoding)
        return response.encode(self.encoding, 'xmlcharrefreplace')

    def system_listMethods(self):
        """system.listMethods() => ['add', 'subtract', 'multiple']

        Returns a list of the methods supported by the server."""
        names = set(self.funcs)
        instance = self.instance
        if instance is not None:
            if hasattr(instance, '_listMethods'):
                names.update(instance._listMethods())
            elif not hasattr(instance, '_dispatch'):
                names.update(list_public_methods(instance))
        return sorted(names)

    def system_methodSignature(self, method_name):
        """system.methodSignature('add') => [double, int, int]

        Returns a list describing the signature of the method. In the
        above example, the add method takes two integers as arguments
        and returns a double result.

        This server does NOT support system.methodSignature."""
        return 'signatures not supported'

    def system_methodHelp(self, method_name):
        """system.methodHelp('add') => "Adds two integers together"

        Returns a string containing documentation for the specified method."""
        method = self.funcs.get(method_name)
        instance = self.instance
        if method is None and instance is not None:
            if hasattr(instance, '_methodHelp'):
                return instance._methodHelp(method_name)
            if not hasattr(instance, '_dispatch'):
                try:
                    method = resolve_dotted_attribute(
                        instance, method_name, self.allow_dotted_names)
                except AttributeError:
                    pass
        if method is None:
            return ''
        return _method_doc(method)

    def system_multicall(self, call_list):
        """system.multicall([{'methodName': 'add', 'params': [2, 2]}, ...])
        => [[4], ...]

        Runs several calls in one request; each result is a one-element
        list, or a fault struct."""
        results = []
        for call in call_list:
            try:
                value = self._dispatch(call['methodName'], call['params'])
            except Fault as fault:
                results.append({'faultCode': fault.faultCode,
                                'faultString': fault.faultString})
            except BaseException as exc:
                results.append({'faultCode': 1,
                                'faultString': "%s:%s" % (type(exc), exc)})
            else:
                results.append([value])
        return results

    def _find_method(self, method):
        """The callable serving *method*, or None."""
        if method in self.funcs:
            return self.funcs[method]
        instance = self.instance
        if instance is None or hasattr(instance, '_dispatch'):
            return None
        try:
            return resolve_dotted_attribute(instance, method,
                                            self.allow_dotted_names)
        except AttributeError:
            return None

    def _dispatch(self, method, params):
        """Call the function serving *method*.

        Registered functions come first; then the instance's _dispatch
        method, or a public method of the instance."""
        func = self._find_method(method)
        if func is not None:
            return func(*params)
        instance = self.instance
        if (method not in self.funcs and instance is not None
                and hasattr(instance, '_dispatch')):
            return instance._dispatch(method, params)
        raise Exception('method "%s" is not supported' % method)


class SimpleXMLRPCRequestHandler(BaseHTTPRequestHandler):
    """HTTP handler for XML-RPC POST requests."""

    # paths that accept requests (an empty sequence accepts any path)
    rpc_paths = ('/', '/RPC2', '/pydoc.css')

    # gzip responses larger than this (None: never)
    encode_threshold = 1400

    wbufsize = -1
    disable_nagle_algorithm = True

    _accept_encoding = re.compile(r"""
        \s* ([^\s;]+) \s*            # content coding
        (;\s* q \s*=\s* ([0-9.]+))?  # optional quality value
        """, re.VERBOSE | re.IGNORECASE)

    def accept_encodings(self):
        """Content codings the client accepts: {coding: quality}."""
        result = {}
        for part in self.headers.get("Accept-Encoding", "").split(","):
            m = self._accept_encoding.match(part)
            if m:
                quality = float(m.group(3)) if m.group(3) else 1.0
                result[m.group(1)] = quality
        return result

    def is_rpc_path_valid(self):
        if self.rpc_paths:
            return self.path in self.rpc_paths
        return True

    def do_POST(self):
        """Run the XML-RPC request in the body and send the response."""
        if not self.is_rpc_path_valid():
            self.report_404()
            return
        try:
            # read in limited chunks so a huge length does not allocate
            # everything at once
            remaining = int(self.headers["content-length"])
            chunks = []
            while remaining:
                chunk = self.rfile.read(min(remaining, 10 * 1024 * 1024))
                if not chunk:
                    break
                chunks.append(chunk)
                remaining -= len(chunk)
            data = self.decode_request_content(b''.join(chunks))
            if data is None:
                return
            response = self.server._marshaled_dispatch(
                data, getattr(self, '_dispatch', None), self.path)
        except Exception as exc:
            self.send_response(500)
            if getattr(self.server, '_send_traceback_header', False):
                self.send_header("X-exception", str(exc))
                trace = traceback.format_exc()
                trace = str(trace.encode('ASCII', 'backslashreplace'),
                            'ASCII')
                self.send_header("X-traceback", trace)
            self.send_header("Content-length", "0")
            self.end_headers()
        else:
            self.send_response(200)
            self.send_header("Content-type", "text/xml")
            if self.encode_threshold is not None:
                if len(response) > self.encode_threshold:
                    quality = self.accept_encodings().get("gzip", 0)
                    if quality:
                        try:
                            response = gzip_encode(response)
                            self.send_header("Content-Encoding", "gzip")
                        except NotImplementedError:
                            pass
            self.send_header("Content-length", str(len(response)))
            self.end_headers()
            self.wfile.write(response)

    def decode_request_content(self, data):
        """Undo the request's Content-Encoding; None after an error reply."""
        encoding = self.headers.get("content-encoding", "identity").lower()
        if encoding == "identity":
            return data
        if encoding == "gzip":
            try:
                return gzip_decode(data)
            except NotImplementedError:
                self.send_response(501, "encoding %r not supported"
                                   % encoding)
            except ValueError:
                self.send_response(400, "error decoding gzip content")
        else:
            self.send_response(501, "encoding %r not supported" % encoding)
        self.send_header("Content-length", "0")
        self.end_headers()
        return None

    def report_404(self):
        self.send_response(404)
        response = b'No such page'
        self.send_header("Content-type", "text/plain")
        self.send_header("Content-length", str(len(response)))
        self.end_headers()
        self.wfile.write(response)

    def log_request(self, code='-', size='-'):
        if self.server.logRequests:
            BaseHTTPRequestHandler.log_request(self, code, size)


class SimpleXMLRPCServer(socketserver.TCPServer, SimpleXMLRPCDispatcher):
    """A TCP server answering XML-RPC requests over HTTP.

    Set _send_traceback_header to True to return the exception and
    traceback of server-side errors as X-exception / X-traceback headers
    (for debugging only).
    """

    allow_reuse_address = True
    _send_traceback_header = False

    def __init__(self, addr, requestHandler=SimpleXMLRPCRequestHandler,
                 logRequests=True, allow_none=False, encoding=None,
                 bind_and_activate=True, use_builtin_types=False):
        self.logRequests = logRequests
        SimpleXMLRPCDispatcher.__init__(self, allow_none, encoding,
                                        use_builtin_types)
        socketserver.TCPServer.__init__(self, addr, requestHandler,
                                        bind_and_activate)


class MultiPathXMLRPCServer(SimpleXMLRPCServer):
    """Serves a separate dispatcher for each request path."""

    def __init__(self, addr, requestHandler=SimpleXMLRPCRequestHandler,
                 logRequests=True, allow_none=False, encoding=None,
                 bind_and_activate=True, use_builtin_types=False):
        SimpleXMLRPCServer.__init__(self, addr, requestHandler, logRequests,
                                    allow_none, encoding, bind_and_activate,
                                    use_builtin_types)
        self.dispatchers = {}
        self.allow_none = allow_none
        self.encoding = encoding or 'utf-8'

    def add_dispatcher(self, path, dispatcher):
        self.dispatchers[path] = dispatcher
        return dispatcher

    def get_dispatcher(self, path):
        return self.dispatchers[path]

    def _marshaled_dispatch(self, data, dispatch_method=None, path=None):
        try:
            return self.dispatchers[path]._marshaled_dispatch(
                data, dispatch_method, path)
        except BaseException as exc:
            # report a missing or failing dispatcher as a fault
            response = dumps(Fault(1, "%s:%s" % (type(exc), exc)),
                             encoding=self.encoding,
                             allow_none=self.allow_none)
            return response.encode(self.encoding, 'xmlcharrefreplace')


class CGIXMLRPCRequestHandler(SimpleXMLRPCDispatcher):
    """Answers one XML-RPC request in a CGI environment."""

    def __init__(self, allow_none=False, encoding=None,
                 use_builtin_types=False):
        SimpleXMLRPCDispatcher.__init__(self, allow_none, encoding,
                                        use_builtin_types)

    def handle_xmlrpc(self, request_text):
        response = self._marshaled_dispatch(request_text)
        print('Content-Type: text/xml')
        print('Content-Length: %d' % len(response))
        print()
        sys.stdout.flush()
        sys.stdout.buffer.write(response)
        sys.stdout.buffer.flush()

    def handle_get(self):
        """A GET request is not an XML-RPC call: answer 400."""
        code = 400
        message, explain = BaseHTTPRequestHandler.responses[code]
        body = http.server.DEFAULT_ERROR_MESSAGE % {
            'code': code, 'message': message, 'explain': explain}
        body = body.encode('utf-8')
        print('Status: %d %s' % (code, message))
        print('Content-Type: %s' % http.server.DEFAULT_ERROR_CONTENT_TYPE)
        print('Content-Length: %d' % len(body))
        print()
        sys.stdout.flush()
        sys.stdout.buffer.write(body)
        sys.stdout.buffer.flush()

    def handle_request(self, request_text=None):
        """Handle the request from the CGI environment (or the given
        request text)."""
        if request_text is None and os.environ.get('REQUEST_METHOD') == 'GET':
            self.handle_get()
            return
        if request_text is None:
            try:
                length = int(os.environ.get('CONTENT_LENGTH', None))
            except (ValueError, TypeError):
                length = -1
            request_text = sys.stdin.read(length)
        self.handle_xmlrpc(request_text)


# -- HTML documentation --------------------------------------------------------

try:
    import pydoc as _pydoc
    _HTMLDocBase = _pydoc.HTMLDoc
except ImportError:
    _pydoc = None
    _HTMLDocBase = object


class ServerHTMLDoc(_HTMLDocBase):
    """HTML documentation of the methods of an XML-RPC server."""

    _LINKABLE = re.compile(r'\b(?:(?P<url>(?:http|https|ftp)://\S+[\w/])|'
                           r'RFC[- ]?(?P<rfc>\d+)|'
                           r'PEP[- ]?(?P<pep>\d+)|'
                           r'(?P<selfdot>self\.)?(?P<name>[\w.]*\w))')

    def markup(self, text, escape=None, funcs={}, classes={}, methods={}):
        """Escape *text* and turn URLs, RFC/PEP references and known
        (dotted) method names into links."""
        escape = escape or self.escape
        pieces = []
        pos = 0
        for m in self._LINKABLE.finditer(text):
            start, end = m.span()
            pieces.append(escape(text[pos:start]))
            pos = end
            whole = m.group(0)
            if m.group('url'):
                url = escape(whole).replace('"', '&quot;')
                pieces.append('<a href="%s">%s</a>' % (url, url))
            elif m.group('rfc'):
                pieces.append('<a href="https://www.rfc-editor.org/rfc/'
                              'rfc%d.txt">%s</a>'
                              % (int(m.group('rfc')), escape(whole)))
            elif m.group('pep'):
                pieces.append('<a href="https://peps.python.org/pep-%04d/">'
                              '%s</a>' % (int(m.group('pep')), escape(whole)))
            else:
                name = m.group('name')
                called = text[end:end + 1] == '('
                if m.group('selfdot'):
                    if called:
                        pieces.append('self.' + self._link(name, methods))
                    else:
                        pieces.append('self.<strong>%s</strong>' % name)
                elif called:
                    pieces.append(self._link(name, methods, funcs, classes))
                else:
                    pieces.append(self._link(name, classes))
        pieces.append(escape(text[pos:]))
        return ''.join(pieces)

    @staticmethod
    def _link(name, *mappings):
        for mapping in mappings:
            if name in mapping:
                return '<a href="%s">%s</a>' % (mapping[name], name)
        return name

    def docroutine(self, object, name, mod=None, funcs={}, classes={},
                   methods={}, cl=None):
        """One method: its signature and documentation."""
        anchor = (cl.__name__ if cl is not None else '') + '-' + name
        title = '<a name="%s"><strong>%s</strong></a>' % (
            self.escape(anchor), self.escape(name))
        if isinstance(object, tuple):
            # (argument string, documentation) from the instance
            argspec = object[0] or '(...)'
            doc = object[1] or ''
        else:
            try:
                argspec = str(signature(object))
            except (TypeError, ValueError):
                argspec = '(...)'
            doc = _method_doc(object)
        decl = title + self.escape(argspec)
        body = self.markup(doc, self.preformat, funcs, classes, methods)
        if body:
            body = '<dd><tt>%s</tt></dd>' % body
        return '<dl><dt>%s</dt>%s</dl>\n' % (decl, body)

    def docserver(self, server_name, package_documentation, methods):
        """The documentation page body for a server."""
        anchors = {name: '#-' + name for name in methods}
        parts = [self.heading(server_name)]
        parts.append('<p><tt>%s</tt></p>\n' % self.markup(
            package_documentation, self.preformat, anchors))
        entries = ''.join(self.docroutine(value, key, funcs=anchors)
                          for key, value in sorted(methods.items()))
        parts.append(self.bigsection('Methods', 'functions', entries))
        return ''.join(parts)

    def page(self, title, contents):
        """A complete page (with a link to the style sheet)."""
        css = '<link rel="stylesheet" type="text/css" href="pydoc.css">'
        return ('<!DOCTYPE html>\n<html lang="en">\n<head>\n'
                '<meta charset="utf-8">\n%s\n<title>Python: %s</title>\n'
                '</head><body>\n%s\n</body></html>' % (css, title, contents))


class XMLRPCDocGenerator:
    """Generates the HTML documentation of a server's methods."""

    def __init__(self):
        self.server_name = 'XML-RPC Server Documentation'
        self.server_documentation = (
            "This server exports the following methods through the XML-RPC "
            "protocol.")
        self.server_title = 'XML-RPC Server Documentation'

    def set_server_title(self, server_title):
        """The title of the documentation page."""
        self.server_title = server_title

    def set_server_name(self, server_name):
        """The heading of the documentation page."""
        self.server_name = server_name

    def set_server_documentation(self, server_documentation):
        """The text describing the server."""
        self.server_documentation = server_documentation

    def _documented_method(self, name):
        if name in self.funcs:
            return self.funcs[name]
        instance = self.instance
        info = (None, None)
        if instance is None:
            return info
        if hasattr(instance, '_get_method_argstring') or \
                hasattr(instance, '_methodHelp'):
            argstring = (instance._get_method_argstring(name)
                         if hasattr(instance, '_get_method_argstring')
                         else None)
            doc = (instance._methodHelp(name)
                   if hasattr(instance, '_methodHelp') else None)
            info = (argstring, doc)
            if info != (None, None):
                return info
        if hasattr(instance, '_dispatch'):
            return info
        try:
            return resolve_dotted_attribute(instance, name)
        except AttributeError:
            return info

    def generate_html_documentation(self):
        """The documentation of all served methods as an HTML page."""
        methods = {name: self._documented_method(name)
                   for name in self.system_listMethods()}
        doc = ServerHTMLDoc()
        body = doc.docserver(self.server_name, self.server_documentation,
                             methods)
        return doc.page(html.escape(self.server_title), body)


_STYLESHEET = """\
body { background-color: #f0f0f8; color: #000; }
table.heading tr { background-color: #7799ee; }
.decor { color: #fff; }
.title-decor { background-color: #ffc8d8; color: #000; }
.functions-decor { background-color: #eeaa77; }
.heading-text { font-family: helvetica, arial; }
.grey { color: #909090; }
"""


class DocXMLRPCRequestHandler(SimpleXMLRPCRequestHandler):
    """Also answers GET requests: the documentation page and its style
    sheet."""

    def _stylesheet(self):
        if _pydoc is not None:
            path = os.path.join(os.path.dirname(_pydoc.__file__),
                                'pydoc_data', '_pydoc.css')
            try:
                with open(path, 'rb') as f:
                    return f.read()
            except OSError:
                pass
        return _STYLESHEET.encode('utf-8')

    def do_GET(self):
        if not self.is_rpc_path_valid():
            self.report_404()
            return
        if self.path.endswith('.css'):
            content_type = 'text/css'
            response = self._stylesheet()
        else:
            content_type = 'text/html'
            response = self.server.generate_html_documentation()
            response = response.encode('utf-8')
        self.send_response(200)
        self.send_header('Content-Type', '%s; charset=UTF-8' % content_type)
        self.send_header("Content-length", str(len(response)))
        self.end_headers()
        self.wfile.write(response)


class DocXMLRPCServer(SimpleXMLRPCServer, XMLRPCDocGenerator):
    """SimpleXMLRPCServer that also documents its methods over GET."""

    def __init__(self, addr, requestHandler=DocXMLRPCRequestHandler,
                 logRequests=True, allow_none=False, encoding=None,
                 bind_and_activate=True, use_builtin_types=False):
        SimpleXMLRPCServer.__init__(self, addr, requestHandler, logRequests,
                                    allow_none, encoding, bind_and_activate,
                                    use_builtin_types)
        XMLRPCDocGenerator.__init__(self)


class DocCGIXMLRPCRequestHandler(CGIXMLRPCRequestHandler,
                                 XMLRPCDocGenerator):
    """CGI handler that answers GET with the documentation page."""

    def handle_get(self):
        response = self.generate_html_documentation().encode('utf-8')
        print('Content-Type: text/html')
        print('Content-Length: %d' % len(response))
        print()
        sys.stdout.flush()
        sys.stdout.buffer.write(response)
        sys.stdout.buffer.flush()

    def __init__(self):
        CGIXMLRPCRequestHandler.__init__(self)
        XMLRPCDocGenerator.__init__(self)


if __name__ == '__main__':
    import datetime

    class ExampleService:
        def getData(self):
            return '42'

        class currentTime:
            @staticmethod
            def getCurrentTime():
                return datetime.datetime.now()

    with SimpleXMLRPCServer(("localhost", 8000)) as server:
        server.register_function(pow)
        server.register_function(lambda x, y: x + y, 'add')
        server.register_instance(ExampleService(), allow_dotted_names=True)
        server.register_multicall_functions()
        print('Serving XML-RPC on localhost port 8000')
        try:
            server.serve_forever()
        except KeyboardInterrupt:
            print("\nKeyboard interrupt received, exiting.")
            sys.exit(0)
