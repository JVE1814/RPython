"""HTTP/1.1 client protocol (RFC 9110/9112).

HTTPConnection sends requests over one (persistent) connection and returns
HTTPResponse objects:

    conn = HTTPConnection('www.example.com')
    conn.request('GET', '/')
    resp = conn.getresponse()
    body = resp.read()

A connection goes through three states: idle, "request started" (after
putrequest(), while headers may be added with putheader()) and "request
sent" (after endheaders()).  getresponse() is only allowed in the last
state and brings the connection back to idle.  request() performs the
whole sequence at once.  HTTPSConnection is available when the ssl module
is.
"""

import collections.abc
import email.parser
import email.message
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

_NOT_YET_KNOWN = 'UNKNOWN'  # attribute values before the status line is read

# connection states
_STATE_IDLE = 'Idle'
_STATE_STARTED = 'Request-started'
_STATE_SENT = 'Request-sent'

# Status code constants (http.client.OK, ...) for backward compatibility.
globals().update(http.HTTPStatus.__members__)

responses = {status: status.phrase
             for status in http.HTTPStatus.__members__.values()}

# Limits protecting against misbehaving servers.
_MAXLINE = 65536
_MAXHEADERS = 100
_MAXINTERIMRESPONSES = 100

# Requests with these methods get "Content-Length: 0" for an empty body.
_METHODS_EXPECTING_BODY = {'PATCH', 'POST', 'PUT'}

# Reads larger than this are done in pieces so that a huge announced
# Content-Length cannot make us allocate a huge buffer up front.
_MAX_SINGLE_READ = 1 << 20

_URL_CONTROL_CHARS = re.compile('[\x00-\x20\x7f]')
_METHOD_CONTROL_CHARS = re.compile('[\x00-\x1f]')
_ASCII_SPACE = frozenset(b' \t\n\r\x0b\x0c')


def _valid_header_name(name):
    """A header name must be non-empty, must not start with whitespace or
    ':' and must not contain ':', CR or LF."""
    if not name or name[0] in _ASCII_SPACE or name[0] == 0x3A:
        return False
    for byte in name:
        if byte in (0x3A, 0x0D, 0x0A):
            return False
    return True


def _bad_header_value(value):
    """CR and LF are only allowed as part of an obsolete line folding
    (followed by a space or a tab)."""
    n = len(value)
    for i, byte in enumerate(value):
        if byte == 0x0A:
            if i + 1 >= n or value[i + 1] not in (0x20, 0x09):
                return True
        elif byte == 0x0D:
            if i + 1 >= n or value[i + 1] not in (0x20, 0x09, 0x0A):
                return True
    return False


def _as_latin1(data, name='data'):
    """Encode *data* as Latin-1, with a helpful message on failure."""
    try:
        return data.encode('latin-1')
    except UnicodeEncodeError as err:
        raise UnicodeEncodeError(
            err.encoding, err.object, err.start, err.end,
            f"{name.title()} ({data[err.start:err.end]!r:.20}) is not valid "
            f"Latin-1. Use {name}.encode('utf-8') if you want to send it "
            f"encoded in UTF-8.") from None


def _strip_zone(host_bytes):
    """Drop an IPv6 zone id ('%eth0') from a host for the Host header."""
    addr, pct, _ = host_bytes.partition(b'%')
    if pct and addr.startswith(b'[') and not addr.endswith(b']'):
        addr += b']'
    return addr


class HTTPMessage(email.message.Message):
    """The headers of an HTTP message."""

    def getallmatchingheaders(self, name):
        """Return the header lines (as str, folded continuation lines
        included) whose field name is *name*, case-insensitively."""
        wanted = name.lower()
        return [f"{key}: {value}" for key, value in self.raw_items()
                if key.lower() == wanted]


def _collect_header_lines(fp, max_headers=None, what='headers'):
    """Read raw header lines up to and excluding the blank line."""
    limit = _MAXHEADERS if max_headers is None else max_headers
    lines = []
    while True:
        line = fp.readline(_MAXLINE + 1)
        if len(line) > _MAXLINE:
            raise LineTooLong('header line')
        if line in (b'\r\n', b'\n', b''):
            return lines
        lines.append(line)
        if len(lines) > limit:
            raise HTTPException(f'got more than {limit} {what}')


