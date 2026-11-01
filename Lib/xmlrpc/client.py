"""XML-RPC client: marshalling of Python values to XML-RPC and back, an
HTTP transport and the ServerProxy that turns attribute calls into remote
procedure calls.

    proxy = ServerProxy("http://localhost:8000/")
    print(proxy.pow(2, 10))

Values map as follows: int <-> <int>/<i4>, bool <-> <boolean>, float <->
<double>, str <-> <string>, bytes/bytearray/Binary <-> <base64>,
datetime/DateTime <-> <dateTime.iso8601>, list/tuple <-> <array>,
dict <-> <struct>, None <-> <nil/> (only with allow_none) and other
objects are sent as a struct of their attributes.
"""

import base64
import errno
import http.client
import sys
import time
from datetime import datetime
from decimal import Decimal
from io import BytesIO
from xml.parsers import expat

try:
    import gzip
except ImportError:
    gzip = None

__all__ = [
    'Binary', 'Boolean', 'DateTime', 'Error', 'Fault', 'MAXINT', 'MININT',
    'Marshaller', 'MultiCall', 'MultiCallIterator', 'ProtocolError',
    'ResponseError', 'SafeTransport', 'Server', 'ServerProxy', 'Transport',
    'Unmarshaller', 'boolean', 'dumps', 'escape', 'getparser', 'gzip_decode',
    'gzip_encode', 'loads',
]


def escape(s):
    """Escape &, < and > for XML character data."""
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


# XML-RPC integers are signed 32-bit values.
MAXINT = 2**31 - 1
MININT = -2**31

# Fault codes of the "specification for fault code interoperability".
PARSE_ERROR = -32700
SERVER_ERROR = -32600
APPLICATION_ERROR = -32500
SYSTEM_ERROR = -32400
TRANSPORT_ERROR = -32300
NOT_WELLFORMED_ERROR = -32700
UNSUPPORTED_ENCODING = -32701
INVALID_ENCODING_CHAR = -32702
INVALID_XMLRPC = -32600
METHOD_NOT_FOUND = -32601
INVALID_METHOD_PARAMS = -32602
INTERNAL_ERROR = -32603


# -- exceptions ---------------------------------------------------------------

class Error(Exception):
    """Base class for the module's exceptions."""

    def __str__(self):
        return repr(self)


class ProtocolError(Error):
    """The HTTP layer reported an error (errcode is the HTTP status)."""

    def __init__(self, url, errcode, errmsg, headers):
        Error.__init__(self)
        self.url = url
        self.errcode = errcode
        self.errmsg = errmsg
        self.headers = headers

    def __repr__(self):
        return "<%s for %s: %s %s>" % (type(self).__name__, self.url,
                                       self.errcode, self.errmsg)


class ResponseError(Error):
    """The response could not be understood."""


class Fault(Error):
    """An XML-RPC fault returned by the server."""

    def __init__(self, faultCode, faultString, **extra):
        Error.__init__(self)
        self.faultCode = faultCode
        self.faultString = faultString

    def __repr__(self):
        return "<%s %s: %r>" % (type(self).__name__, self.faultCode,
                                self.faultString)


# -- special values -------------------------------------------------------------

boolean = Boolean = bool

_ISO_FORMAT = "%Y%m%dT%H:%M:%S"


def _iso8601(value):
    """'YYYYMMDDTHH:MM:SS' for a datetime (zero padded for any year)."""
    return "%04d%02d%02dT%02d:%02d:%02d" % (
        value.year, value.month, value.day,
        value.hour, value.minute, value.second)


def _date_string(value):
    if isinstance(value, datetime):
        return _iso8601(value)
    if isinstance(value, (tuple, time.struct_time)):
        return "%04d%02d%02dT%02d:%02d:%02d" % tuple(value[:6])
    if value == 0:
        value = time.time()
    return _date_string(time.localtime(value))


