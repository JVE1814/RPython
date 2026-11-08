"""Client side of HTTP/1.1 (RFC 9110 semantics, RFC 9112 message syntax).

An HTTPConnection talks to one server over one connection, which it keeps
open between requests when the server allows it:

    conn = HTTPConnection('www.example.com')
    conn.request('GET', '/index.html')
    reply = conn.getresponse()      # an HTTPResponse
    data = reply.read()

request() does everything in one call.  The lower-level steps are
putrequest() (the request line), putheader() (any number of headers) and
endheaders() (the blank line and the body), followed by getresponse().
Calling them out of order raises an ImproperConnectionState subclass.
HTTPSConnection exists when the ssl module does.
"""

import collections.abc
import email.message
import email.parser
import errno
import http
import io
import re
import socket
import sys
from urllib.parse import urlsplit

__all__ = ["HTTPResponse", "HTTPConnection",
           "HTTPException", "NotConnected", "UnknownProtocol",
           "UnknownTransferEncoding", "UnimplementedFileMode",
           "IncompleteRead", "InvalidURL", "ImproperConnectionState",
           "CannotSendRequest", "CannotSendHeader", "ResponseNotReady",
           "BadStatusLine", "LineTooLong", "RemoteDisconnected", "error",
           "responses"]

HTTP_PORT = 80
HTTPS_PORT = 443

# Status codes as module constants (http.client.NOT_FOUND, ...) and their
# reason phrases.
globals().update(http.HTTPStatus.__members__)
responses = {status: status.phrase for status in http.HTTPStatus}

# What an HTTPResponse reports before its status line has been read.
_UNKNOWN = 'UNKNOWN'

# Where an HTTPConnection stands in the request/response cycle.  The texts
# appear in the exceptions raised for calls out of order.
_READY = 'Idle'                 # may start a request
_WRITING = 'Request-started'    # request line sent, headers may follow
_WAITING = 'Request-sent'       # headers complete, a response is due

# Bounds against misbehaving servers.
_MAXLINE = 65536
_MAXHEADERS = 100
_MAXINTERIMRESPONSES = 100

# Bodies of known size are read in pieces of at most this many bytes, so
# that a huge Content-Length does not allocate a huge buffer up front.
_READ_PIECE = 1 << 20

# Methods whose requests normally have a body: they get
# "Content-Length: 0" when sent without one.
_BODY_METHODS = frozenset({'PATCH', 'POST', 'PUT'})

# Control characters (and space) that must not occur in a URL or host;
# methods may contain spaces in principle, but no control characters.
_contains_url_control = re.compile('[\x00-\x20\x7f]').search
_contains_method_control = re.compile('[\x00-\x1f]').search

# A field name is any run of bytes without ':', CR or LF that does not
# start with whitespace.  In a value, CR and LF may only appear as part of
# an (obsolete) line folding: followed by a space or a tab.
_is_good_header_name = re.compile(rb'[^:\s][^:\r\n]*').fullmatch
_has_bad_line_break = re.compile(rb'\n(?![ \t])|\r(?![ \t\n])').search


def _encode_latin1(text, what):
    try:
        return text.encode('latin-1')
    except UnicodeEncodeError as exc:
        hint = ("%s (%.20r) is not valid Latin-1. Use %s.encode('utf-8') "
                "if you want to send it encoded in UTF-8."
                % (what.title(), text[exc.start:exc.end], what))
        raise UnicodeEncodeError(exc.encoding, exc.object, exc.start,
                                 exc.end, hint) from None


def _bracketed(host):
    """An IPv6 address (str or bytes) in brackets, as URLs write it."""
    if isinstance(host, str):
        return '[%s]' % host if ':' in host and host[:1] != '[' else host
    return b'[%s]' % host if b':' in host and host[:1] != b'[' else host


def _without_zone_id(host):
    # "fe80::1%eth0" -> "fe80::1": the zone of a link-local address means
    # nothing to the server.  A bracketed host keeps its closing bracket.
    address, percent, _ = host.partition(b'%')
    if percent and address.startswith(b'[') and not address.endswith(b']'):
        address += b']'
    return address


