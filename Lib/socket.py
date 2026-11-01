"""Object-oriented socket interface built on the _socket extension.

socket.socket extends _socket.socket with context management, makefile(),
sendfile(), dup()/accept() returning socket.socket objects and enum-typed
family/type attributes.  Module helpers: create_connection(),
create_server(), socketpair(), fromfd(), send_fds(), recv_fds(), getfqdn()
and getaddrinfo() returning enum members.
"""

import _socket
from _socket import *

import io
import os
import sys
from enum import IntEnum, IntFlag

try:
    import errno
except ImportError:
    errno = None
EBADF = getattr(errno, 'EBADF', 9)
EAGAIN = getattr(errno, 'EAGAIN', 11)
EWOULDBLOCK = getattr(errno, 'EWOULDBLOCK', 11)

__all__ = ["fromfd", "getfqdn", "create_connection", "create_server",
           "has_dualstack_ipv6", "AddressFamily", "SocketKind"]
__all__ += [_n for _n in dir(_socket) if not _n.startswith('_')]


def _enum_from_prefix(base, name, prefix):
    base._convert_(name, __name__,
                   lambda const: const.isupper() and const.startswith(prefix))


_enum_from_prefix(IntEnum, 'AddressFamily', 'AF_')
_enum_from_prefix(IntEnum, 'SocketKind', 'SOCK_')
_enum_from_prefix(IntFlag, 'MsgFlag', 'MSG_')
_enum_from_prefix(IntFlag, 'AddressInfo', 'AI_')
del _enum_from_prefix

_LOCALHOST = '127.0.0.1'
_LOCALHOST_V6 = '::1'


def _intenum_converter(value, enum_klass):
    """enum_klass(value) if it is a known member, else value unchanged."""
    try:
        return enum_klass(value)
    except ValueError:
        return value


class _GiveupOnSendfile(Exception):
    """os.sendfile() cannot be used for this file: fall back to send()."""


_WOULD_BLOCK = frozenset((EAGAIN, EWOULDBLOCK))