class DateTime:
    """An XML-RPC dateTime.iso8601 value.

    DateTime() is the current local time; the value may also be given as
    seconds since the epoch, a time tuple, a datetime or an ISO 8601
    string ('YYYYMMDDTHH:MM:SS').
    """

    def __init__(self, value=0):
        if isinstance(value, str):
            self.value = value
        else:
            self.value = _date_string(value)

    def _comparable(self, other):
        if isinstance(other, DateTime):
            return self.value, other.value
        if isinstance(other, datetime):
            return self.value, _iso8601(other)
        if isinstance(other, str):
            return self.value, other
        if hasattr(other, 'timetuple'):
            return self.timetuple(), other.timetuple()
        return None

    def __lt__(self, other):
        pair = self._comparable(other)
        return NotImplemented if pair is None else pair[0] < pair[1]

    def __le__(self, other):
        pair = self._comparable(other)
        return NotImplemented if pair is None else pair[0] <= pair[1]

    def __gt__(self, other):
        pair = self._comparable(other)
        return NotImplemented if pair is None else pair[0] > pair[1]

    def __ge__(self, other):
        pair = self._comparable(other)
        return NotImplemented if pair is None else pair[0] >= pair[1]

    def __eq__(self, other):
        pair = self._comparable(other)
        return NotImplemented if pair is None else pair[0] == pair[1]

    __hash__ = None

    def timetuple(self):
        return time.strptime(self.value, _ISO_FORMAT)

    def __str__(self):
        return self.value

    def __repr__(self):
        return "<%s %r at %#x>" % (type(self).__name__, self.value, id(self))

    def decode(self, data):
        self.value = str(data).strip()

    def encode(self, out):
        out.write("<value><dateTime.iso8601>")
        out.write(self.value)
        out.write("</dateTime.iso8601></value>\n")


def _datetime(data):
    """A DateTime from its ISO 8601 text."""
    value = DateTime()
    value.decode(data)
    return value


def _datetime_type(data):
    """A datetime.datetime from ISO 8601 text."""
    return datetime.strptime(data, _ISO_FORMAT)


class Binary:
    """Binary data, sent as base64."""

    def __init__(self, data=None):
        if data is None:
            data = b""
        elif not isinstance(data, (bytes, bytearray)):
            raise TypeError("expected bytes or bytearray, not %s"
                            % type(data).__name__)
        else:
            data = bytes(data)
        self.data = data

    def __str__(self):
        return str(self.data, "latin-1")

    def __eq__(self, other):
        if isinstance(other, Binary):
            other = other.data
        return self.data == other

    __hash__ = None

    def decode(self, data):
        self.data = base64.decodebytes(data)

    def encode(self, out):
        out.write("<value><base64>\n")
        out.write(base64.encodebytes(self.data).decode('ascii'))
        out.write("</base64></value>\n")


def _binary(data):
    value = Binary()
    value.decode(data)
    return value


WRAPPERS = (DateTime, Binary)


# -- marshalling ------------------------------------------------------------------

class Marshaller:
    """Converts Python values to XML-RPC <params> (or a <fault>)."""

    def __init__(self, encoding=None, allow_none=False):
        self.memo = {}
        self.data = None
        self.encoding = encoding
        self.allow_none = allow_none

    def dumps(self, values):
        out = []
        write = out.append
        if isinstance(values, Fault):
            write("<fault>\n")
            self.__dump({'faultCode': values.faultCode,
                         'faultString': values.faultString}, write)
            write("</fault>\n")
        else:
            write("<params>\n")
            for value in values:
                write("<param>\n")
                self.__dump(value, write)
                write("</param>\n")
            write("</params>\n")
        return "".join(out)

    def __dump(self, value, write):
        for klass in type(value).__mro__:
            method = self._dumpers.get(klass)
            if method is not None:
                return method(self, value, write)
        if hasattr(value, '__dict__'):
            return self.dump_instance(value, write)
        raise TypeError("cannot marshal %s objects" % type(value))

    def dump_nil(self, value, write):
        if not self.allow_none:
            raise TypeError("cannot marshal None unless allow_none is enabled")
        write("<value><nil/></value>")

    def dump_bool(self, value, write):
        write("<value><boolean>%d</boolean></value>\n" % (1 if value else 0))

    def dump_long(self, value, write):
        if value > MAXINT or value < MININT:
            raise OverflowError("int exceeds XML-RPC limits")
        write("<value><int>%d</int></value>\n" % int(value))

    dump_int = dump_long

    def dump_double(self, value, write):
        write("<value><double>%r</double></value>\n" % float(value))

    def dump_unicode(self, value, write, escape=escape):
        write("<value><string>%s</string></value>\n" % escape(value))

    def dump_bytes(self, value, write):
        write("<value><base64>\n%s</base64></value>\n"
              % base64.encodebytes(value).decode('ascii'))

    def _enter(self, value, what):
        key = id(value)
        if key in self.memo:
            raise TypeError("cannot marshal recursive %s" % what)
        self.memo[key] = None
        return key

    def dump_array(self, value, write):
        key = self._enter(value, "sequences")
        write("<value><array><data>\n")
        for item in value:
            self.__dump(item, write)
        write("</data></array></value>\n")
        del self.memo[key]

    def dump_struct(self, value, write, escape=escape):
        key = self._enter(value, "dictionaries")
        write("<value><struct>\n")
        for name, item in value.items():
            if not isinstance(name, str):
                raise TypeError("dictionary key must be string")
            write("<member>\n<name>%s</name>\n" % escape(name))
            self.__dump(item, write)
            write("</member>\n")
        write("</struct></value>\n")
        del self.memo[key]

    def dump_datetime(self, value, write):
        write("<value><dateTime.iso8601>%s</dateTime.iso8601></value>\n"
              % _iso8601(value))

    def dump_instance(self, value, write):
        if isinstance(value, WRAPPERS):
            self.write = write
            value.encode(self)
            del self.write
        else:
            # other objects travel as a struct of their attributes
            self.dump_struct(value.__dict__, write)

    _dumpers = {
        type(None): dump_nil,
        bool: dump_bool,
        int: dump_long,
        float: dump_double,
        str: dump_unicode,
        bytes: dump_bytes,
        bytearray: dump_bytes,
        tuple: dump_array,
        list: dump_array,
        dict: dump_struct,
        datetime: dump_datetime,
        DateTime: dump_instance,
        Binary: dump_instance,
    }


