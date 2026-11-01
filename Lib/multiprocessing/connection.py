#
# multiprocessing.connection: message-oriented connections over pipes and
# sockets, listeners/clients with HMAC challenge authentication, and wait().
#
# Wire format: every message is a 4-byte big-endian signed length followed
# by the payload; payloads of 2 GiB or more use the length -1 followed by
# an 8-byte unsigned length.  Objects are sent pickled (ForkingPickler);
# only connect to peers that know the authkey, unpickling runs code.
#

__all__ = ['Client', 'Listener', 'Pipe', 'wait']

import errno
import io
import itertools
import os
import sys
import socket
import struct
import time

from . import util
from . import AuthenticationError, BufferTooShort
from .context import reduction

_ForkingPickler = reduction.ForkingPickler

BUFSIZE = 8192
# seconds a Client keeps retrying while the listener is not ready yet
CONNECTION_TIMEOUT = 20.

_mmap_counter = itertools.count()

default_family = 'AF_INET'
families = ['AF_INET']

if hasattr(socket, 'AF_UNIX'):
    default_family = 'AF_UNIX'
    families += ['AF_UNIX']

if sys.platform == 'win32':
    default_family = 'AF_PIPE'
    families += ['AF_PIPE']


def _init_timeout(timeout=CONNECTION_TIMEOUT):
    return time.monotonic() + timeout


def _check_timeout(t):
    return time.monotonic() > t


def arbitrary_address(family):
    """Return an unused address of the given family."""
    if family == 'AF_INET':
        return ('localhost', 0)
    elif family == 'AF_UNIX':
        import tempfile
        return tempfile.mktemp(prefix='listener-', dir=util.get_temp_dir())
    elif family == 'AF_PIPE':
        import tempfile
        return tempfile.mktemp(prefix=r'\\.\pipe\pyc-%d-%d-' %
                               (os.getpid(), next(_mmap_counter)), dir="")
    else:
        raise ValueError('unrecognized family')


def _check_family(family):
    if family != 'AF_PIPE' and not hasattr(socket, family):
        raise ValueError('Family %s is not recognized.' % family)
    if family == 'AF_PIPE' and sys.platform != 'win32':
        raise ValueError('Family %s is not recognized.' % family)


def address_type(address):
    """Guess the family of an address."""
    if type(address) == tuple:
        return 'AF_INET'
    elif type(address) is str and address.startswith('\\\\'):
        return 'AF_PIPE'
    elif type(address) is str or util.is_abstract_socket_namespace(address):
        return 'AF_UNIX'
    else:
        raise ValueError('address type of %r unrecognized' % address)


# ---------------------------------------------------------------- connections