class socket(_socket.socket):
    """socket(family=AF_INET, type=SOCK_STREAM, proto=0, fileno=None)"""

    __slots__ = ["__weakref__", "_io_refs", "_closed"]

    def __init__(self, family=-1, type=-1, proto=-1, fileno=None):
        if fileno is None:
            # explicit defaults; with a fileno, -1 means "ask the kernel"
            family = AF_INET if family == -1 else family
            type = SOCK_STREAM if type == -1 else type
            proto = 0 if proto == -1 else proto
        _socket.socket.__init__(self, family, type, proto, fileno)
        self._io_refs = 0
        self._closed = False

    def __enter__(self):
        return self

    def __exit__(self, *exc_info):
        if not self._closed:
            self.close()

    def __repr__(self):
        closed = getattr(self, '_closed', False)
        text = (f"<{type(self).__module__}.{type(self).__qualname__}"
                f"{' [closed]' if closed else ''} fd={self.fileno()}, "
                f"family={self.family}, type={self.type}, proto={self.proto}")
        if not closed:
            for label, getter in (('laddr', 'getsockname'),
                                  ('raddr', 'getpeername')):
                try:
                    addr = getattr(self, getter)()
                except (error, AttributeError):
                    continue
                if addr:
                    text += f", {label}={addr}"
        return text + '>'

    def __getstate__(self):
        raise TypeError(f"cannot pickle {self.__class__.__name__!r} object")

    def dup(self):
        """Duplicate the socket (the new descriptor is non-inheritable)."""
        twin = self.__class__(self.family, self.type, self.proto,
                              fileno=dup(self.fileno()))
        twin.settimeout(self.gettimeout())
        return twin

    def accept(self):
        """Accept a connection; return (connected socket, peer address)."""
        fd, address = self._accept()
        conn = socket(self.family, self.type, self.proto, fileno=fd)
        if getdefaulttimeout() is None and self.gettimeout():
            # Some platforms let the accepted socket inherit O_NONBLOCK from
            # a listening socket with a timeout; without a default timeout
            # the new socket must be blocking.
            conn.setblocking(True)
        return conn, address

    def makefile(self, mode="r", buffering=None, *,
                 encoding=None, errors=None, newline=None):
        """Return a file object for the socket ('r', 'w' and/or 'b' modes)."""
        if not set(mode) <= {"r", "w", "b"}:
            raise ValueError("invalid mode %r (only r, w, b allowed)" % (mode,))
        writing = "w" in mode
        reading = "r" in mode or not writing
        binary = "b" in mode
        raw = SocketIO(self, ("r" if reading else "") + ("w" if writing else ""))
        self._io_refs += 1
        if buffering is None or buffering < 0:
            buffering = io.DEFAULT_BUFFER_SIZE
        if buffering == 0:
            if not binary:
                raise ValueError("unbuffered streams must be binary")
            return raw
        if reading and writing:
            stream = io.BufferedRWPair(raw, raw, buffering)
        elif reading:
            stream = io.BufferedReader(raw, buffering)
        else:
            stream = io.BufferedWriter(raw, buffering)
        if binary:
            return stream
        text = io.TextIOWrapper(stream, io.text_encoding(encoding),
                                errors, newline)
        text.mode = mode
        return text

    # -- sendfile

    def _check_sendfile_params(self, file, offset, count):
        if 'b' not in getattr(file, 'mode', 'b'):
            raise ValueError("file should be opened in binary mode")
        if not self.type & SOCK_STREAM:
            raise ValueError("only SOCK_STREAM type sockets are supported")
        if count is None:
            return
        if not isinstance(count, int):
            raise TypeError(f"count must be a positive integer (got {count!r})")
        if count <= 0:
            raise ValueError(f"count must be a positive integer (got {count!r})")

    def _sendfile_use_sendfile(self, file, offset=0, count=None):
        if not hasattr(os, 'sendfile'):
            raise _GiveupOnSendfile("os.sendfile() not available on this platform")
        import selectors

        self._check_sendfile_params(file, offset, count)
        try:
            in_fd = file.fileno()
        except (AttributeError, io.UnsupportedOperation) as exc:
            raise _GiveupOnSendfile(exc)
        try:
            size = os.fstat(in_fd).st_size
        except OSError as exc:
            raise _GiveupOnSendfile(exc)
        if size == 0:
            return 0
        timeout = self.gettimeout()
        if timeout == 0:
            raise ValueError("non-blocking sockets are not supported")
        # one sendfile() call moves at most 1 GiB (avoids size overflows)
        chunk = min(count or size, 1 << 30)
        out_fd = self.fileno()
        waiter = (selectors.PollSelector() if hasattr(selectors, 'PollSelector')
                  else selectors.SelectSelector())
        waiter.register(out_fd, selectors.EVENT_WRITE)
        sent_total = 0
        try:
            while count is None or sent_total < count:
                if timeout and not waiter.select(timeout):
                    raise TimeoutError('timed out')
                if count is not None:
                    chunk = min(chunk, count - sent_total)
                try:
                    n = os.sendfile(out_fd, in_fd, offset, chunk)
                except BlockingIOError:
                    if not timeout:
                        waiter.select()
                    continue
                except OSError as exc:
                    if sent_total:
                        raise exc from None
                    # nothing sent yet: let sendfile() retry with send()
                    raise _GiveupOnSendfile(exc)
                if n == 0:
                    break
                offset += n
                sent_total += n
            return sent_total
        finally:
            waiter.close()
            if sent_total and hasattr(file, 'seek'):
                file.seek(offset)

    def _sendfile_use_send(self, file, offset=0, count=None):
        self._check_sendfile_params(file, offset, count)
        if self.gettimeout() == 0:
            raise ValueError("non-blocking sockets are not supported")
        if offset:
            file.seek(offset)
        piece = min(count, 8192) if count else 8192
        sent_total = 0
        try:
            while count is None or sent_total < count:
                if count is not None:
                    piece = min(piece, count - sent_total)
                view = memoryview(file.read(piece))
                if not view:
                    break
                while view:
                    try:
                        n = self.send(view)
                    except BlockingIOError:
                        continue
                    sent_total += n
                    view = view[n:]
            return sent_total
        finally:
            if sent_total and hasattr(file, 'seek'):
                file.seek(offset + sent_total)

    def sendfile(self, file, offset=0, count=None):
        """Send the contents of a binary file; return the byte count.

        Uses os.sendfile() for regular files and send() otherwise.  The
        file position ends after the last byte sent.
        """
        try:
            return self._sendfile_use_sendfile(file, offset, count)
        except _GiveupOnSendfile:
            return self._sendfile_use_send(file, offset, count)

    # -- closing

    def _decref_socketios(self):
        if self._io_refs > 0:
            self._io_refs -= 1
        if self._closed:
            self.close()

    def _real_close(self, _close=_socket.socket.close):
        # bound default: works even when module globals are torn down
        _close(self)

    def close(self):
        """Close the socket once no makefile() stream uses it any more."""
        self._closed = True
        if self._io_refs <= 0:
            self._real_close()

    def detach(self):
        """Give up the descriptor without closing it and return it."""
        self._closed = True
        return super().detach()

    @property
    def family(self):
        """The address family (an AddressFamily member when known)."""
        return _intenum_converter(super().family, AddressFamily)

    @property
    def type(self):
        """The socket type (a SocketKind member when known)."""
        return _intenum_converter(super().type, SocketKind)

    def get_inheritable(self):
        """Get the inheritable flag of the socket"""
        return os.get_inheritable(self.fileno())

    def set_inheritable(self, inheritable):
        """Set the inheritable flag of the socket"""
        os.set_inheritable(self.fileno(), inheritable)