# -- unmarshalling ------------------------------------------------------------

class Unmarshaller:
    """Builds Python values from the parse events of an XML-RPC document.

    close() returns the tuple of values, or raises Fault for a fault
    response.
    """

    def __init__(self, use_datetime=False, use_builtin_types=False):
        self._values = []
        self._marks = []
        self._data = []
        self._kind = None
        self._methodname = None
        self._raw_value = False
        self._encoding = "utf-8"
        self._use_datetime = use_builtin_types or use_datetime
        self._use_bytes = use_builtin_types
        self.append = self._values.append

    def close(self):
        if self._kind is None or self._marks:
            raise ResponseError()
        if self._kind == "fault":
            raise Fault(**self._values[0])
        return tuple(self._values)

    def getmethodname(self):
        return self._methodname

    def xml(self, version, encoding, standalone):
        self._encoding = encoding

    def start(self, tag, attrs):
        tag = tag.rpartition(':')[2]
        if tag in ('array', 'struct'):
            self._marks.append(len(self._values))
        elif tag == 'value':
            self._raw_value = True
        elif tag in ('params', 'fault'):
            self._kind = tag
        self._data = []

    def data(self, text):
        self._data.append(text)

    def end(self, tag):
        tag = tag.rpartition(':')[2]
        handler = self._enders.get(tag)
        if handler is None:
            raise ResponseError("unknown tag %r" % tag)
        handler(self, "".join(self._data))

    def end_dispatch(self, tag, data):
        handler = self._enders.get(tag)
        if handler is None:
            raise ResponseError("unknown tag %r" % tag)
        return handler(self, data)

    def _typed(self, value):
        self.append(value)
        self._raw_value = False

    def end_nil(self, data):
        self._typed(None)

    def end_boolean(self, data):
        if data == "0":
            self._typed(False)
        elif data == "1":
            self._typed(True)
        else:
            raise TypeError("bad boolean value")

    def end_int(self, data):
        self._typed(int(data))

    def end_double(self, data):
        self._typed(float(data))

    def end_bigdecimal(self, data):
        self._typed(Decimal(data))

    def end_string(self, data):
        self._typed(data)

    def end_base64(self, data):
        raw = base64.decodebytes(data.encode("ascii"))
        self._typed(raw if self._use_bytes else _binary_from(raw))

    def end_datetime(self, data):
        if self._use_datetime:
            self._typed(_datetime_type(data))
        else:
            self._typed(_datetime(data))

    def end_array(self, data):
        start = self._marks.pop()
        items = self._values[start:]
        del self._values[start:]
        self._typed(items)

    def end_struct(self, data):
        start = self._marks.pop()
        flat = self._values[start:]
        del self._values[start:]
        result = {}
        for i in range(0, len(flat), 2):
            result[flat[i]] = flat[i + 1]
        self._typed(result)

    def end_value(self, data):
        # a <value> without a type element holds a string
        if self._raw_value:
            self._typed(data)

    def end_name(self, data):
        self.append(data)

    def end_methodname(self, data):
        self._methodname = data
        self._kind = "methodName"

    def end_ignored(self, data):
        pass

    _enders = {
        'nil': end_nil,
        'boolean': end_boolean,
        'int': end_int, 'i1': end_int, 'i2': end_int, 'i4': end_int,
        'i8': end_int, 'biginteger': end_int,
        'double': end_double, 'float': end_double,
        'bigdecimal': end_bigdecimal,
        'string': end_string,
        'base64': end_base64,
        'dateTime.iso8601': end_datetime,
        'array': end_array,
        'struct': end_struct,
        'value': end_value,
        'name': end_name,
        'methodName': end_methodname,
        'data': end_ignored, 'member': end_ignored, 'param': end_ignored,
        'params': end_ignored, 'fault': end_ignored,
        'methodCall': end_ignored, 'methodResponse': end_ignored,
    }