class _ConnectionBase:
    _handle = None

    def __init__(self, handle, readable=True, writable=True):
        handle = handle.__index__()
        if handle < 0:
            raise ValueError("invalid handle")
        if not readable and not writable:
            raise ValueError(
                "at least one of `readable` and `writable` must be True")
        self._handle = handle
        self._readable = readable
        self._writable = writable

    def __del__(self):
        if self._handle is not None:
            self._close()

    def _check_closed(self):
        if self._handle is None:
            raise OSError("handle is closed")

    def _check_readable(self):
        if not self._readable:
            raise OSError("connection is write-only")

    def _check_writable(self):
        if not self._writable:
            raise OSError("connection is read-only")

    def _bad_message_length(self):
        if self._writable:
            self._readable = False
        else:
            self.close()
        raise OSError("bad message length")

    @property
    def closed(self):
        """True if the connection is closed."""
        return self._handle is None

    @property
    def readable(self):
        """True if the connection can receive."""
        return self._readable

    @property
    def writable(self):
        """True if the connection can send."""
        return self._writable

    def fileno(self):
        """The file descriptor of the connection."""
        self._check_closed()
        return self._handle

    def close(self):
        """Close the connection."""
        if self._handle is not None:
            try:
                self._close()
            finally:
                self._handle = None

    def _detach(self):
        self._handle = None

    def send_bytes(self, buf, offset=0, size=None):
        """Send the bytes of a bytes-like object (or a slice of it)."""
        self._check_closed()
        self._check_writable()
        m = memoryview(buf)
        try:
            if m.itemsize > 1:
                m = m.cast('B')
            n = m.nbytes
            if offset < 0:
                raise ValueError("offset is negative")
            if n < offset:
                raise ValueError("buffer length < offset")
            if size is None:
                size = n - offset
            elif size < 0:
                raise ValueError("size is negative")
            elif offset + size > n:
                raise ValueError("buffer length < offset + size")
            self._send_bytes(m[offset:offset + size])
        finally:
            m.release()

    def send(self, obj):
        """Send a picklable object."""
        self._check_closed()
        self._check_writable()
        self._send_bytes(_ForkingPickler.dumps(obj))

    def recv_bytes(self, maxlength=None):
        """Receive one message as bytes."""
        self._check_closed()
        self._check_readable()
        if maxlength is not None and maxlength < 0:
            raise ValueError("negative maxlength")
        buf = self._recv_bytes(maxlength)
        if buf is None:
            self._bad_message_length()
        return buf.getvalue()

    def recv_bytes_into(self, buf, offset=0):
        """Receive one message into the writable buffer *buf* (starting at
        byte *offset*); returns the message size."""
        self._check_closed()
        self._check_readable()
        with memoryview(buf) as m:
            itemsize = m.itemsize
            bytesize = itemsize * len(m)
            if offset < 0:
                raise ValueError("negative offset")
            elif offset > bytesize:
                raise ValueError("offset too large")
            result = self._recv_bytes()
            size = result.tell()
            if bytesize < offset + size:
                raise BufferTooShort(result.getvalue())
            result.seek(0)
            with m.cast('B') as flat:
                result.readinto(flat[offset:offset + size])
            return size

    def recv(self):
        """Receive a pickled object."""
        self._check_closed()
        self._check_readable()
        buf = self._recv_bytes()
        return _ForkingPickler.loads(buf.getbuffer())

    def poll(self, timeout=0.0):
        """True if there is data to read (waits up to *timeout* seconds;
        forever if None)."""
        self._check_closed()
        self._check_readable()
        return self._poll(timeout)

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_value, exc_tb):
        self.close()


class Connection(_ConnectionBase):
    """A connection over a file descriptor (pipe end or socket)."""

    _write = os.write
    _read = os.read

    def _close(self, _close=os.close):
        _close(self._handle)

    def _send(self, buf, write=_write):
        remaining = len(buf)
        while True:
            n = write(self._handle, buf)
            remaining -= n
            if remaining == 0:
                break
            buf = buf[n:]

    def _recv(self, size, read=_read):
        buf = io.BytesIO()
        handle = self._handle
        remaining = size
        while remaining > 0:
            chunk = read(handle, remaining)
            n = len(chunk)
            if n == 0:
                if remaining == size:
                    raise EOFError
                else:
                    raise OSError("got end of file during message")
            buf.write(chunk)
            remaining -= n
        return buf

    def _send_bytes(self, buf):
        n = len(buf)
        if n > 0x7fffffff:
            self._send(struct.pack("!i", -1))
            self._send(struct.pack("!Q", n))
            self._send(buf)
        else:
            header = struct.pack("!i", n)
            if n > 16384:
                # avoid copying a big payload just to prepend the header
                self._send(header)
                self._send(buf)
            else:
                self._send(header + bytes(buf))

    def _recv_bytes(self, maxsize=None):
        buf = self._recv(4)
        size, = struct.unpack("!i", buf.getvalue())
        if size == -1:
            buf = self._recv(8)
            size, = struct.unpack("!Q", buf.getvalue())
        if maxsize is not None and size > maxsize:
            return None
        return self._recv(size)

    def _poll(self, timeout):
        return bool(wait([self], timeout))


# ---------------------------------------------------------------- listeners

class Listener(object):
    """A bound socket accepting authenticated connections."""

    def __init__(self, address=None, family=None, backlog=1, authkey=None):
        family = family or (address and address_type(address)) \
                 or default_family
        address = address or arbitrary_address(family)

        _check_family(family)
        if family == 'AF_PIPE':
            raise ValueError('Family AF_PIPE is not supported')
        self._listener = SocketListener(address, family, backlog)

        if authkey is not None and not isinstance(authkey, bytes):
            raise TypeError('authkey should be a byte string')

        self._authkey = authkey

    def accept(self):
        """Accept a connection (authenticating it if an authkey is set)."""
        if self._listener is None:
            raise OSError('listener is closed')

        c = self._listener.accept()
        if self._authkey is not None:
            deliver_challenge(c, self._authkey)
            answer_challenge(c, self._authkey)
        return c

    def close(self):
        """Close the listening socket."""
        listener = self._listener
        if listener is not None:
            self._listener = None
            listener.close()

    @property
    def address(self):
        return self._listener._address

    @property
    def last_accepted(self):
        return self._listener._last_accepted

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_value, exc_tb):
        self.close()