def _message_from_lines(header_lines, _class=HTTPMessage):
    text = b''.join(header_lines).decode('iso-8859-1')
    return email.parser.Parser(_class=_class).parsestr(text)


def parse_headers(fp, _class=HTTPMessage, *, _max_headers=None):
    """Read headers from the binary file *fp* and return a message object
    (an HTTPMessage by default)."""
    return _message_from_lines(_collect_header_lines(fp, _max_headers), _class)


class HTTPResponse(io.BufferedIOBase):
    """A response read from an HTTPConnection (or a socket)."""

    def __init__(self, sock, debuglevel=0, method=None, url=None):
        # Read through a buffered file; the socket must not be read
        # directly afterwards or buffered data would be lost.
        self.fp = sock.makefile('rb')
        self.debuglevel = debuglevel
        self._method = method
        self._max_headers = None

        self.headers = self.msg = None
        self.version = _NOT_YET_KNOWN
        self.status = _NOT_YET_KNOWN
        self.reason = _NOT_YET_KNOWN

        self.chunked = _NOT_YET_KNOWN
        self.chunk_left = _NOT_YET_KNOWN
        self.length = _NOT_YET_KNOWN
        self.will_close = _NOT_YET_KNOWN
        self.url = url

    def _read_status(self):
        raw = self.fp.readline(_MAXLINE + 1)
        line = str(raw, 'iso-8859-1')
        if len(line) > _MAXLINE:
            raise LineTooLong('status line')
        if self.debuglevel > 0:
            print('reply:', repr(line))
        if not line:
            raise RemoteDisconnected(
                'Remote end closed connection without response')
        pieces = line.split(None, 2)
        version = pieces[0] if pieces else ''
        status = pieces[1] if len(pieces) > 1 else ''
        reason = pieces[2] if len(pieces) > 2 else ''
        if not version.startswith('HTTP/'):
            self._close_conn()
            raise BadStatusLine(line)
        if len(status) != 3 or not (status.isascii() and status.isdigit()):
            raise BadStatusLine(line)
        code = int(status)
        if not 100 <= code <= 999:
            raise BadStatusLine(line)
        return version, code, reason

    def begin(self, *, _max_headers=None):
        if self.headers is not None:
            return          # already done
        self._max_headers = _max_headers
        interim = 0
        while True:
            version, status, reason = self._read_status()
            if status != CONTINUE:
                break
            interim += 1
            if interim > _MAXINTERIMRESPONSES:
                raise HTTPException(f'got more than {_MAXINTERIMRESPONSES} '
                                    f'interim responses')
            skipped = _collect_header_lines(self.fp, _max_headers)
            if self.debuglevel > 0:
                for line in skipped:
                    print('header:', line.decode('iso-8859-1').rstrip())

        self.code = self.status = status
        self.reason = reason.strip()
        if version in ('HTTP/1.0', 'HTTP/0.9'):
            self.version = 10
        elif version.startswith('HTTP/1.'):
            self.version = 11
        else:
            raise UnknownProtocol(version)

        self.headers = self.msg = parse_headers(self.fp,
                                                _max_headers=_max_headers)
        if self.debuglevel > 0:
            for name, value in self.headers.items():
                print('header:', name + ':', value)

        te = self.headers.get('transfer-encoding')
        if te and te.lower() == 'chunked':
            self.chunked = True
            self.chunk_left = None
        else:
            self.chunked = False

        self.will_close = self._check_close()

        self.length = None
        length = self.headers.get('content-length')
        if length and not self.chunked:
            try:
                self.length = int(length)
            except ValueError:
                self.length = None
            else:
                if self.length < 0:
                    self.length = None

        if (status in (NO_CONTENT, NOT_MODIFIED) or 100 <= status < 200 or
                self._method == 'HEAD'):
            self.length = 0

        if not self.will_close and not self.chunked and self.length is None:
            self.will_close = True

    def _check_close(self):
        conn = self.headers.get('connection')
        if self.version == 11:
            return bool(conn and 'close' in conn.lower())
        # HTTP/1.0: keep the connection only if asked to
        if conn and 'keep-alive' in conn.lower():
            return False
        if self.headers.get('keep-alive'):
            return False
        pconn = self.headers.get('proxy-connection')
        if pconn and 'keep-alive' in pconn.lower():
            return False
        return True

    def _close_conn(self):
        fp = self.fp
        self.fp = None
        fp.close()

    def close(self):
        try:
            super().close()
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
        """True when the connection to the server is closed."""
        return self.fp is None

    def _body_done(self):
        return self.fp is None or self._method == 'HEAD'

    def read(self, amt=None):
        """Read and return up to *amt* bytes of the body (all if None)."""
        if self.fp is None:
            return b''
        if self._method == 'HEAD':
            self._close_conn()
            return b''
        if amt is not None and amt < 0:
            amt = None
        if self.chunked:
            return self._read_chunked(amt)
        if amt is not None:
            if self.length is not None and amt > self.length:
                amt = self.length
            data = self.fp.read(amt)
            if not data and amt:
                self._close_conn()
            elif self.length is not None:
                self.length -= len(data)
                if not self.length:
                    self._close_conn()
            return data
        if self.length is None:
            data = self.fp.read()
        else:
            try:
                data = self._safe_read(self.length)
            except IncompleteRead:
                self._close_conn()
                raise
            self.length = 0
        self._close_conn()
        return data

    def readinto(self, b):
        """Read into the writable buffer *b*; return the number of bytes."""
        if self.fp is None:
            return 0
        if self._method == 'HEAD':
            self._close_conn()
            return 0
        view = memoryview(b).cast('B')
        data = self.read(len(view)) if len(view) else b''
        view[:len(data)] = data
        return len(data)

    def _read_next_chunk_size(self):
        line = self.fp.readline(_MAXLINE + 1)
        if len(line) > _MAXLINE:
            raise LineTooLong('chunk size')
        size = line.split(b';', 1)[0]
        try:
            return int(size, 16)
        except ValueError:
            self._close_conn()
            raise

    def _read_and_discard_trailer(self):
        _collect_header_lines(self.fp, self._max_headers, 'trailers')

    def _get_chunk_left(self):
        """Bytes left in the current chunk, reading the next chunk header
        if needed; None at the end of the body."""
        left = self.chunk_left
        if not left:
            if left is not None:
                self._safe_read(2)      # CRLF ending the previous chunk
            try:
                left = self._read_next_chunk_size()
            except ValueError:
                raise IncompleteRead(b'')
            if left == 0:
                self._read_and_discard_trailer()
                self._close_conn()
                left = None
            self.chunk_left = left
        return left

    def _read_chunked(self, amt=None):
        parts = []
        try:
            while True:
                left = self._get_chunk_left()
                if left is None:
                    break
                if amt is not None and amt <= left:
                    parts.append(self._safe_read(amt))
                    self.chunk_left = left - amt
                    break
                parts.append(self._safe_read(left))
                if amt is not None:
                    amt -= left
                self.chunk_left = 0
            return b''.join(parts)
        except IncompleteRead as exc:
            raise IncompleteRead(b''.join(parts)) from exc

    def _safe_read(self, amt):
        """Read exactly *amt* bytes or raise IncompleteRead."""
        if amt <= _MAX_SINGLE_READ:
            data = self.fp.read(amt)
            if len(data) < amt:
                raise IncompleteRead(data, amt - len(data))
            return data
        parts = []
        remaining = amt
        while remaining > 0:
            piece = self.fp.read(min(remaining, _MAX_SINGLE_READ))
            if not piece:
                data = b''.join(parts)
                raise IncompleteRead(data, amt - len(data))
            parts.append(piece)
            remaining -= len(piece)
        return b''.join(parts)

    def _safe_readinto(self, b):
        data = self._safe_read(len(b))
        b[:len(data)] = data
        return len(data)

    def read1(self, n=-1):
        """Read up to *n* bytes with at most one read of the socket."""
        if self.fp is None or self._method == 'HEAD':
            return b''
        if self.chunked:
            return self._read1_chunked(n)
        if self.length is not None and (n < 0 or n > self.length):
            n = self.length
        data = self.fp.read1(n)
        if not data and n:
            self._close_conn()
        elif self.length is not None:
            self.length -= len(data)
            if not self.length:
                self._close_conn()
        return data

    def peek(self, n=-1):
        if self.fp is None or self._method == 'HEAD':
            return b''
        if self.chunked:
            return self._peek_chunked(n)
        return self.fp.peek(n)

    def readline(self, limit=-1):
        if self.fp is None or self._method == 'HEAD':
            return b''
        if limit is None:
            limit = -1
        if self.chunked:
            return self._readline_chunked(limit)
        if self.length is not None and (limit < 0 or limit > self.length):
            limit = self.length
        data = self.fp.readline(limit)
        if not data and limit:
            self._close_conn()
        elif self.length is not None:
            self.length -= len(data)
            if not self.length:
                self._close_conn()
        return data

    def _readline_chunked(self, limit):
        out = bytearray()
        while limit < 0 or len(out) < limit:
            left = self._get_chunk_left()
            if left is None:
                break
            want = left if limit < 0 else min(left, limit - len(out))
            line = self.fp.readline(want)
            if not line:
                raise IncompleteRead(bytes(out))
            self.chunk_left = left - len(line)
            out += line
            if line.endswith(b'\n'):
                break
        return bytes(out)

    def _read1_chunked(self, n):
        left = self._get_chunk_left()
        if left is None or n == 0:
            return b''
        if not 0 <= n <= left:
            n = left
        data = self.fp.read1(n)
        self.chunk_left -= len(data)
        if not data:
            raise IncompleteRead(b'')
        return data

    def _peek_chunked(self, n):
        try:
            left = self._get_chunk_left()
        except IncompleteRead:
            return b''
        if left is None:
            return b''
        return self.fp.peek(left)[:left]

    def fileno(self):
        return self.fp.fileno()

    def getheader(self, name, default=None):
        """Return the value of header *name* (all values joined with ', '),
        or *default*."""
        if self.headers is None:
            raise ResponseNotReady()
        values = self.headers.get_all(name) or default
        if isinstance(values, str) or not hasattr(values, '__iter__'):
            return values
        return ', '.join(values)

    def getheaders(self):
        """Return a list of (name, value) pairs."""
        if self.headers is None:
            raise ResponseNotReady()
        return list(self.headers.items())

    def __iter__(self):
        return self

    def info(self):
        """The headers of the response (an HTTPMessage)."""
        return self.headers

    def geturl(self):
        """The URL of the resource (set by urllib.request)."""
        return self.url

    def getcode(self):
        """The HTTP status code."""
        return self.status