def _binary_from(raw):
    value = Binary()
    value.data = raw
    return value


class ExpatParser:
    """Feeds an Unmarshaller from an expat parser."""

    def __init__(self, target):
        self._parser = parser = expat.ParserCreate(None, None)
        self._target = target
        parser.StartElementHandler = target.start
        parser.EndElementHandler = target.end
        parser.CharacterDataHandler = target.data

    def feed(self, data):
        self._parser.Parse(data, False)

    def close(self):
        try:
            parser = self._parser
        except AttributeError:
            return
        del self._target, self._parser
        parser.Parse(b"", True)


def getparser(use_datetime=False, use_builtin_types=False):
    """A (parser, unmarshaller) pair: feed the parser, then close both."""
    target = Unmarshaller(use_datetime=use_datetime,
                          use_builtin_types=use_builtin_types)
    return ExpatParser(target), target


def dumps(params, methodname=None, methodresponse=None, encoding=None,
          allow_none=False):
    """Marshal a tuple of parameters (or a Fault) as an XML-RPC request
    (if *methodname* is given), a response (if *methodresponse*) or a
    bare <params> fragment."""
    if not isinstance(params, (tuple, Fault)):
        raise TypeError("argument must be tuple or Fault instance")
    if isinstance(params, Fault):
        methodresponse = 1
    elif methodresponse and len(params) != 1:
        raise ValueError("response tuple must be a singleton")
    encoding = encoding or "utf-8"
    body = Marshaller(encoding, allow_none).dumps(params)
    if encoding != "utf-8":
        header = "<?xml version='1.0' encoding='%s'?>\n" % str(encoding)
    else:
        header = "<?xml version='1.0'?>\n"
    if methodname:
        return "".join((header, "<methodCall>\n<methodName>",
                        escape(methodname), "</methodName>\n",
                        body, "</methodCall>\n"))
    if methodresponse:
        return "".join((header, "<methodResponse>\n", body,
                        "</methodResponse>\n"))
    return body


def loads(data, use_datetime=False, use_builtin_types=False):
    """Parse an XML-RPC request or response: returns (params, methodname)
    and raises Fault for a fault response."""
    parser, unmarshaller = getparser(use_datetime=use_datetime,
                                     use_builtin_types=use_builtin_types)
    parser.feed(data)
    parser.close()
    return unmarshaller.close(), unmarshaller.getmethodname()


def gzip_encode(data):
    """Compress *data* with gzip."""
    if not gzip:
        raise NotImplementedError
    out = BytesIO()
    with gzip.GzipFile(mode="wb", fileobj=out, compresslevel=1) as f:
        f.write(data)
    return out.getvalue()


def gzip_decode(data, max_decode=20971520):
    """Decompress gzip *data*; ValueError if it is invalid or would grow
    beyond *max_decode* bytes (a negative limit means no limit)."""
    if not gzip:
        raise NotImplementedError
    with gzip.GzipFile(mode="rb", fileobj=BytesIO(data)) as f:
        try:
            if max_decode < 0:
                decoded = f.read()
            else:
                decoded = f.read(max_decode + 1)
        except OSError:
            raise ValueError("invalid data")
    if max_decode >= 0 and len(decoded) > max_decode:
        raise ValueError("max gzipped payload length exceeded")
    return decoded


class GzipDecodedResponse(gzip.GzipFile if gzip else object):
    """A file-like view of a gzip-encoded HTTP response body."""

    def __init__(self, response):
        if not gzip:
            raise NotImplementedError
        self.io = BytesIO(response.read())
        gzip.GzipFile.__init__(self, mode="rb", fileobj=self.io)

    def close(self):
        try:
            gzip.GzipFile.close(self)
        finally:
            self.io.close()


# -- calls ------------------------------------------------------------------