def Client(address, family=None, authkey=None):
    """Connect to a Listener at *address*."""
    family = family or address_type(address)
    _check_family(family)
    c = SocketClient(address)

    if authkey is not None and not isinstance(authkey, bytes):
        raise TypeError('authkey should be a byte string')

    if authkey is not None:
        answer_challenge(c, authkey)
        deliver_challenge(c, authkey)

    return c


def Pipe(duplex=True):
    """Return a pair of connected Connection objects."""
    if duplex:
        s1, s2 = socket.socketpair()
        s1.setblocking(True)
        s2.setblocking(True)
        c1 = Connection(s1.detach())
        c2 = Connection(s2.detach())
    else:
        fd1, fd2 = os.pipe()
        c1 = Connection(fd1, writable=False)
        c2 = Connection(fd2, readable=False)
    return c1, c2


class SocketListener(object):
    """Listening socket wrapper used by Listener."""

    def __init__(self, address, family, backlog=1):
        self._socket = socket.socket(getattr(socket, family))
        try:
            if os.name == 'posix':
                self._socket.setsockopt(socket.SOL_SOCKET,
                                        socket.SO_REUSEADDR, 1)
            self._socket.setblocking(True)
            self._socket.bind(address)
            self._socket.listen(backlog)
            self._address = self._socket.getsockname()
        except OSError:
            self._socket.close()
            raise
        self._family = family
        self._last_accepted = None

        if family == 'AF_UNIX' and not util.is_abstract_socket_namespace(address):
            # remove the socket file when the listener goes away
            self._unlink = util.Finalize(
                self, os.unlink, args=(address,), exitpriority=0
            )
        else:
            self._unlink = None

    def accept(self):
        s, self._last_accepted = self._socket.accept()
        s.setblocking(True)
        return Connection(s.detach())

    def close(self):
        try:
            self._socket.close()
        finally:
            unlink = self._unlink
            if unlink is not None:
                self._unlink = None
                unlink()


def SocketClient(address):
    """Connect a socket to *address* and wrap it in a Connection."""
    family = address_type(address)
    with socket.socket(getattr(socket, family)) as s:
        s.setblocking(True)
        s.connect(address)
        return Connection(s.detach())


# ---------------------------------------------------------------- authentication
#
# Both sides prove knowledge of the shared authkey: one sends a random
# challenge "{digest}<random bytes>", the other answers with
# "{digest}" + HMAC(authkey, challenge).  Peers from before digest
# negotiation send a bare 20-byte challenge and expect a bare MD5 HMAC.

MESSAGE_LENGTH = 40  # random bytes per challenge
_MD5ONLY_MESSAGE_LENGTH = 20
_MD5_DIGEST_LEN = 16
_LEGACY_LENGTHS = (_MD5ONLY_MESSAGE_LENGTH, _MD5_DIGEST_LEN)

_CHALLENGE = b'#CHALLENGE#'
_WELCOME = b'#WELCOME#'
_FAILURE = b'#FAILURE#'

_ALLOWED_DIGESTS = frozenset(
        {b'md5', b'sha256', b'sha384', b'sha3_256', b'sha3_384'})
_MAX_DIGEST_LEN = max(len(_) for _ in _ALLOWED_DIGESTS)

# upper bound for any message of the handshake
_MAX_HANDSHAKE_LEN = 256


def _get_digest_name_and_payload(message):
    """Split "{digest}payload"; bare legacy messages imply md5."""
    if len(message) in _LEGACY_LENGTHS:
        return 'md5', message
    if message.startswith(b'{'):
        end = message.find(b'}', 1, _MAX_DIGEST_LEN + 2)
        if end > 0:
            digest = message[1:end]
            if digest in _ALLOWED_DIGESTS:
                return digest.decode('ascii'), message[end + 1:]
    raise AuthenticationError(
            f'unsupported message length={len(message)}')


def _create_response(authkey, message):
    """The answer to the challenge *message*."""
    import hmac
    digest_name = _get_digest_name_and_payload(message)[0]
    mac = hmac.new(authkey, message, digest_name).digest()
    if len(message) in _LEGACY_LENGTHS:
        # a legacy peer expects the bare digest
        return mac
    return b'{' + digest_name.encode('ascii') + b'}' + mac


