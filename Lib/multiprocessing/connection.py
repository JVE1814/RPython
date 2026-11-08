#
# multiprocessing.connection -- message channels between processes.
#
# A Connection sends length-prefixed messages over a file descriptor (one
# end of a pipe or a connected socket).  Listener/Client build connections
# over sockets and can authenticate both ends with an HMAC handshake over a
# shared key.  wait() blocks until one of several objects is readable.
#

__all__ = ['Client', 'Listener', 'Pipe', 'wait']

import itertools
import math
import os
import select
import socket
import struct
import time

from . import util
from . import AuthenticationError, BufferTooShort
from .context import reduction

_ForkingPickler = reduction.ForkingPickler

BUFSIZE = 8192

# seconds a Client keeps retrying a refused connection
CONNECTION_TIMEOUT = 20.

_mmap_counter = itertools.count()

families = ['AF_INET']
default_family = 'AF_INET'
if hasattr(socket, 'AF_UNIX'):
    families.append('AF_UNIX')
    default_family = 'AF_UNIX'


#
# Addresses
#

def arbitrary_address(family):
    """An address of the given family that nothing is listening on yet."""
    if family == 'AF_INET':
        return ('localhost', 0)
    if family == 'AF_UNIX':
        import tempfile
        return tempfile.mktemp(prefix='listener-', dir=util.get_temp_dir())
    if family == 'AF_PIPE':
        import tempfile
        prefix = r'\\.\pipe\pyc-%d-%d-' % (os.getpid(), next(_mmap_counter))
        return tempfile.mktemp(prefix=prefix, dir='')
    raise ValueError('unrecognized family')


def _check_family(family):
    """Reject families this platform cannot provide."""
    usable = (family != 'AF_PIPE' and hasattr(socket, family))
    if not usable:
        raise ValueError('Family %s is not recognized.' % family)


def address_type(address):
    """The family an address belongs to."""
    if type(address) == tuple:
        return 'AF_INET'
    if type(address) is str and address.startswith('\\\\'):
        return 'AF_PIPE'
    if type(address) is str or util.is_abstract_socket_namespace(address):
        return 'AF_UNIX'
    raise ValueError('address type of %r unrecognized' % address)


#
# Connection
#

# message header: a signed 32-bit big-endian length; -1 announces that an
# unsigned 64-bit length follows (messages of 2 GiB or more)
_SHORT_HEADER = struct.Struct('!i')
_LONG_HEADER = struct.Struct('!Q')
_LONG_MARK = -1
_SHORT_LIMIT = 0x7fffffff
# messages up to this size are written in one call together with the header
_COALESCE_LIMIT = 16384


def _as_bytes_view(buf):
    """A flat memoryview of buf's bytes."""
    view = memoryview(buf)
    if view.ndim != 1 or view.itemsize != 1:
        view = view.cast('B')
    return view