class _Method:
    """A (possibly dotted) remote method name bound to a send function."""

    def __init__(self, send, name):
        self.__send = send
        self.__name = name

    def __getattr__(self, name):
        return _Method(self.__send, "%s.%s" % (self.__name, name))

    def __call__(self, *args):
        return self.__send(self.__name, args)


class _MultiCallMethod:
    def __init__(self, call_list, name):
        self.__call_list = call_list
        self.__name = name

    def __getattr__(self, name):
        return _MultiCallMethod(self.__call_list,
                                "%s.%s" % (self.__name, name))

    def __call__(self, *args):
        self.__call_list.append((self.__name, args))


class MultiCallIterator:
    """The results of a MultiCall; faults are raised when accessed."""

    def __init__(self, results):
        self.results = results

    def __getitem__(self, i):
        item = self.results[i]
        if isinstance(item, dict):
            raise Fault(item['faultCode'], item['faultString'])
        if isinstance(item, list):
            return item[0]
        raise ValueError("unexpected type in multicall result")


class MultiCall:
    """Collects calls and sends them in one system.multicall request.

        multicall = MultiCall(server)
        multicall.add(1, 2)
        multicall.pow(2, 3)
        add_result, pow_result = multicall()
    """

    def __init__(self, server):
        self.__server = server
        self.__call_list = []

    def __repr__(self):
        return "<%s at %#x>" % (type(self).__name__, id(self))

    def __getattr__(self, name):
        return _MultiCallMethod(self.__call_list, name)

    def __call__(self):
        calls = [{'methodName': name, 'params': params}
                 for name, params in self.__call_list]
        return MultiCallIterator(self.__server.system.multicall(calls))


class Transport:
    """Sends requests over HTTP (with keep-alive) and parses the
    responses."""

    user_agent = "Python-xmlrpc/%d.%d" % sys.version_info[:2]
    accept_gzip_encoding = True
    accept_gzip_encoding = True
    verbose = False
    encode_threshold = None

    def __init__(self, use_datetime=False, use_builtin_types=False, *,
                 headers=()):
        self._use_datetime = use_datetime
        self._use_builtin_types = use_builtin_types
        self._connection = (None, None)
        self._headers = list(headers)
        self._extra_headers = []

    def request(self, host, handler, request_body, verbose=False):
        # A kept-alive connection may have been closed by the server in
        # the meantime: retry once on a fresh connection.
        for attempt in (0, 1):
            try:
                return self.single_request(host, handler, request_body,
                                           verbose)
            except http.client.RemoteDisconnected:
                if attempt:
                    raise
            except OSError as exc:
                if attempt or exc.errno not in (errno.ECONNRESET,
                                                errno.ECONNABORTED,
                                                errno.EPIPE):
                    raise

    def single_request(self, host, handler, request_body, verbose=False):
        try:
            connection = self.send_request(host, handler, request_body,
                                           verbose)
            response = connection.getresponse()
            if response.status == 200:
                self.verbose = verbose
                return self.parse_response(response)
        except Fault:
            raise
        except Exception:
            self.close()
            raise
        if response.getheader("content-length", ""):
            response.read()
        raise ProtocolError(host + handler, response.status,
                            response.reason, dict(response.getheaders()))

    def getparser(self):
        return getparser(use_datetime=self._use_datetime,
                         use_builtin_types=self._use_builtin_types)

    def get_host_info(self, host):
        """Split 'user:password@host' into (host, auth headers, x509)."""
        x509 = {}
        if isinstance(host, tuple):
            host, x509 = host
        auth, at, hostname = host.rpartition('@')
        headers = []
        if at:
            from urllib.parse import unquote_to_bytes
            credentials = base64.encodebytes(unquote_to_bytes(auth))
            credentials = "".join(credentials.decode("ascii").split())
            headers.append(("Authorization", "Basic " + credentials))
            host = hostname
        return host, headers, x509

    def make_connection(self, host):
        cached_host, connection = self._connection
        if connection is not None and host == cached_host:
            return connection
        hostname, self._extra_headers, x509 = self.get_host_info(host)
        connection = http.client.HTTPConnection(hostname)
        self._connection = host, connection
        return connection

    def close(self):
        host, connection = self._connection
        self._connection = (None, None)
        if connection is not None:
            connection.close()

    def send_request(self, host, handler, request_body, debug):
        connection = self.make_connection(host)
        headers = self._headers + self._extra_headers
        if debug:
            connection.set_debuglevel(1)
        if self.accept_gzip_encoding and gzip:
            connection.putrequest("POST", handler, skip_accept_encoding=True)
            headers.append(("Accept-Encoding", "gzip"))
        else:
            connection.putrequest("POST", handler)
        headers.append(("Content-Type", "text/xml"))
        headers.append(("User-Agent", self.user_agent))
        self.send_headers(connection, headers)
        self.send_content(connection, request_body)
        return connection

    def send_headers(self, connection, headers):
        for key, value in headers:
            connection.putheader(key, value)

    def send_content(self, connection, request_body):
        threshold = self.encode_threshold
        if (threshold is not None and threshold < len(request_body)
                and gzip):
            connection.putheader("Content-Encoding", "gzip")
            request_body = gzip_encode(request_body)
        connection.putheader("Content-Length", str(len(request_body)))
        connection.endheaders(request_body)

    def parse_response(self, response):
        if response.getheader("Content-Encoding", "") == "gzip":
            stream = GzipDecodedResponse(response)
        else:
            stream = response
        parser, unmarshaller = self.getparser()
        while True:
            data = stream.read(1024)
            if not data:
                break
            if self.verbose:
                print("body:", repr(data))
            parser.feed(data)
        if stream is not response:
            stream.close()
        parser.close()
        return unmarshaller.close()