def _verify_challenge(authkey, message, response):
    """Check that *response* answers the challenge *message*."""
    import hmac
    digest_name, response_mac = _get_digest_name_and_payload(response)
    expected = hmac.new(authkey, message, digest_name).digest()
    if len(expected) != len(response_mac):
        raise AuthenticationError(
                f'expected {digest_name!r} of length {len(expected)} '
                f'got {len(response_mac)}')
    if not hmac.compare_digest(expected, response_mac):
        raise AuthenticationError('digest received was wrong')


def deliver_challenge(connection, authkey, digest_name='sha256'):
    """Challenge the peer and check its answer."""
    if not isinstance(authkey, bytes):
        raise ValueError(
            "Authkey must be bytes, not {0!s}".format(type(authkey)))
    message = (b'{' + digest_name.encode('ascii') + b'}' +
               os.urandom(MESSAGE_LENGTH))
    connection.send_bytes(_CHALLENGE + message)
    response = connection.recv_bytes(_MAX_HANDSHAKE_LEN)
    try:
        _verify_challenge(authkey, message, response)
    except AuthenticationError:
        connection.send_bytes(_FAILURE)
        raise
    else:
        connection.send_bytes(_WELCOME)


def answer_challenge(connection, authkey):
    """Answer the peer's challenge."""
    if not isinstance(authkey, bytes):
        raise ValueError(
            "Authkey must be bytes, not {0!s}".format(type(authkey)))
    message = connection.recv_bytes(_MAX_HANDSHAKE_LEN)
    if not message.startswith(_CHALLENGE):
        raise AuthenticationError(
                f'Protocol error, expected challenge: {message=}')
    message = message[len(_CHALLENGE):]
    if len(message) < _MD5ONLY_MESSAGE_LENGTH:
        raise AuthenticationError(f'challenge too short: {len(message)} bytes')
    connection.send_bytes(_create_response(authkey, message))
    response = connection.recv_bytes(_MAX_HANDSHAKE_LEN)
    if response != _WELCOME:
        raise AuthenticationError('digest sent was rejected')


# ---------------------------------------------------------------- xmlrpc flavour

class ConnectionWrapper(object):
    """Adapts a connection to send/receive objects with custom dumps/loads."""

    def __init__(self, conn, dumps, loads):
        self._conn = conn
        self._dumps = dumps
        self._loads = loads
        for attr in ('fileno', 'close', 'poll', 'recv_bytes', 'send_bytes'):
            obj = getattr(conn, attr)
            setattr(self, attr, obj)

    def send(self, obj):
        self._conn.send_bytes(self._dumps(obj))

    def recv(self):
        return self._loads(self._conn.recv_bytes())


def _xml_dumps(obj):
    import xmlrpc.client
    return xmlrpc.client.dumps((obj,), None, None, None, 1).encode('utf-8')


def _xml_loads(s):
    import xmlrpc.client
    (obj,), method = xmlrpc.client.loads(s.decode('utf-8'))
    return obj


class XmlListener(Listener):
    def accept(self):
        obj = Listener.accept(self)
        return ConnectionWrapper(obj, _xml_dumps, _xml_loads)


def XmlClient(*args, **kwds):
    return ConnectionWrapper(Client(*args, **kwds), _xml_dumps, _xml_loads)


# ---------------------------------------------------------------- wait()

import selectors

if hasattr(selectors, 'PollSelector'):
    _WaitSelector = selectors.PollSelector
else:
    _WaitSelector = selectors.SelectSelector


def wait(object_list, timeout=None):
    """Wait until an object of *object_list* is ready.

    Objects are file descriptors or have a fileno() method (connections,
    sockets, process sentinels).  Returns the list of ready objects (empty
    on timeout)."""
    with _WaitSelector() as selector:
        for obj in object_list:
            selector.register(obj, selectors.EVENT_READ)

        if timeout is not None:
            deadline = time.monotonic() + timeout

        while True:
            ready = selector.select(timeout)
            if ready:
                return [key.fileobj for (key, events) in ready]
            if timeout is not None:
                timeout = deadline - time.monotonic()
                if timeout < 0:
                    return ready


# ---------------------------------------------------------------- pickling

def reduce_connection(conn):
    df = reduction.DupFd(conn.fileno())
    return rebuild_connection, (df, conn.readable, conn.writable)


def rebuild_connection(df, readable, writable):
    fd = df.detach()
    return Connection(fd, readable, writable)


reduction.register(Connection, reduce_connection)