class Connection:
    """One end of a channel built on a file descriptor."""

    _handle = None

    def __init__(self, handle, readable=True, writable=True):
        handle = handle.__index__()
        if handle < 0:
            raise ValueError("invalid handle")
        if not (readable or writable):
            raise ValueError(
                "at least one of `readable` and `writable` must be True")
        self._handle = handle
        self._readable = readable
        self._writable = writable

    def __del__(self):
        if self._handle is not None:
            self._close()

    def __repr__(self):
        state = 'closed' if self._handle is None else 'fd=%d' % self._handle
        return '<%s %s>' % (type(self).__qualname__, state)

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_value, exc_tb):
        self.close()

    # -- state

    @property
    def closed(self):
        """True once close() was called."""
        return self._handle is None

    @property
    def readable(self):
        """True if this end can receive."""
        return self._readable

    @property
    def writable(self):
        """True if this end can send."""
        return self._writable

    def _require(self, direction=None):
        if self._handle is None:
            raise OSError("handle is closed")
        if direction == 'r' and not self._readable:
            raise OSError("connection is write-only")
        if direction == 'w' and not self._writable:
            raise OSError("connection is read-only")

    def fileno(self):
        """The underlying file descriptor."""
        self._require()
        return self._handle

    def close(self):
        """Close the connection (a no-op when already closed)."""
        if self._handle is not None:
            try:
                self._close()
            finally:
                self._handle = None

    def _close(self, _close=os.close):
        _close(self._handle)

    def _detach(self):
        """Forget the descriptor without closing it."""
        self._handle = None

    # -- low-level I/O

    def _write_all(self, data, write=os.write):
        view = memoryview(data)
        while view:
            done = write(self._handle, view)
            view = view[done:]

    def _read_exact(self, target, at_start=False, readinto=os.readinto):
        """Fill the writable buffer target (a byte view) completely.  An end
        of file before the first byte is EOFError when at_start (nothing of a
        message was read yet), any other one OSError."""
        filled = 0
        wanted = len(target)
        while filled < wanted:
            n = readinto(self._handle, target[filled:])
            if n == 0:
                if filled == 0 and at_start:
                    raise EOFError
                raise OSError("got end of file during message")
            filled += n

    def _send_message(self, view):
        size = len(view)
        if size > _SHORT_LIMIT:
            self._write_all(_SHORT_HEADER.pack(_LONG_MARK) +
                            _LONG_HEADER.pack(size))
            self._write_all(view)
        elif size > _COALESCE_LIMIT:
            self._write_all(_SHORT_HEADER.pack(size))
            self._write_all(view)
        else:
            self._write_all(_SHORT_HEADER.pack(size) + view)

    def _read_size(self):
        head = bytearray(_SHORT_HEADER.size)
        self._read_exact(memoryview(head), at_start=True)
        size, = _SHORT_HEADER.unpack(head)
        if size == _LONG_MARK:
            head = bytearray(_LONG_HEADER.size)
            self._read_exact(memoryview(head))
            size, = _LONG_HEADER.unpack(head)
        return size

    def _read_body(self, size):
        body = bytearray(size)
        if size:
            self._read_exact(memoryview(body))
        return body

    def _message_too_long(self):
        # the rest of the message is still in the channel: this end can no
        # longer be read from (and is useless if it cannot send either)
        self._readable = False
        if not self._writable:
            self.close()
        raise OSError("bad message length")

    # -- public API

    def send_bytes(self, buf, offset=0, size=None):
        """Send the bytes of buf[offset:offset + size] as one message."""
        self._require('w')
        view = _as_bytes_view(buf)
        total = len(view)
        if offset < 0:
            raise ValueError("offset is negative")
        if total < offset:
            raise ValueError("buffer length < offset")
        if size is None:
            size = total - offset
        elif size < 0:
            raise ValueError("size is negative")
        elif offset + size > total:
            raise ValueError("buffer length < offset + size")
        self._send_message(view[offset:offset + size])

    def send(self, obj):
        """Send a picklable object."""
        self._require('w')
        self._send_message(memoryview(_ForkingPickler.dumps(obj)))

    def recv_bytes(self, maxlength=None):
        """Receive one message as bytes.  A message longer than maxlength
        closes the connection and raises OSError."""
        self._require('r')
        if maxlength is not None and maxlength < 0:
            raise ValueError("negative maxlength")
        size = self._read_size()
        if maxlength is not None and size > maxlength:
            self._message_too_long()
        return bytes(self._read_body(size))

    def recv_bytes_into(self, buf, offset=0):
        """Receive one message into the writable buffer buf, starting at byte
        offset; return its size.  If it does not fit, BufferTooShort carries
        the whole message."""
        self._require('r')
        with memoryview(buf) as outer:
            view = _as_bytes_view(outer)
            if offset < 0:
                raise ValueError("negative offset")
            if offset > len(view):
                raise ValueError("offset too large")
            size = self._read_size()
            if offset + size > len(view):
                raise BufferTooShort(bytes(self._read_body(size)))
            if size:
                self._read_exact(view[offset:offset + size])
            return size

    def recv(self):
        """Receive one pickled object."""
        self._require('r')
        return _ForkingPickler.loads(self._read_body(self._read_size()))

    def poll(self, timeout=0.0):
        """Whether data can be read, waiting up to timeout seconds (None:
        without limit)."""
        self._require('r')
        return self._poll(timeout)

    def _poll(self, timeout):
        return _fd_readable(self._handle, timeout)


#
# Pipe, Listener, Client
#

def Pipe(duplex=True):
    """A pair of connected Connections: (reader, writer) when duplex is
    false, else two ends that can both send and receive."""
    if duplex:
        left, right = socket.socketpair()
        # a default socket timeout would make the descriptors non-blocking
        left.setblocking(True)
        right.setblocking(True)
        return Connection(left.detach()), Connection(right.detach())
    rfd, wfd = os.pipe()
    return Connection(rfd, writable=False), Connection(wfd, readable=False)


class SocketListener(object):
    """A listening socket producing Connections."""

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
        self._unlink = None
        if family == 'AF_UNIX' and not util.is_abstract_socket_namespace(address):
            # remove the socket file at the latest when the process exits
            self._unlink = util.Finalize(self, os.unlink, args=(address,),
                                         exitpriority=0)

    def accept(self):
        sock, self._last_accepted = self._socket.accept()
        sock.setblocking(True)
        return Connection(sock.detach())

    def close(self):
        try:
            self._socket.close()
        finally:
            unlink, self._unlink = self._unlink, None
            if unlink is not None:
                unlink()