def _host_bytes(host):
    try:
        return host.encode('ascii')
    except UnicodeEncodeError:
        return host.encode('idna')


# ------------------------------------------------------------ headers

class HTTPMessage(email.message.Message):
    """The header section of an HTTP response."""

    def getallmatchingheaders(self, name):
        """The header lines named name (any case), as "Name: value" str."""
        name = name.lower()
        return ["%s: %s" % (key, value) for key, value in self.raw_items()
                if key.lower() == name]


def _read_header_lines(fp, limit=None, kind='headers'):
    """The raw lines of a header block, up to the blank line (or EOF)."""
    if limit is None:
        limit = _MAXHEADERS
    lines = []
    while True:
        line = fp.readline(_MAXLINE + 1)
        if len(line) > _MAXLINE:
            raise LineTooLong("header line")
        if line in (b'\r\n', b'\n', b''):
            return lines
        lines.append(line)
        if len(lines) > limit:
            raise HTTPException("got more than %d %s" % (limit, kind))


def _parse_header_lines(lines, _class=HTTPMessage):
    # Header bytes are taken as Latin-1, which round-trips every octet.
    text = b''.join(lines).decode('iso-8859-1')
    return email.parser.Parser(_class=_class).parsestr(text)


def parse_headers(fp, _class=HTTPMessage, *, _max_headers=None):
    """Read a header block from the binary file fp and return it as an
    HTTPMessage (or as an instance of _class)."""
    return _parse_header_lines(_read_header_lines(fp, _max_headers), _class)


# ------------------------------------------------------------ responses