def _create_https_context(http_version):
    context = ssl._create_default_https_context()
    if http_version == 11:
        context.set_alpn_protocols(['http/1.1'])
    if context.post_handshake_auth is not None:
        context.post_handshake_auth = True
    return context


class HTTPConnection:
    """One connection to an HTTP server."""

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
        """The Content-Length to send for *body*, or None if it cannot be
        known in advance (chunked encoding is used then)."""
        if body is None:
            if method.upper() in _METHODS_EXPECTING_BODY:
                return 0
            return None
        if hasattr(body, 'read'):
            return None
        try:
            return memoryview(body).nbytes
        except TypeError:
            pass
        if isinstance(body, str):
            return len(body)
        return None

    def __init__(self, host, port=None, timeout=socket._GLOBAL_DEFAULT_TIMEOUT,
                 source_address=None, blocksize=8192, *,
                 max_response_headers=None):
        self.timeout = timeout
        self.source_address = source_address
        self.blocksize = blocksize
        self.max_response_headers = max_response_headers
        self.sock = None
        self._buffer = []
        self.__response = None
        self.__state = _STATE_IDLE
        self._method = None
        self._tunnel_host = None
        self._tunnel_port = None
        self._tunnel_headers = {}
        self._raw_proxy_headers = None

        self.host, self.port = self._get_hostport(host, port)
        self._validate_host(self.host)
        # tests replace this to avoid real network connections
        self._create_connection = socket.create_connection

    def set_tunnel(self, host, port=None, headers=None):
        """Tunnel through an HTTP proxy with CONNECT.

        The connection's own host/port are those of the proxy; *host* and
        *port* name the final destination.  *headers* are sent with the
        CONNECT request.
        """
        if self.sock:
            raise RuntimeError("Can't set up tunnel for established connection")
        self._tunnel_host, self._tunnel_port = self._get_hostport(host, port)
        headers = dict(headers) if headers else {}
        if not any(name.lower() == 'host' for name in headers):
            encoded = self._tunnel_host.encode('idna').decode('ascii')
            headers['Host'] = '%s:%d' % (self._wrap_ipv6_str(encoded),
                                         self._tunnel_port)
        self._tunnel_headers = headers

    def _get_hostport(self, host, port):
        if port is None:
            colon = host.rfind(':')
            bracket = host.rfind(']')       # IPv6 addresses are bracketed
            if colon > bracket:
                text = host[colon + 1:]
                if text == '':
                    port = self.default_port
                else:
                    try:
                        port = int(text)
                    except ValueError:
                        raise InvalidURL(f"nonnumeric port: '{text}'") from None
                host = host[:colon]
            else:
                port = self.default_port
        if host and host[0] == '[' and host[-1] == ']':
            host = host[1:-1]
        return host, port

    def set_debuglevel(self, level):
        self.debuglevel = level

    @staticmethod
    def _wrap_ipv6(host):
        if b':' in host and not host.startswith(b'['):
            return b'[' + host + b']'
        return host

    @staticmethod
    def _wrap_ipv6_str(host):
        if ':' in host and not host.startswith('['):
            return '[' + host + ']'
        return host

    def _tunnel(self):
        host = self._tunnel_host
        if _URL_CONTROL_CHARS.search(host):
            raise ValueError("Tunnel host can't contain control characters "
                             f"{host!r}")
        request = [b'CONNECT %s:%d %s\r\n' % (
            self._wrap_ipv6(host.encode('idna')), self._tunnel_port,
            self._http_vsn_str.encode('ascii'))]
        for name, value in self._tunnel_headers.items():
            name_b = str(name).encode('latin-1')
            value_b = str(value).encode('latin-1')
            if not _valid_header_name(name_b):
                raise ValueError(f'Invalid header name {name_b!r}')
            if _bad_header_value(value_b):
                raise ValueError(f'Invalid header value {value_b!r}')
            request.append(name_b + b': ' + value_b + b'\r\n')
        request.append(b'\r\n')
        # one send: some proxies dislike a CONNECT split over packets
        self.send(b''.join(request))

        response = self.response_class(self.sock, method=self._method)
        try:
            version, code, message = response._read_status()
            self._raw_proxy_headers = _collect_header_lines(
                response.fp, self.max_response_headers)
            if self.debuglevel > 0:
                for line in self._raw_proxy_headers:
                    print('header:', line.decode('iso-8859-1').rstrip('\r\n'))
            if code != http.HTTPStatus.OK:
                self.close()
                raise OSError(f'Tunnel connection failed: {code} '
                              f'{message.strip()}')
        finally:
            response.close()

    def get_proxy_response_headers(self):
        """Headers of the proxy's answer to CONNECT, or None if no tunnel
        was set up."""
        if self._raw_proxy_headers is None:
            return None
        return _message_from_lines(self._raw_proxy_headers)

    def connect(self):
        """Connect to the host and port given to the constructor."""
        sys.audit('http.client.connect', self, self.host, self.port)
        self.sock = self._create_connection(
            (self.host, self.port), self.timeout, self.source_address)
        try:
            self.sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        except OSError as exc:
            if exc.errno != errno.ENOPROTOOPT:
                raise
        if self._tunnel_host:
            self._tunnel()

    def close(self):
        """Close the connection (and a response still being read)."""
        self.__state = _STATE_IDLE
        try:
            sock = self.sock
            if sock:
                self.sock = None
                sock.close()
        finally:
            response = self.__response
            if response:
                self.__response = None
                response.close()

    def send(self, data):
        """Send *data* (bytes-like, a file object or an iterable of
        bytes-like objects) to the server."""
        if self.sock is None:
            if self.auto_open:
                self.connect()
            else:
                raise NotConnected()
        if self.debuglevel > 0:
            print('send:', repr(data))
        if hasattr(data, 'read'):
            if self.debuglevel > 0:
                print('sending a readable')
            encode = self._is_textIO(data)
            if encode and self.debuglevel > 0:
                print('encoding file using iso-8859-1')
            while True:
                block = data.read(self.blocksize)
                if not block:
                    break
                if encode:
                    block = block.encode('iso-8859-1')
                sys.audit('http.client.send', self, block)
                self.sock.sendall(block)
            return
        sys.audit('http.client.send', self, data)
        try:
            self.sock.sendall(data)
        except TypeError:
            if isinstance(data, collections.abc.Iterable):
                for piece in data:
                    self.sock.sendall(piece)
            else:
                raise TypeError('data should be a bytes-like object or an '
                                f'iterable, got {type(data)!r}') from None

    def _output(self, s):
        """Queue a header line (bytes) for the next _send_output()."""
        self._buffer.append(s)

    def _read_readable(self, readable):
        if self.debuglevel > 0:
            print('reading a readable')
        encode = self._is_textIO(readable)
        if encode and self.debuglevel > 0:
            print('encoding file using iso-8859-1')
        while True:
            block = readable.read(self.blocksize)
            if not block:
                break
            if encode:
                block = block.encode('iso-8859-1')
            yield block

    def _send_output(self, message_body=None, encode_chunked=False):
        """Send the queued request line and headers, then the body."""
        self._buffer.extend((b'', b''))
        head = b'\r\n'.join(self._buffer)
        del self._buffer[:]
        self.send(head)

        if message_body is None:
            return
        if hasattr(message_body, 'read'):
            chunks = self._read_readable(message_body)
        else:
            try:
                memoryview(message_body)
            except TypeError:
                try:
                    chunks = iter(message_body)
                except TypeError:
                    raise TypeError('message_body should be a bytes-like '
                                    'object or an iterable, got '
                                    f'{type(message_body)!r}') from None
            else:
                chunks = (message_body,)

        chunked = encode_chunked and self._http_vsn == 11
        for chunk in chunks:
            if not chunk:
                if self.debuglevel > 0:
                    print('Zero length chunk ignored')
                continue
            if chunked:
                chunk = b'%X\r\n%s\r\n' % (len(chunk), chunk)
            self.send(chunk)
        if chunked:
            self.send(b'0\r\n\r\n')

    def putrequest(self, method, url, skip_host=False,
                   skip_accept_encoding=False):
        """Start a request: send the request line.

        A Host header and "Accept-Encoding: identity" are added unless
        *skip_host* / *skip_accept_encoding* are true.
        """
        if self.__response and self.__response.isclosed():
            self.__response = None
        if self.__state == _STATE_IDLE:
            self.__state = _STATE_STARTED
        else:
            raise CannotSendRequest(self.__state)

        self._validate_method(method)
        self._method = method
        url = url or '/'
        self._validate_path(url)
        self._output(self._encode_request(f'{method} {url} {self._http_vsn_str}'))

        if self._http_vsn != 11:
            return
        if not skip_host:
            netloc = ''
            if url.startswith('http'):
                netloc = urlsplit(url)[1]
            if netloc:
                try:
                    netloc_b = netloc.encode('ascii')
                except UnicodeEncodeError:
                    netloc_b = netloc.encode('idna')
                self.putheader('Host', _strip_zone(netloc_b))
            else:
                if self._tunnel_host:
                    host, port = self._tunnel_host, self._tunnel_port
                else:
                    host, port = self.host, self.port
                try:
                    host_b = host.encode('ascii')
                except UnicodeEncodeError:
                    host_b = host.encode('idna')
                host_b = host_b.partition(b'%')[0]
                if b':' in host_b:
                    host_b = b'[' + host_b + b']'
                if port == self.default_port:
                    self.putheader('Host', host_b)
                else:
                    self.putheader('Host', f"{host_b.decode('ascii')}:{port}")
        if not skip_accept_encoding:
            self.putheader('Accept-Encoding', 'identity')

    def _encode_request(self, request):
        return request.encode('ascii')

    def _validate_method(self, method):
        match = _METHOD_CONTROL_CHARS.search(method)
        if match:
            raise ValueError(f"method can't contain control characters. "
                             f"{method!r} (found at least {match.group()!r})")

    def _validate_path(self, url):
        match = _URL_CONTROL_CHARS.search(url)
        if match:
            raise InvalidURL(f"URL can't contain control characters. "
                             f"{url!r} (found at least {match.group()!r})")

    def _validate_host(self, host):
        match = _URL_CONTROL_CHARS.search(host)
        if match:
            raise InvalidURL(f"URL can't contain control characters. "
                             f"{host!r} (found at least {match.group()!r})")

    def putheader(self, header, *values):
        """Queue a header line; several values are joined by folding."""
        if self.__state != _STATE_STARTED:
            raise CannotSendHeader()
        if hasattr(header, 'encode'):
            header = header.encode('ascii')
        if not _valid_header_name(header):
            raise ValueError(f'Invalid header name {header!r}')
        encoded = []
        for value in values:
            if hasattr(value, 'encode'):
                value = value.encode('latin-1')
            elif isinstance(value, int):
                value = str(value).encode('ascii')
            if _bad_header_value(value):
                raise ValueError(f'Invalid header value {value!r}')
            encoded.append(value)
        self._output(header + b': ' + b'\r\n\t'.join(encoded))

    def endheaders(self, message_body=None, *, encode_chunked=False):
        """Finish the headers and send the request (with *message_body*)."""
        if self.__state == _STATE_STARTED:
            self.__state = _STATE_SENT
        else:
            raise CannotSendHeader()
        self._send_output(message_body, encode_chunked=encode_chunked)

    def request(self, method, url, body=None, headers={}, *,
                encode_chunked=False):
        """Send a complete request."""
        self._send_request(method, url, body, headers, encode_chunked)

    def _send_request(self, method, url, body, headers, encode_chunked):
        names = frozenset(name.lower() for name in headers)
        skips = {}
        if 'host' in names:
            skips['skip_host'] = 1
        if 'accept-encoding' in names:
            skips['skip_accept_encoding'] = 1
        self.putrequest(method, url, **skips)

        if 'content-length' not in names:
            if 'transfer-encoding' not in names:
                # we decide how the body is framed
                encode_chunked = False
                length = self._get_content_length(body, method)
                if length is None:
                    if body is not None:
                        if self.debuglevel > 0:
                            print(f'Unable to determine size of {body!r}')
                        encode_chunked = True
                        self.putheader('Transfer-Encoding', 'chunked')
                else:
                    self.putheader('Content-Length', str(length))
        else:
            encode_chunked = False

        for name, value in headers.items():
            self.putheader(name, value)
        if isinstance(body, str):
            body = _as_latin1(body, 'body')
        self.endheaders(body, encode_chunked=encode_chunked)

    def getresponse(self):
        """Return the response to the request just sent."""
        if self.__response and self.__response.isclosed():
            self.__response = None
        if self.__state != _STATE_SENT or self.__response:
            raise ResponseNotReady(self.__state)

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
            assert response.will_close != _NOT_YET_KNOWN
            self.__state = _STATE_IDLE
            if response.will_close:
                # the response owns the socket's file now
                self.close()
            else:
                self.__response = response
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
        """An HTTPConnection that uses TLS."""

        default_port = HTTPS_PORT

        def __init__(self, host, port=None, *,
                     timeout=socket._GLOBAL_DEFAULT_TIMEOUT,
                     source_address=None, context=None, blocksize=8192,
                     max_response_headers=None):
            super().__init__(host, port, timeout, source_address,
                             blocksize=blocksize,
                             max_response_headers=max_response_headers)
            if context is None:
                context = _create_https_context(self._http_vsn)
            self._context = context

        def connect(self):
            super().connect()
            server_hostname = self._tunnel_host or self.host
            self.sock = self._context.wrap_socket(
                self.sock, server_hostname=server_hostname)

    __all__.append('HTTPSConnection')


class HTTPException(Exception):
    pass


class NotConnected(HTTPException):
    pass


class InvalidURL(HTTPException):
    pass


class UnknownProtocol(HTTPException):
    def __init__(self, version):
        self.args = version,
        self.version = version


class UnknownTransferEncoding(HTTPException):
    pass


class UnimplementedFileMode(HTTPException):
    pass


class IncompleteRead(HTTPException):
    def __init__(self, partial, expected=None):
        self.args = partial,
        self.partial = partial
        self.expected = expected

    def __repr__(self):
        more = ''
        if self.expected is not None:
            more = f', {self.expected} more expected'
        return f'{self.__class__.__name__}({len(self.partial)} bytes read{more})'

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
        self.args = line,
        self.line = line


class LineTooLong(HTTPException):
    def __init__(self, line_type):
        HTTPException.__init__(
            self, f'got more than {_MAXLINE} bytes when reading {line_type}')


class RemoteDisconnected(ConnectionResetError, BadStatusLine):
    def __init__(self, *pos, **kw):
        BadStatusLine.__init__(self, '')
        ConnectionResetError.__init__(self, *pos, **kw)


# Backwards compatible alias.
error = HTTPException