def SocketClient(address):
    """A Connection to a listening socket."""
    family = getattr(socket, address_type(address))
    with socket.socket(family) as sock:
        sock.setblocking(True)
        sock.connect(address)
        return Connection(sock.detach())


def _check_authkey(authkey):
    if authkey is not None and not isinstance(authkey, bytes):
        raise TypeError('authkey should be a byte string')


class Listener(object):
    """Accepts connections at an address; with an authkey, every accepted
    connection must pass the HMAC handshake (both directions)."""

    def __init__(self, address=None, family=None, backlog=1, authkey=None):
        if family is None:
            family = address_type(address) if address else default_family
        if address is None:
            address = arbitrary_address(family)
        _check_family(family)
        if family == 'AF_PIPE':
            raise ValueError('Family AF_PIPE is not supported')
        _check_authkey(authkey)
        self._listener = SocketListener(address, family, backlog)
        self._authkey = authkey

    def accept(self):
        """Wait for, authenticate and return the next connection."""
        if self._listener is None:
            raise OSError('listener is closed')
        conn = self._listener.accept()
        if self._authkey is not None:
            deliver_challenge(conn, self._authkey)
            answer_challenge(conn, self._authkey)
        return conn

    def close(self):
        """Stop listening."""
        listener, self._listener = self._listener, None
        if listener is not None:
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
    """Connect to a Listener at address (authenticating with authkey)."""
    if family is None:
        family = address_type(address)
    _check_family(family)
    _check_authkey(authkey)
    conn = SocketClient(address)
    if authkey is not None:
        answer_challenge(conn, authkey)
        deliver_challenge(conn, authkey)
    return conn


#
# Authentication
#
# The side that delivers a challenge sends
#     _CHALLENGE + b'{' + digest name + b'}' + random bytes
# and expects back  b'{' + digest name + b'}' + HMAC(key, challenge), where
# the challenge is everything after _CHALLENGE (so the digest name is
# authenticated too).  It answers _WELCOME or _FAILURE.  Peers from before
# digest names were introduced send 20 random bytes and answer with a bare
# 16-byte HMAC-MD5; both forms are understood.
#

MESSAGE_LENGTH = 40               # random bytes in a challenge
_MD5ONLY_MESSAGE_LENGTH = 20      # random bytes in a legacy challenge
_MD5_DIGEST_LEN = 16              # a legacy (bare HMAC-MD5) response
_LEGACY_LENGTHS = (_MD5ONLY_MESSAGE_LENGTH, _MD5_DIGEST_LEN)

_CHALLENGE = b'#CHALLENGE#'
_WELCOME = b'#WELCOME#'
_FAILURE = b'#FAILURE#'

_ALLOWED_DIGESTS = frozenset(
    {b'md5', b'sha256', b'sha384', b'sha3_256', b'sha3_384'})
_MAX_DIGEST_LEN = max(len(name) for name in _ALLOWED_DIGESTS)

# upper bound for every handshake message we accept
_MAX_HANDSHAKE_LEN = 256


def _get_digest_name_and_payload(message):
    """Split a challenge or response into (digest name, rest).  For the
    legacy untagged form the rest is the message object itself."""
    if message[:1] == b'{':
        close = message.find(b'}', 1, _MAX_DIGEST_LEN + 2)
        if close > 0:
            name = message[1:close]
            if name in _ALLOWED_DIGESTS:
                return name.decode('ascii'), message[close + 1:]
    if len(message) in _LEGACY_LENGTHS:
        return 'md5', message
    raise AuthenticationError(
        'unsupported message length or digest: %r' % message[:40])


def _mac(authkey, message, digest_name):
    import hmac
    return hmac.new(authkey, message, digest_name).digest()


def _create_response(authkey, message):
    """The answer to the challenge message."""
    digest_name, rest = _get_digest_name_and_payload(message)
    mac = _mac(authkey, message, digest_name)
    if rest is message:
        return mac                               # legacy peer: bare MAC
    return b'{%s}%s' % (digest_name.encode('ascii'), mac)


def _verify_challenge(authkey, message, response):
    """Raise AuthenticationError unless response answers message."""
    digest_name, mac = _get_digest_name_and_payload(response)
    import hmac
    expected = _mac(authkey, message, digest_name)
    if len(mac) != len(expected):
        raise AuthenticationError(
            f'expected {digest_name!r} of length {len(expected)} '
            f'got {len(mac)}')
    if not hmac.compare_digest(mac, expected):
        raise AuthenticationError('digest received was wrong')