class HTTPResponse(io.BufferedIOBase):
    """The response to a request; a readable binary stream of the body.

    After begin(): status, reason, version (10 or 11) and headers (also
    called msg).  The body is read with read(), readinto(), read1(),
    readline() and peek(); "chunked" transfer coding is undone on the way.
    """

    def __init__(self, sock, debuglevel=0, method=None, url=None):
        # Everything is read through this buffered file; reading the socket
        # directly afterwards could skip data that is in the buffer.
        self.fp = sock.makefile('rb')
        self.debuglevel = debuglevel
        self._method = method
        self.url = url
        self._max_headers = None
        self.headers = self.msg = None
        self.version = self.status = self.reason = _UNKNOWN
        # How the body is delimited, known after begin():
        self.chunked = _UNKNOWN     # chunked transfer coding?
        self.chunk_left = _UNKNOWN  # bytes left in the current chunk
        self.length = _UNKNOWN      # bytes left in the body (None: until EOF)
        self.will_close = _UNKNOWN  # does the connection end with the body?

    # -- the head

    def _read_status(self):
        raw = self.fp.readline(_MAXLINE + 1)
        if len(raw) > _MAXLINE:
            raise LineTooLong("status line")
        line = raw.decode('iso-8859-1')
        if self.debuglevel > 0:
            print("reply:", repr(line))
        if not line:
            # The server closed the connection: it may have dropped an idle
            # keep-alive connection.
            raise RemoteDisconnected("Remote end closed connection without response")
        version, status, reason = (line.split(None, 2) + ['', '', ''])[:3]
        if not version.startswith('HTTP/'):
            self._close_conn()
            raise BadStatusLine(line)
        if not (len(status) == 3 and status.isascii() and status.isdigit()
                and 100 <= int(status) <= 999):
            raise BadStatusLine(line)
        return version, int(status), reason

    def _show_header_lines(self, lines):
        for line in lines:
            print("header:", line.decode('iso-8859-1').rstrip())

    def begin(self, *, _max_headers=None):
        """Read the status line and headers and work out the body framing."""
        if self.headers is not None:
            return
        self._max_headers = _max_headers
        # 1xx answers are informational: skip them, headers and all.
        for skipped in range(_MAXINTERIMRESPONSES + 1):
            version, status, reason = self._read_status()
            if status != CONTINUE:
                break
            interim = _read_header_lines(self.fp, _max_headers)
            if self.debuglevel > 0:
                self._show_header_lines(interim)
        else:
            raise HTTPException("got more than %d interim responses"
                                % _MAXINTERIMRESPONSES)

        self.code = self.status = status
        self.reason = reason.strip()
        if version in ('HTTP/1.0', 'HTTP/0.9'):
            self.version = 10
        elif version.startswith('HTTP/1.'):
            self.version = 11       # also for any later 1.x
        else:
            raise UnknownProtocol(version)

        self.headers = self.msg = parse_headers(self.fp, _max_headers=_max_headers)
        if self.debuglevel > 0:
            for name, value in self.headers.items():
                print("header:", name + ":", value)
        self._decide_framing()

    def _decide_framing(self):
        coding = self.headers.get('transfer-encoding')
        self.chunked = bool(coding) and coding.lower() == 'chunked'
        self.chunk_left = None if self.chunked else _UNKNOWN
        self.will_close = self._check_close()

        self.length = None
        declared = self.headers.get('content-length')
        if declared and not self.chunked:
            try:
                size = int(declared)
            except ValueError:
                size = -1
            if size >= 0:
                self.length = size
        status = self.status
        if (status in (NO_CONTENT, NOT_MODIFIED) or 100 <= status < 200
                or self._method == 'HEAD'):
            self.length = 0     # no body, whatever the headers say
        if self.length is None and not self.chunked:
            self.will_close = True      # the body ends where the connection does

    def _check_close(self):
        """Whether the server will close the connection after this
        response, by the protocol version and the Connection headers."""
        def says(name, token):
            value = self.headers.get(name)
            return bool(value) and token in value.lower()

        if self.version == 11:
            return says('connection', 'close')
        # HTTP/1.0 closes unless the server explicitly keeps it open.
        keep = (says('connection', 'keep-alive')
                or bool(self.headers.get('keep-alive'))
                or says('proxy-connection', 'keep-alive'))
        return not keep

    # -- stream plumbing

    def _close_conn(self):
        fp, self.fp = self.fp, None
        fp.close()

    def close(self):
        try:
            super().close()     # marks the stream closed
        finally:
            if self.fp:
                self._close_conn()

    def flush(self):
        super().flush()
        if self.fp:
            self.fp.flush()

    def readable(self):
        return True

    def isclosed(self):
        """True once the body has been read completely (or the response
        was closed); the connection can then carry a new request."""
        return self.fp is None

    def fileno(self):
        return self.fp.fileno()

    def _no_body_left(self):
        # Nothing (more) to read: closed, or the answer to a HEAD request
        # (whose headers may announce a body that is never sent).
        if self.fp is None:
            return True
        if self._method == 'HEAD':
            self._close_conn()
            return True
        return False

    def _account(self, data, wanted):
        # After reading data from an identity-coded body: an empty read
        # means EOF; reaching the announced length ends the body too.
        if not data and wanted:
            self._close_conn()
        elif self.length is not None:
            self.length -= len(data)
            if not self.length:
                self._close_conn()

    def _exactly(self, amt):
        """Read exactly amt bytes, raising IncompleteRead with what arrived
        if the stream ends first."""
        if amt <= _READ_PIECE:
            data = self.fp.read(amt)
            if len(data) < amt:
                raise IncompleteRead(data, amt - len(data))
            return data
        pieces = []
        missing = amt
        while missing > 0:
            piece = self.fp.read(min(missing, _READ_PIECE))
            if not piece:
                data = b''.join(pieces)
                raise IncompleteRead(data, amt - len(data))
            pieces.append(piece)
            missing -= len(piece)
        return b''.join(pieces)

    # Kept under its old name for subclasses.
    _safe_read = _exactly

    # -- chunked transfer coding

    def _chunk_size(self):
        line = self.fp.readline(_MAXLINE + 1)
        if len(line) > _MAXLINE:
            raise LineTooLong("chunk size")
        try:
            return int(line.split(b';', 1)[0], 16)    # extensions are ignored
        except ValueError:
            self._close_conn()
            raise IncompleteRead(b'') from None

    def _chunk_window(self):
        """Bytes left in the current chunk, starting the next chunk if this
        one is used up; None once the last chunk has been read."""
        left = self.chunk_left
        if left:
            return left
        if left is not None:
            self._exactly(2)    # CRLF after the data of the previous chunk
        left = self._chunk_size()
        if left == 0:
            # last chunk: trailer fields follow, which are discarded
            _read_header_lines(self.fp, self._max_headers, 'trailers')
            self._close_conn()
            left = None
        self.chunk_left = left
        return left

    def _read_chunked(self, amt=None):
        got = []
        try:
            while True:
                left = self._chunk_window()
                if left is None:
                    break
                take = amt if amt is not None and amt < left else left
                got.append(self._exactly(take))
                self.chunk_left = left - take
                if amt is not None:
                    amt -= take
                    if not amt:
                        break
        except IncompleteRead as exc:
            raise IncompleteRead(b''.join(got)) from exc
        return b''.join(got)

    # -- reading the body

    def read(self, amt=None):
        """Read up to amt bytes of the body, or all of the rest."""
        if (self.fp is None or self._method == 'HEAD') and self._no_body_left():
            return b''
        if amt is not None and amt < 0:
            amt = None
        if self.chunked:
            return self._read_chunked(amt)
        if amt is not None:
            if self.length is not None:
                amt = min(amt, self.length)
            data = self.fp.read(amt)
            self._account(data, amt)
            return data
        if self.length is None:
            data = self.fp.read()
        else:
            try:
                data = self._exactly(self.length)
            except IncompleteRead:
                self._close_conn()
                raise
            self.length = 0
        self._close_conn()
        return data

    def readinto(self, b):
        """Read body bytes into the writable buffer b; returns how many."""
        if self._no_body_left():
            return 0
        target = memoryview(b).cast('B')
        if not len(target):
            return 0
        data = self.read(len(target))
        target[:len(data)] = data
        return len(data)

    def read1(self, n=-1):
        """Read up to n bytes with at most one read from the socket."""
        if self.fp is None or self._method == 'HEAD':
            return b''
        if self.chunked:
            left = self._chunk_window()
            if left is None or n == 0:
                return b''
            data = self.fp.read1(left if not 0 <= n <= left else n)
            self.chunk_left -= len(data)
            if not data:
                raise IncompleteRead(b'')
            return data
        if self.length is not None and not 0 <= n <= self.length:
            n = self.length
        data = self.fp.read1(n)
        self._account(data, n)
        return data

    def peek(self, n=-1):
        """Body bytes that can be returned without reading the socket; at
        least one unless the body is finished."""
        if self.fp is None or self._method == 'HEAD':
            return b''
        if not self.chunked:
            return self.fp.peek(n)
        try:
            left = self._chunk_window()
        except IncompleteRead:
            return b''
        if left is None:
            return b''
        return self.fp.peek(left)[:left]

    def readline(self, limit=-1):
        """Read one line of the body (at most limit bytes)."""
        if self.fp is None or self._method == 'HEAD':
            return b''
        if limit is None:
            limit = -1
        if self.chunked:
            return self._readline_chunked(limit)
        if self.length is not None and not 0 <= limit <= self.length:
            limit = self.length
        data = self.fp.readline(limit)
        self._account(data, limit)
        return data

    def _readline_chunked(self, limit):
        line = bytearray()
        while limit < 0 or len(line) < limit:
            left = self._chunk_window()
            if left is None:
                break
            budget = left if limit < 0 else min(left, limit - len(line))
            piece = self.fp.readline(budget)
            if not piece:
                raise IncompleteRead(bytes(line))
            self.chunk_left = left - len(piece)
            line += piece
            if piece.endswith(b'\n'):
                break
        return bytes(line)

    # -- headers and the urllib response interface

    def getheader(self, name, default=None):
        """The value of header name (several are joined with ", "), or
        default when it is missing."""
        if self.headers is None:
            raise ResponseNotReady()
        values = self.headers.get_all(name) or default
        if isinstance(values, str) or not hasattr(values, '__iter__'):
            return values
        return ', '.join(values)

    def getheaders(self):
        """All headers as a list of (name, value) pairs."""
        if self.headers is None:
            raise ResponseNotReady()
        return list(self.headers.items())

    def __iter__(self):
        return self

    def info(self):
        """The headers (an HTTPMessage)."""
        return self.headers

    def geturl(self):
        """The URL that was fetched (filled in by urllib.request)."""
        return self.url

    def getcode(self):
        """The status code."""
        return self.status