def fromfd(fd, family, type, proto=0):
    """Wrap a duplicate of descriptor *fd* in a new socket object."""
    return socket(family, type, proto, dup(fd))


def send_fds(sock, buffers, fds, flags=0, address=None):
    """Send *buffers* plus the descriptors *fds* over an AF_UNIX socket."""
    import array
    rights = array.array("i", fds)
    return sock.sendmsg(buffers, [(SOL_SOCKET, SCM_RIGHTS, rights)])


def recv_fds(sock, bufsize, maxfds, flags=0):
    """Receive up to *maxfds* descriptors; return
    (data, fds, msg_flags, address)."""
    import array
    received = array.array("i")
    size = received.itemsize
    data, ancillary, msg_flags, address = sock.recvmsg(
        bufsize, CMSG_LEN(maxfds * size))
    for level, kind, payload in ancillary:
        if (level, kind) == (SOL_SOCKET, SCM_RIGHTS):
            usable = len(payload) - len(payload) % size
            received.frombytes(payload[:usable])
    return data, list(received), msg_flags, address


__all__ += ["send_fds", "recv_fds"]


def _fallback_socketpair(family=AF_INET, type=SOCK_STREAM, proto=0):
    """socketpair() built from a loopback TCP connection."""
    hosts = {AF_INET: _LOCALHOST}
    if 'AF_INET6' in globals():
        hosts[AF_INET6] = _LOCALHOST_V6
    if family not in hosts:
        raise ValueError("Only AF_INET and AF_INET6 socket address families "
                         "are supported")
    if type != SOCK_STREAM:
        raise ValueError("Only SOCK_STREAM socket type is supported")
    if proto != 0:
        raise ValueError("Only protocol zero is supported")

    with socket(family, type, proto) as listener:
        listener.bind((hosts[family], 0))
        listener.listen()
        target = listener.getsockname()[:2]
        client = socket(family, type, proto)
        try:
            # connect without blocking, then accept the other end
            client.setblocking(False)
            try:
                client.connect(target)
            except (BlockingIOError, InterruptedError):
                pass
            client.setblocking(True)
            server, _ = listener.accept()
        except:
            client.close()
            raise
    try:
        # someone else might have connected first: check it is our client
        if (server.getsockname() != client.getpeername()
                or client.getsockname() != server.getpeername()):
            raise ConnectionError("Unexpected peer connection")
    except:
        server.close()
        client.close()
        raise
    return server, client


def socketpair(family=None, type=SOCK_STREAM, proto=0):
    """socketpair([family[, type[, proto]]]) -> (socket object, socket object)

    Return two connected sockets (AF_UNIX by default).
    """
    if family is None:
        family = globals().get('AF_UNIX', AF_INET)
    if not hasattr(_socket, 'socketpair'):
        return _fallback_socketpair(family, type, proto)
    pair = _socket.socketpair(family, type, proto)
    return tuple(socket(family, type, proto, s.detach()) for s in pair)


class SocketIO(io.RawIOBase):
    """Raw binary stream over a stream socket (what makefile() wraps)."""

    def __init__(self, sock, mode):
        if mode not in ("r", "w", "rw", "rb", "wb", "rwb"):
            raise ValueError("invalid mode: %r" % mode)
        io.RawIOBase.__init__(self)
        self._sock = sock
        self._mode = mode if "b" in mode else mode + "b"
        self._reading = "r" in mode
        self._writing = "w" in mode
        self._timeout_occurred = False

    def readinto(self, b):
        """Receive into *b*; None when a non-blocking socket has no data."""
        self._checkClosed()
        self._checkReadable()
        if self._timeout_occurred:
            raise OSError("cannot read from timed out object")
        try:
            return self._sock.recv_into(b)
        except timeout:
            # the stream position is unknown after a timeout
            self._timeout_occurred = True
            raise
        except error as exc:
            if exc.errno in _WOULD_BLOCK:
                return None
            raise

    def write(self, b):
        """Send *b*; return the byte count, or None if it would block."""
        self._checkClosed()
        self._checkWritable()
        try:
            return self._sock.send(b)
        except error as exc:
            if exc.errno in _WOULD_BLOCK:
                return None
            raise

    def _ensure_open(self):
        if self.closed:
            raise ValueError("I/O operation on closed socket.")

    def readable(self):
        self._ensure_open()
        return self._reading

    def writable(self):
        self._ensure_open()
        return self._writing

    def seekable(self):
        self._ensure_open()
        return super().seekable()

    def fileno(self):
        self._checkClosed()
        return self._sock.fileno()

    @property
    def name(self):
        return -1 if self.closed else self.fileno()

    @property
    def mode(self):
        return self._mode

    def close(self):
        """Close the stream (the socket closes when nothing uses it)."""
        if not self.closed:
            io.RawIOBase.close(self)
            self._sock._decref_socketios()
            self._sock = None