def deliver_challenge(connection, authkey, digest_name='sha256'):
    """Challenge the peer to prove it knows authkey."""
    if not isinstance(authkey, bytes):
        raise ValueError(
            "Authkey must be bytes, not {0!s}".format(type(authkey)))
    if digest_name.encode('ascii') not in _ALLOWED_DIGESTS:
        raise ValueError(f'digest {digest_name!r} is not allowed')
    challenge = b'{%s}%s' % (digest_name.encode('ascii'),
                             os.urandom(MESSAGE_LENGTH))
    connection.send_bytes(_CHALLENGE + challenge)
    response = connection.recv_bytes(_MAX_HANDSHAKE_LEN)
    try:
        _verify_challenge(authkey, challenge, response)
    except AuthenticationError:
        connection.send_bytes(_FAILURE)
        raise
    connection.send_bytes(_WELCOME)


def answer_challenge(connection, authkey):
    """Prove to the peer that we know authkey."""
    if not isinstance(authkey, bytes):
        raise ValueError(
            "Authkey must be bytes, not {0!s}".format(type(authkey)))
    message = connection.recv_bytes(_MAX_HANDSHAKE_LEN)
    if not message.startswith(_CHALLENGE):
        raise AuthenticationError(
            f'Protocol error, expected challenge: {message=}')
    challenge = message[len(_CHALLENGE):]
    if len(challenge) < _MD5ONLY_MESSAGE_LENGTH:
        raise AuthenticationError(
            f'challenge too short: {len(challenge)} bytes')
    connection.send_bytes(_create_response(authkey, challenge))
    verdict = connection.recv_bytes(_MAX_HANDSHAKE_LEN)
    if verdict != _WELCOME:
        raise AuthenticationError('digest sent was rejected')


#
# Connections that exchange XML-RPC encoded values (managers' 'xmlrpclib'
# serializer)
#

class ConnectionWrapper(object):
    """A connection whose send()/recv() use other (de)serializers."""

    def __init__(self, conn, dumps, loads):
        self._conn = conn
        self._dumps = dumps
        self._loads = loads
        for name in ('fileno', 'close', 'poll', 'recv_bytes', 'send_bytes'):
            setattr(self, name, getattr(conn, name))

    def send(self, obj):
        self._conn.send_bytes(self._dumps(obj))

    def recv(self):
        return self._loads(self._conn.recv_bytes())


def _xml_dumps(obj):
    import xmlrpc.client
    return xmlrpc.client.dumps((obj,), None, None, None, 1).encode('utf-8')


def _xml_loads(s):
    import xmlrpc.client
    (obj,), _method = xmlrpc.client.loads(s.decode('utf-8'))
    return obj


class XmlListener(Listener):
    def accept(self):
        return ConnectionWrapper(Listener.accept(self), _xml_dumps, _xml_loads)


def XmlClient(*args, **kwds):
    return ConnectionWrapper(Client(*args, **kwds), _xml_dumps, _xml_loads)


#
# wait()
#

_READY_EVENTS = select.POLLIN | select.POLLHUP | select.POLLERR | select.POLLNVAL


def _poll_timeout(timeout):
    """Seconds -> poll()'s milliseconds (None: no limit), rounded up so that
    a short wait does not turn into a busy loop."""
    if timeout is None:
        return None
    if timeout <= 0:
        return 0
    return math.ceil(timeout * 1000)


def _fd_readable(fd, timeout):
    poller = select.poll()
    poller.register(fd, select.POLLIN)
    if timeout is not None:
        deadline = time.monotonic() + timeout
    while True:
        if poller.poll(_poll_timeout(timeout)):
            return True
        if timeout is None:
            continue
        timeout = deadline - time.monotonic()
        if timeout <= 0:
            return False


def wait(object_list, timeout=None):
    """Wait until at least one object of object_list (connections, sockets,
    file descriptors, process sentinels...) is readable or timeout seconds
    passed; return the readable ones (an empty list on timeout)."""
    by_fd = {}
    for obj in object_list:
        fd = obj if isinstance(obj, int) else obj.fileno()
        by_fd.setdefault(fd, []).append(obj)
    poller = select.poll()
    for fd in by_fd:
        poller.register(fd, select.POLLIN)
    if timeout is not None:
        deadline = time.monotonic() + timeout
    while True:
        events = poller.poll(_poll_timeout(timeout))
        ready = [fd for fd, ev in events if ev & _READY_EVENTS]
        if ready:
            return [obj for fd in ready for obj in by_fd[fd]]
        if timeout is not None:
            timeout = deadline - time.monotonic()
            if timeout <= 0:
                return []


#
# Pickling connections (they are passed by duplicating the descriptor)
#

def reduce_connection(conn):
    dup = reduction.DupFd(conn.fileno())
    return rebuild_connection, (dup, conn.readable, conn.writable)


def rebuild_connection(dup, readable, writable):
    return Connection(dup.detach(), readable, writable)


reduction.register(Connection, reduce_connection)