# ------------------------------------------------------------ connections

def _create_https_context(http_version):
    # The default context for HTTPSConnection: certificate checks on,
    # ALPN offering HTTP/1.1, and post-handshake authentication allowed.
    context = ssl._create_default_https_context()
    if http_version == 11:
        context.set_alpn_protocols(['http/1.1'])
    if context.post_handshake_auth is not None:
        context.post_handshake_auth = True
    return context


class HTTPConnection:
    """A connection to one HTTP server (opened on first use)."""

    _http_vsn = 11
    _http_vsn_str = 'HTTP/1.1'

    response_class = HTTPResponse
    default_port = HTTP_PORT
    auto_open = 1
    debuglevel = 0

    @staticmethod
    def _is_textIO(stream):
        return isinstance(stream, io.TextIOBase)

    @staticmethod
    def _get_content_length(body, method):
        """The Content-Length header value for body, or None when it cannot
        be known up front (the body is then sent chunked)."""
        if body is None:
            return 0 if method.upper() in _BODY_METHODS else None
        if hasattr(body, 'read'):
            return None         # a file: size unknown
        try:
            return memoryview(body).nbytes
        except TypeError:
            pass
        if isinstance(body, str):
            return len(body)    # sent as Latin-1: one byte per character
        return None

    def __init__(self, host, port=None, timeout=socket._GLOBAL_DEFAULT_TIMEOUT,
                 source_address=None, blocksize=8192, *,
                 max_response_headers=None):
        self.timeout = timeout
        self.source_address = source_address
        self.blocksize = blocksize
        self.max_response_headers = max_response_headers
        self.sock = None
        self._buffer = []           # request line and headers not sent yet
        self._phase = _READY
        self._open_response = None  # a response whose body is still unread
        self._method = None
        self._tunnel_host = None
        self._tunnel_port = None
        self._tunnel_headers = {}
        self._raw_proxy_headers = None

        self.host, self.port = self._get_hostport(host, port)
        self._validate_host(self.host)
        # Tests replace this to connect without a network.
        self._create_connection = socket.create_connection

    def _get_hostport(self, host, port):
        if port is None:
            # "host:port", where an IPv6 address must be bracketed
            colon = host.rfind(':')
            if colon > host.rfind(']'):
                digits = host[colon + 1:]
                host = host[:colon]
                if not digits:
                    port = self.default_port
                else:
                    try:
                        port = int(digits)
                    except ValueError:
                        raise InvalidURL("nonnumeric port: '%s'" % digits) from None
            else:
                port = self.default_port
        if host[:1] == '[' and host[-1:] == ']':
            host = host[1:-1]
        return host, port

    def set_debuglevel(self, level):
        self.debuglevel = level

    def set_tunnel(self, host, port=None, headers=None):
        """Reach host:port through the HTTP proxy this connection goes to,
        with a CONNECT request carrying headers.

        Must be called before the connection is opened.
        """
        if self.sock:
            raise RuntimeError("Can't set up tunnel for established connection")
        self._tunnel_host, self._tunnel_port = self._get_hostport(host, port)
        headers = dict(headers) if headers else {}
        if all(name.lower() != 'host' for name in headers):
            target = self._tunnel_host.encode('idna').decode('ascii')
            headers['Host'] = '%s:%d' % (_bracketed(target), self._tunnel_port)
        self._tunnel_headers = headers

    def _tunnel(self):
        host = self._tunnel_host
        if _contains_url_control(host):
            raise ValueError("Tunnel host can't contain control characters %r"
                             % (host,))
        lines = [b'CONNECT %s:%d %s\r\n' % (_bracketed(host.encode('idna')),
                                             self._tunnel_port,
                                             self._http_vsn_str.encode('ascii'))]
        for name, value in self._tunnel_headers.items():
            lines.append(self._header_line(str(name).encode('latin-1'),
                                           [str(value)]) + b'\r\n')
        lines.append(b'\r\n')
        # one send: some proxies do not cope with a CONNECT split in pieces
        self.send(b''.join(lines))

        reply = self.response_class(self.sock, method=self._method)
        try:
            version, code, message = reply._read_status()
            self._raw_proxy_headers = _read_header_lines(
                reply.fp, self.max_response_headers)
            if self.debuglevel > 0:
                for line in self._raw_proxy_headers:
                    print('header:', line.decode('iso-8859-1').rstrip('\r\n'))
            if code != http.HTTPStatus.OK:
                self.close()
                raise OSError("Tunnel connection failed: %d %s"
                              % (code, message.strip()))
        finally:
            reply.close()

    def get_proxy_response_headers(self):
        """The headers of the proxy's answer to CONNECT (an HTTPMessage),
        or None without a tunnel."""
        if self._raw_proxy_headers is None:
            return None
        return _parse_header_lines(self._raw_proxy_headers)

    def connect(self):
        """Open the connection to the host and port given at creation."""
        sys.audit("http.client.connect", self, self.host, self.port)
        self.sock = self._create_connection((self.host, self.port),
                                            self.timeout, self.source_address)
        # Requests are written in few pieces; do not let Nagle delay them.
        try:
            self.sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        except OSError as exc:
            if exc.errno != errno.ENOPROTOOPT:
                raise
        if self._tunnel_host:
            self._tunnel()

    def close(self):
        """Close the connection and any response still being read."""
        self._phase = _READY
        try:
            sock, self.sock = self.sock, None
            if sock:
                sock.close()
        finally:
            response, self._open_response = self._open_response, None
            if response:
                response.close()

    # -- sending

    def _blocks_of(self, readable):
        # A file's contents in blocksize pieces; text files are encoded as
        # Latin-1.
        textual = self._is_textIO(readable)
        if self.debuglevel > 0 and textual:
            print("encoding file using iso-8859-1")
        while True:
            block = readable.read(self.blocksize)
            if not block:
                return
            yield block.encode('iso-8859-1') if textual else block

    def send(self, data):
        """Send data: bytes-like, a file object or an iterable of bytes-like
        objects."""
        if self.sock is None:
            if not self.auto_open:
                raise NotConnected()
            self.connect()
        if self.debuglevel > 0:
            print("send:", repr(data))
        if hasattr(data, 'read'):
            if self.debuglevel > 0:
                print("sending a readable")
            for block in self._blocks_of(data):
                sys.audit("http.client.send", self, block)
                self.sock.sendall(block)
            return
        sys.audit("http.client.send", self, data)
        try:
            self.sock.sendall(data)
        except TypeError:
            if not isinstance(data, collections.abc.Iterable):
                raise TypeError("data should be a bytes-like object or an "
                                "iterable, got %r" % type(data)) from None
            for piece in data:
                self.sock.sendall(piece)

    def _output(self, s):
        """Queue one line (bytes) of the request head."""
        self._buffer.append(s)

    def _send_output(self, message_body=None, encode_chunked=False):
        """Send the queued head, the blank line, then the body."""
        self._buffer.extend((b'', b''))
        head = b'\r\n'.join(self._buffer)
        del self._buffer[:]
        self.send(head)
        if message_body is None:
            return

        if hasattr(message_body, 'read'):
            if self.debuglevel > 0:
                print("reading a readable")
            pieces = self._blocks_of(message_body)
        else:
            try:
                memoryview(message_body)
            except TypeError:
                try:
                    pieces = iter(message_body)
                except TypeError:
                    raise TypeError("message_body should be a bytes-like "
                                    "object or an iterable, got %r"
                                    % type(message_body)) from None
            else:
                pieces = (message_body,)

        # Chunked coding only exists in HTTP/1.1.
        chunked = encode_chunked and self._http_vsn == 11
        for piece in pieces:
            if not piece:
                # an empty chunk would end a chunked body early
                if self.debuglevel > 0:
                    print("Zero length chunk ignored")
                continue
            self.send(b'%X\r\n%s\r\n' % (len(piece), piece) if chunked else piece)
        if chunked:
            self.send(b'0\r\n\r\n')

    # -- composing a request

    def _forget_finished_response(self):
        if self._open_response and self._open_response.isclosed():
            self._open_response = None

    def putrequest(self, method, url, skip_host=False,
                   skip_accept_encoding=False):
        """Begin a request with its request line.

        Unless told to skip them, also adds Host (from url when it is
        absolute, else from the connection) and "Accept-Encoding: identity".
        """
        self._forget_finished_response()
        if self._phase != _READY:
            raise CannotSendRequest(self._phase)
        self._phase = _WRITING

        self._validate_method(method)
        self._method = method
        url = url or '/'
        self._validate_path(url)
        self._output(self._encode_request(
            '%s %s %s' % (method, url, self._http_vsn_str)))

        if self._http_vsn != 11:
            return      # HTTP/1.0 needs neither header
        if not skip_host:
            self.putheader('Host', self._host_header_value(url))
        if not skip_accept_encoding:
            self.putheader('Accept-Encoding', 'identity')

    def _host_header_value(self, url):
        netloc = urlsplit(url)[1] if url.startswith('http') else ''
        if netloc:
            return _without_zone_id(_host_bytes(netloc))
        if self._tunnel_host:
            host, port = self._tunnel_host, self._tunnel_port
        else:
            host, port = self.host, self.port
        name = _host_bytes(host).partition(b'%')[0]
        if b':' in name:
            name = b'[%s]' % name       # an IPv6 address
        if port == self.default_port:
            return name
        return '%s:%s' % (name.decode('ascii'), port)

    def _encode_request(self, request):
        # The request line must be plain ASCII.
        return request.encode('ascii')

    def _validate_method(self, method):
        found = _contains_method_control(method)
        if found:
            raise ValueError("method can't contain control characters. %r "
                             "(found at least %r)" % (method, found.group()))

    def _validate_path(self, url):
        found = _contains_url_control(url)
        if found:
            raise InvalidURL("URL can't contain control characters. %r "
                             "(found at least %r)" % (url, found.group()))

    def _validate_host(self, host):
        found = _contains_url_control(host)
        if found:
            raise InvalidURL("URL can't contain control characters. %r "
                             "(found at least %r)" % (host, found.group()))

    @staticmethod
    def _header_line(name, values):
        # One header as bytes; name is bytes already.  Text values go out
        # as Latin-1, integers in decimal; several values become folded
        # continuation lines.
        if not _is_good_header_name(name):
            raise ValueError('Invalid header name %r' % (name,))
        encoded = []
        for value in values:
            if hasattr(value, 'encode'):
                value = value.encode('latin-1')
            elif isinstance(value, int):
                value = str(value).encode('ascii')
            if _has_bad_line_break(value):
                raise ValueError('Invalid header value %r' % (value,))
            encoded.append(value)
        return name + b': ' + b'\r\n\t'.join(encoded)

    def putheader(self, header, *values):
        """Add a header to the request being composed."""
        if self._phase != _WRITING:
            raise CannotSendHeader()
        if hasattr(header, 'encode'):
            header = header.encode('ascii')
        self._output(self._header_line(header, values))

    def endheaders(self, message_body=None, *, encode_chunked=False):
        """Finish the head and send the request, with message_body if given
        (encode_chunked: send the body with chunked transfer coding)."""
        if self._phase != _WRITING:
            raise CannotSendHeader()
        self._phase = _WAITING
        self._send_output(message_body, encode_chunked=encode_chunked)

    def request(self, method, url, body=None, headers={}, *,
                encode_chunked=False):
        """Send a whole request: method, url, optional body and headers.

        Without a Content-Length or Transfer-Encoding header, the body's
        length is sent if it can be known, else it is sent chunked.
        """
        self._send_request(method, url, body, headers, encode_chunked)

    def _send_request(self, method, url, body, headers, encode_chunked):
        given = {name.lower() for name in headers}
        self.putrequest(method, url,
                        **{flag: 1 for flag, name in
                           (('skip_host', 'host'),
                            ('skip_accept_encoding', 'accept-encoding'))
                           if name in given})

        # The caller's own framing headers win; otherwise pick one.
        if 'content-length' in given:
            encode_chunked = False
        elif 'transfer-encoding' not in given:
            encode_chunked = False
            length = self._get_content_length(body, method)
            if length is not None:
                self.putheader('Content-Length', str(length))
            elif body is not None:
                if self.debuglevel > 0:
                    print('Unable to determine size of %r' % body)
                encode_chunked = True
                self.putheader('Transfer-Encoding', 'chunked')

        for name, value in headers.items():
            self.putheader(name, value)
        if isinstance(body, str):
            body = _encode_latin1(body, 'body')
        self.endheaders(body, encode_chunked=encode_chunked)

    def getresponse(self):
        """Read the server's response to the request that was sent.

        The connection is ready for the next request once the body has
        been read completely (or right away if the server closes it).
        """
        self._forget_finished_response()
        if self._phase != _WAITING or self._open_response:
            raise ResponseNotReady(self._phase)

        if self.debuglevel > 0:
            response = self.response_class(self.sock, self.debuglevel,
                                           method=self._method)
        else:
            response = self.response_class(self.sock, method=self._method)
        try:
            try:
                if self.max_response_headers is None:
                    response.begin()
                else:
                    response.begin(_max_headers=self.max_response_headers)
            except ConnectionError:
                self.close()
                raise
            assert response.will_close != _UNKNOWN
            self._phase = _READY
            if response.will_close:
                # the response reads the rest through its own file object
                self.close()
            else:
                self._open_response = response
            return response
        except:
            response.close()
            raise