class SafeTransport(Transport):
    """Transport over HTTPS."""

    def __init__(self, use_datetime=False, use_builtin_types=False, *,
                 headers=(), context=None):
        super().__init__(use_datetime=use_datetime,
                         use_builtin_types=use_builtin_types,
                         headers=headers)
        self.context = context

    def make_connection(self, host):
        cached_host, connection = self._connection
        if connection is not None and host == cached_host:
            return connection
        if not hasattr(http.client, "HTTPSConnection"):
            raise NotImplementedError(
                "your version of http.client doesn't support HTTPS")
        hostname, self._extra_headers, x509 = self.get_host_info(host)
        connection = http.client.HTTPSConnection(hostname, None,
                                                 context=self.context, **x509)
        self._connection = host, connection
        return connection


class ServerProxy:
    """A proxy for a remote XML-RPC server: attribute access gives remote
    methods (dotted names allowed).

    ServerProxy(uri, transport=None, encoding=None, verbose=False,
                allow_none=False, use_datetime=False,
                use_builtin_types=False, *, headers=(), context=None)

    proxy('close')() closes the connection and proxy('transport') returns
    the transport.
    """

    def __init__(self, uri, transport=None, encoding=None, verbose=False,
                 allow_none=False, use_datetime=False,
                 use_builtin_types=False, *, headers=(), context=None):
        from urllib.parse import urlsplit, urlunsplit
        parts = urlsplit(uri)
        if parts.scheme not in ("http", "https"):
            raise OSError("unsupported XML-RPC protocol")
        self.__host = parts.netloc
        self.__handler = urlunsplit(("", "", parts.path, parts.query,
                                     parts.fragment)) or "/RPC2"
        if transport is None:
            if parts.scheme == "https":
                transport = SafeTransport(use_datetime=use_datetime,
                                          use_builtin_types=use_builtin_types,
                                          headers=headers, context=context)
            else:
                transport = Transport(use_datetime=use_datetime,
                                      use_builtin_types=use_builtin_types,
                                      headers=headers)
        self.__transport = transport
        self.__encoding = encoding or "utf-8"
        self.__verbose = verbose
        self.__allow_none = allow_none

    def __close(self):
        self.__transport.close()

    def __request(self, methodname, params):
        request = dumps(params, methodname, encoding=self.__encoding,
                        allow_none=self.__allow_none)
        request = request.encode(self.__encoding, "xmlcharrefreplace")
        response = self.__transport.request(self.__host, self.__handler,
                                            request, verbose=self.__verbose)
        if len(response) == 1:
            response = response[0]
        return response

    def __repr__(self):
        return "<%s for %s%s>" % (type(self).__name__, self.__host,
                                  self.__handler)

    def __getattr__(self, name):
        return _Method(self.__request, name)

    def __call__(self, attr):
        if attr == "close":
            return self.__close
        if attr == "transport":
            return self.__transport
        raise AttributeError("Attribute %r not found" % (attr,))

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.__close()


Server = ServerProxy


if __name__ == "__main__":
    proxy = ServerProxy("http://localhost:8000")
    try:
        print(proxy.currentTime.getCurrentTime())
    except Error as exc:
        print("ERROR", exc)