def getfqdn(name=''):
    """Best fully qualified name for *name* (default: the local host)."""
    name = name.strip()
    if name in ('', '0.0.0.0', '::'):
        name = gethostname()
    try:
        primary, aliases, _ = gethostbyaddr(name)
    except error:
        return name
    for candidate in [primary, *aliases]:
        if '.' in candidate:
            return candidate
    return primary


_GLOBAL_DEFAULT_TIMEOUT = object()


def create_connection(address, timeout=_GLOBAL_DEFAULT_TIMEOUT,
                      source_address=None, *, all_errors=False):
    """Open a TCP connection to (host, port) and return the socket.

    Every address getaddrinfo() returns is tried in order.  If all fail,
    the last error is raised -- or, with all_errors=True, an
    ExceptionGroup of all of them.
    """
    host, port = address
    failures = []
    for family, kind, proto, _, sockaddr in getaddrinfo(host, port, 0,
                                                          SOCK_STREAM):
        sock = None
        try:
            sock = socket(family, kind, proto)
            if timeout is not _GLOBAL_DEFAULT_TIMEOUT:
                sock.settimeout(timeout)
            if source_address:
                sock.bind(source_address)
            sock.connect(sockaddr)
        except error as exc:
            if sock is not None:
                sock.close()
            if all_errors:
                failures.append(exc)
            else:
                failures = [exc]
            continue
        failures = None
        return sock
    if not failures:
        raise error("getaddrinfo returns an empty list")
    try:
        if all_errors:
            raise ExceptionGroup("create_connection failed", failures)
        raise failures[0]
    finally:
        failures = None


def has_dualstack_ipv6():
    """True if one AF_INET6 socket can serve IPv4 and IPv6 clients."""
    if not (has_ipv6 and hasattr(_socket, 'IPPROTO_IPV6')
            and hasattr(_socket, 'IPV6_V6ONLY')):
        return False
    try:
        with socket(AF_INET6, SOCK_STREAM) as probe:
            probe.setsockopt(IPPROTO_IPV6, IPV6_V6ONLY, 0)
    except error:
        return False
    return True


def create_server(address, *, family=AF_INET, backlog=None, reuse_port=False,
                  dualstack_ipv6=False):
    """Return a TCP socket bound to *address* and listening."""
    if reuse_port and not hasattr(_socket, "SO_REUSEPORT"):
        raise ValueError("SO_REUSEPORT not supported on this platform")
    if dualstack_ipv6:
        if not has_dualstack_ipv6():
            raise ValueError("dualstack_ipv6 not supported on this platform")
        if family != AF_INET6:
            raise ValueError("dualstack_ipv6 requires AF_INET6 family")
    sock = socket(family, SOCK_STREAM)
    try:
        if os.name not in ('nt', 'cygwin') and hasattr(_socket, 'SO_REUSEADDR'):
            try:
                sock.setsockopt(SOL_SOCKET, SO_REUSEADDR, 1)
            except error:
                pass  # bind() will report real problems
        if reuse_port and family in (AF_INET, AF_INET6):
            sock.setsockopt(SOL_SOCKET, SO_REUSEPORT, 1)
        if has_ipv6 and family == AF_INET6 and hasattr(_socket, "IPV6_V6ONLY"):
            sock.setsockopt(IPPROTO_IPV6, IPV6_V6ONLY, 0 if dualstack_ipv6 else 1)
        try:
            sock.bind(address)
        except error as exc:
            raise error(exc.errno, f'{exc.strerror} (while attempting to bind '
                                   f'on address {address!r})') from None
        if backlog is None:
            sock.listen()
        else:
            sock.listen(backlog)
    except error:
        sock.close()
        raise
    return sock


def getaddrinfo(host, port, family=0, type=0, proto=0, flags=0):
    """Resolve (host, port) to a list of
    (family, type, proto, canonname, sockaddr) with enum-typed family/type.
    """
    return [(_intenum_converter(fam, AddressFamily),
             _intenum_converter(kind, SocketKind), pr, canon, sa)
            for fam, kind, pr, canon, sa in
            _socket.getaddrinfo(host, port, family, type, proto, flags)]