try:
    import ssl
except ImportError:
    pass
else:
    class HTTPSConnection(HTTPConnection):
        """An HTTPConnection over TLS (context: an ssl.SSLContext)."""

        default_port = HTTPS_PORT

        def __init__(self, host, port=None, *,
                     timeout=socket._GLOBAL_DEFAULT_TIMEOUT,
                     source_address=None, context=None, blocksize=8192,
                     max_response_headers=None):
            super().__init__(host, port, timeout, source_address,
                             blocksize=blocksize,
                             max_response_headers=max_response_headers)
            self._context = (_create_https_context(self._http_vsn)
                             if context is None else context)

        def connect(self):
            super().connect()
            # Through a tunnel the certificate is the final server's.
            self.sock = self._context.wrap_socket(
                self.sock, server_hostname=self._tunnel_host or self.host)

    __all__.append("HTTPSConnection")


# ------------------------------------------------------------ exceptions

class HTTPException(Exception):
    """Base class of this module's exceptions."""


class NotConnected(HTTPException):
    pass


class InvalidURL(HTTPException):
    pass


class UnknownProtocol(HTTPException):
    def __init__(self, version):
        self.args = (version,)
        self.version = version


class UnknownTransferEncoding(HTTPException):
    pass


class UnimplementedFileMode(HTTPException):
    pass


class IncompleteRead(HTTPException):
    """The body ended early: `partial` holds what was read, `expected` how
    many more bytes were due (None if unknown)."""

    def __init__(self, partial, expected=None):
        self.args = (partial,)
        self.partial = partial
        self.expected = expected

    def __repr__(self):
        tail = '' if self.expected is None else ', %i more expected' % self.expected
        return '%s(%i bytes read%s)' % (type(self).__name__, len(self.partial), tail)

    __str__ = object.__str__


class ImproperConnectionState(HTTPException):
    pass


class CannotSendRequest(ImproperConnectionState):
    pass


class CannotSendHeader(ImproperConnectionState):
    pass


class ResponseNotReady(ImproperConnectionState):
    pass


class BadStatusLine(HTTPException):
    def __init__(self, line):
        if not line:
            line = repr(line)
        self.args = (line,)
        self.line = line


class LineTooLong(HTTPException):
    def __init__(self, line_type):
        HTTPException.__init__(self, "got more than %d bytes when reading %s"
                               % (_MAXLINE, line_type))


class RemoteDisconnected(ConnectionResetError, BadStatusLine):
    """The server closed the connection before sending a status line."""

    def __init__(self, *pos, **kw):
        BadStatusLine.__init__(self, "")
        ConnectionResetError.__init__(self, *pos, **kw)


# Old name for the base class.
error = HTTPException
