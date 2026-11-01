"""Socket interface: a thin object-oriented layer over the _socket module.

The socket class adds makefile(), sendfile(), context-manager support and
IntEnum-typed family/type attributes to _socket.socket.  Helper functions:
create_connection(), create_server(), socketpair(), fromfd(), send_fds(),
recv_fds(), getfqdn() and an enum-aware getaddrinfo().
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
__all__.extend(getattr(_socket, "__all__", None) or
               [n for n in dir(_socket) if n[0] != "_"])

IntEnum._convert_('AddressFamily', __name__,
                  lambda C: C.isupper() and C.startswith('AF_'))
IntEnum._convert_('SocketKind', __name__,
                  lambda C: C.isupper() and C.startswith('SOCK_'))
IntFlag._convert_('MsgFlag', __name__,
                  lambda C: C.isupper() and C.startswith('MSG_'))
IntFlag._convert_('AddressInfo', __name__,
                  lambda C: C.isupper() and C.startswith('AI_'))

_LOCALHOST = '127.0.0.1'
_LOCALHOST_V6 = '::1'


def _intenum_converter(value, enum_klass):
    """Map *value* to a member of *enum_klass*; unknown values pass through."""
    try:
        return enum_klass(value)
    except ValueError:
        return value


class _GiveupOnSendfile(Exception):
    pass


class socket(_socket.socket):
    """socket(family=AF_INET, type=SOCK_STREAM, proto=0, fileno=None)"""

    __slots__ = ["__weakref__", "_io_refs", "_closed"]

    def __init__(self, family=-1, type=-1, proto=-1, fileno=None):
        if fileno is None:
            if family == -1:
                family = AF_INET
            if type == -1:
                type = SOCK_STREAM
            if proto == -1:
                proto = 0
        _socket.socket.__init__(self, family, type, proto, fileno)
        self._io_refs = 0
        self._closed = False

    def __enter__(self):
        return self

    def __exit__(self, *args):
        if not self._closed:
            self.close()

    def __repr__(self):
        closed = getattr(self, '_closed', False)
        parts = ["<%s.%s%s fd=%i, family=%s, type=%s, proto=%i"
                 % (self.__class__.__module__, self.__class__.__qualname__,
                    " [closed]" if closed else "", self.fileno(),
                    self.family, self.type, self.proto)]
        if not closed:
            try:
                laddr = self.getsockname()
                if laddr:
                    parts.append(", laddr=%s" % str(laddr))
            except (error, AttributeError):
                pass
            try:
                raddr = self.getpeername()
                if raddr:
                    parts.append(", raddr=%s" % str(raddr))
            except (error, AttributeError):
                pass
        parts.append('>')
        return ''.join(parts)

    def __getstate__(self):
        raise TypeError(f"cannot pickle {self.__class__.__name__!r} object")

    def dup(self):
        """Return a new (non-inheritable) socket object for the same socket."""
        fd = dup(self.fileno())
        sock = self.__class__(self.family, self.type, self.proto, fileno=fd)
        sock.settimeout(self.gettimeout())
        return sock

    def accept(self):
        """Wait for a connection; return (new socket, client address)."""
        fd, addr = self._accept()
        sock = socket(self.family, self.type, self.proto, fileno=fd)
        # Without a default timeout the accepted socket must be blocking,
        # whatever flags the platform copied from the listening socket.
        if getdefaulttimeout() is None and self.gettimeout():
            sock.setblocking(True)
        return sock, addr

    def makefile(self, mode="r", buffering=None, *,
                 encoding=None, errors=None, newline=None):
        """Return a file object reading from and/or writing to the socket.

        Only the mode characters 'r', 'w' and 'b' are supported.
        """
        if not set(mode) <= {"r", "w", "b"}:
            raise ValueError("invalid mode %r (only r, w, b allowed)" % (mode,))
        writing = "w" in mode
        reading = "r" in mode or not writing
        binary = "b" in mode
        rawmode = ("r" if reading else "") + ("w" if writing else "")
        raw = SocketIO(self, rawmode)
        self._io_refs += 1
        if buffering is None:
            buffering = -1
        if buffering < 0:
            buffering = io.DEFAULT_BUFFER_SIZE
        if buffering == 0:
            if not binary:
                raise ValueError("unbuffered streams must be binary")
            return raw
        if reading and writing:
            buffer = io.BufferedRWPair(raw, raw, buffering)
        elif reading:
            buffer = io.BufferedReader(raw, buffering)
        else:
            buffer = io.BufferedWriter(raw, buffering)
        if binary:
            return buffer
        encoding = io.text_encoding(encoding)
        text = io.TextIOWrapper(buffer, encoding, errors, newline)
        text.mode = mode
        return text

    if hasattr(os, 'sendfile'):

        def _sendfile_use_sendfile(self, file, offset=0, count=None):
            import selectors

            self._check_sendfile_params(file, offset, count)
            sockno = self.fileno()
            try:
                fileno = file.fileno()
            except (AttributeError, io.UnsupportedOperation) as err:
                raise _GiveupOnSendfile(err)
            try:
                fsize = os.fstat(fileno).st_size
            except OSError as err:
                raise _GiveupOnSendfile(err)
            if not fsize:
                return 0
            # os.sendfile() may overflow above 2 GiB: send at most 1 GiB per call
            blocksize = min(count or fsize, 2 ** 30)
            timeout = self.gettimeout()
            if timeout == 0:
                raise ValueError("non-blocking sockets are not supported")
            if hasattr(selectors, 'PollSelector'):
                selector = selectors.PollSelector()
            else:
                selector = selectors.SelectSelector()
            selector.register(sockno, selectors.EVENT_WRITE)

            total_sent = 0
            wait = selector.select
            sendfile = os.sendfile
            try:
                while True:
                    if timeout and not wait(timeout):
                        raise TimeoutError('timed out')
                    if count:
                        blocksize = min(count - total_sent, blocksize)
                        if blocksize <= 0:
                            break
                    try:
                        sent = sendfile(sockno, fileno, offset, blocksize)
                    except BlockingIOError:
                        if not timeout:
                            wait()
                        continue
                    except OSError as err:
                        if total_sent == 0:
                            # e.g. not a regular file: fall back to send()
                            raise _GiveupOnSendfile(err)
                        raise err from None
                    else:
                        if sent == 0:
                            break
                        offset += sent
                        total_sent += sent
                return total_sent
            finally:
                if total_sent > 0 and hasattr(file, 'seek'):
                    file.seek(offset)
    else:
        def _sendfile_use_sendfile(self, file, offset=0, count=None):
            raise _GiveupOnSendfile(
                "os.sendfile() not available on this platform")

    def _sendfile_use_send(self, file, offset=0, count=None):
        self._check_sendfile_params(file, offset, count)
        if self.gettimeout() == 0:
            raise ValueError("non-blocking sockets are not supported")
        if offset:
            file.seek(offset)
        blocksize = min(count, 8192) if count else 8192
        total_sent = 0
        read = file.read
        send = self.send
        try:
            while True:
                if count:
                    blocksize = min(count - total_sent, blocksize)
                    if blocksize <= 0:
                        break
                data = memoryview(read(blocksize))
                if not data:
                    break
                while True:
                    try:
                        sent = send(data)
                    except BlockingIOError:
                        continue
                    total_sent += sent
                    if sent < len(data):
                        data = data[sent:]
                    else:
                        break
            return total_sent
        finally:
            if total_sent > 0 and hasattr(file, 'seek'):
                file.seek(offset + total_sent)

    def _check_sendfile_params(self, file, offset, count):
        if 'b' not in getattr(file, 'mode', 'b'):
            raise ValueError("file should be opened in binary mode")
        if not self.type & SOCK_STREAM:
            raise ValueError("only SOCK_STREAM type sockets are supported")
        if count is not None:
            if not isinstance(count, int):
                raise TypeError(
                    "count must be a positive integer (got {!r})".format(count))
            if count <= 0:
                raise ValueError(
                    "count must be a positive integer (got {!r})".format(count))

    def sendfile(self, file, offset=0, count=None):
        """Send a binary file over the socket; return the number of bytes sent.

        os.sendfile() is used when possible, send() otherwise.  The file
        position is left after the last byte sent.
        """
        try:
            return self._sendfile_use_sendfile(file, offset, count)
        except _GiveupOnSendfile:
            return self._sendfile_use_send(file, offset, count)

    def _decref_socketios(self):
        if self._io_refs > 0:
            self._io_refs -= 1
        if self._closed:
            self.close()

    def _real_close(self, _ss=_socket.socket):
        # must not touch module globals: may run during interpreter shutdown
        _ss.close(self)

    def close(self):
        """Close the socket (once all makefile() objects are closed too)."""
        self._closed = True
        if self._io_refs <= 0:
            self._real_close()

    def detach(self):
        """Disown the file descriptor and return it; the object becomes closed."""
        self._closed = True
        return super().detach()

    @property
    def family(self):
        """The socket's address family."""
        return _intenum_converter(super().family, AddressFamily)

    @property
    def type(self):
        """The socket type."""
        return _intenum_converter(super().type, SocketKind)

    if os.name == 'nt':
        def get_inheritable(self):
            return os.get_handle_inheritable(self.fileno())
        def set_inheritable(self, inheritable):
            os.set_handle_inheritable(self.fileno(), inheritable)
    else:
        def get_inheritable(self):
            return os.get_inheritable(self.fileno())
        def set_inheritable(self, inheritable):
            os.set_inheritable(self.fileno(), inheritable)
    get_inheritable.__doc__ = "Get the inheritable flag of the socket"
    set_inheritable.__doc__ = "Set the inheritable flag of the socket"


def fromfd(fd, family, type, proto=0):
    """Create a socket object from a duplicate of file descriptor *fd*."""
    nfd = dup(fd)
    return socket(family, type, proto, nfd)


if hasattr(_socket.socket, "sendmsg"):
    def send_fds(sock, buffers, fds, flags=0, address=None):
        """Send the file descriptors *fds* over an AF_UNIX socket."""
        import array

        return sock.sendmsg(buffers, [(_socket.SOL_SOCKET,
                                       _socket.SCM_RIGHTS, array.array("i", fds))])
    __all__.append("send_fds")

if hasattr(_socket.socket, "recvmsg"):
    def recv_fds(sock, bufsize, maxfds, flags=0):
        """Receive data and up to *maxfds* file descriptors.

        Return (data, list of fds, msg_flags, address).
        """
        import array

        fds = array.array("i")
        msg, ancdata, flags, addr = sock.recvmsg(
            bufsize, _socket.CMSG_LEN(maxfds * fds.itemsize))
        for level, kind, data in ancdata:
            if level == _socket.SOL_SOCKET and kind == _socket.SCM_RIGHTS:
                fds.frombytes(data[:len(data) - (len(data) % fds.itemsize)])
        return msg, list(fds), flags, addr
    __all__.append("recv_fds")

if hasattr(_socket.socket, "share"):
    def fromshare(info):
        """Create a socket from the bytes returned by socket.share()."""
        return socket(0, 0, 0, info)
    __all__.append("fromshare")


def _fallback_socketpair(family=AF_INET, type=SOCK_STREAM, proto=0):
    # socketpair() emulation over a loopback TCP connection (kept available
    # everywhere so the tests can exercise it)
    if family == AF_INET:
        host = _LOCALHOST
    elif family == AF_INET6:
        host = _LOCALHOST_V6
    else:
        raise ValueError("Only AF_INET and AF_INET6 socket address families "
                         "are supported")
    if type != SOCK_STREAM:
        raise ValueError("Only SOCK_STREAM socket type is supported")
    if proto != 0:
        raise ValueError("Only protocol zero is supported")

    lsock = socket(family, type, proto)
    try:
        lsock.bind((host, 0))
        lsock.listen()
        addr, port = lsock.getsockname()[:2]
        csock = socket(family, type, proto)
        try:
            csock.setblocking(False)
            try:
                csock.connect((addr, port))
            except (BlockingIOError, InterruptedError):
                pass
            csock.setblocking(True)
            ssock, _ = lsock.accept()
        except:
            csock.close()
            raise
    finally:
        lsock.close()

    # make sure the accepted connection really is our own client
    try:
        if (ssock.getsockname() != csock.getpeername()
                or csock.getsockname() != ssock.getpeername()):
            raise ConnectionError("Unexpected peer connection")
    except:
        ssock.close()
        csock.close()
        raise
    return (ssock, csock)


if hasattr(_socket, "socketpair"):
    def socketpair(family=None, type=SOCK_STREAM, proto=0):
        if family is None:
            try:
                family = AF_UNIX
            except NameError:
                family = AF_INET
        a, b = _socket.socketpair(family, type, proto)
        a = socket(family, type, proto, a.detach())
        b = socket(family, type, proto, b.detach())
        return a, b
else:
    socketpair = _fallback_socketpair
    __all__.append("socketpair")

socketpair.__doc__ = """socketpair([family[, type[, proto]]]) -> (socket object, socket object)

Return a pair of connected socket objects.  The default family is AF_UNIX
where available, AF_INET otherwise.
"""

_blocking_errnos = {EAGAIN, EWOULDBLOCK}


class SocketIO(io.RawIOBase):
    """Raw binary stream on top of a stream socket (used by makefile())."""

    def __init__(self, sock, mode):
        if mode not in ("r", "w", "rw", "rb", "wb", "rwb"):
            raise ValueError("invalid mode: %r" % mode)
        io.RawIOBase.__init__(self)
        self._sock = sock
        if "b" not in mode:
            mode += "b"
        self._mode = mode
        self._reading = "r" in mode
        self._writing = "w" in mode
        self._timeout_occurred = False

    def readinto(self, b):
        """Receive into *b*; None if a non-blocking socket has no data."""
        self._checkClosed()
        self._checkReadable()
        if self._timeout_occurred:
            raise OSError("cannot read from timed out object")
        try:
            return self._sock.recv_into(b)
        except timeout:
            self._timeout_occurred = True
            raise
        except error as e:
            if e.errno in _blocking_errnos:
                return None
            raise

    def write(self, b):
        """Send *b*; return the byte count or None if it would block."""
        self._checkClosed()
        self._checkWritable()
        try:
            return self._sock.send(b)
        except error as e:
            if e.errno in _blocking_errnos:
                return None
            raise

    def readable(self):
        if self.closed:
            raise ValueError("I/O operation on closed socket.")
        return self._reading

    def writable(self):
        if self.closed:
            raise ValueError("I/O operation on closed socket.")
        return self._writing

    def seekable(self):
        if self.closed:
            raise ValueError("I/O operation on closed socket.")
        return super().seekable()

    def fileno(self):
        self._checkClosed()
        return self._sock.fileno()

    @property
    def name(self):
        if not self.closed:
            return self.fileno()
        return -1

    @property
    def mode(self):
        return self._mode

    def close(self):
        """Close this stream; the socket closes once nothing references it."""
        if self.closed:
            return
        io.RawIOBase.close(self)
        self._sock._decref_socketios()
        self._sock = None


def getfqdn(name=''):
    """Return a fully qualified domain name for *name* (default: this host)."""
    name = name.strip()
    if not name or name in ('0.0.0.0', '::'):
        name = gethostname()
    try:
        hostname, aliases, ipaddrs = gethostbyaddr(name)
    except error:
        pass
    else:
        aliases.insert(0, hostname)
        for name in aliases:
            if '.' in name:
                break
        else:
            name = hostname
    return name


_GLOBAL_DEFAULT_TIMEOUT = object()


def create_connection(address, timeout=_GLOBAL_DEFAULT_TIMEOUT,
                      source_address=None, *, all_errors=False):
    """Connect a TCP socket to *address* (host, port) and return it.

    Each address returned by getaddrinfo() is tried in turn.  On failure the
    last error is raised, or an ExceptionGroup of all errors if *all_errors*.
    """
    host, port = address
    exceptions = []
    for af, socktype, proto, canonname, sa in getaddrinfo(host, port, 0, SOCK_STREAM):
        sock = None
        try:
            sock = socket(af, socktype, proto)
            if timeout is not _GLOBAL_DEFAULT_TIMEOUT:
                sock.settimeout(timeout)
            if source_address:
                sock.bind(source_address)
            sock.connect(sa)
            exceptions.clear()
            return sock
        except error as exc:
            if not all_errors:
                exceptions.clear()
            exceptions.append(exc)
            if sock is not None:
                sock.close()

    if exceptions:
        try:
            if not all_errors:
                raise exceptions[0]
            raise ExceptionGroup("create_connection failed", exceptions)
        finally:
            exceptions.clear()
    raise error("getaddrinfo returns an empty list")


def has_dualstack_ipv6():
    """Return True if an AF_INET6 socket can accept IPv4 connections too."""
    if (not has_ipv6 or not hasattr(_socket, 'IPPROTO_IPV6')
            or not hasattr(_socket, 'IPV6_V6ONLY')):
        return False
    try:
        with socket(AF_INET6, SOCK_STREAM) as sock:
            sock.setsockopt(IPPROTO_IPV6, IPV6_V6ONLY, 0)
            return True
    except error:
        return False


def create_server(address, *, family=AF_INET, backlog=None, reuse_port=False,
                  dualstack_ipv6=False):
    """Create a listening TCP socket bound to *address* and return it."""
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
                pass
        if reuse_port and family in (AF_INET, AF_INET6):
            sock.setsockopt(SOL_SOCKET, SO_REUSEPORT, 1)
        if has_ipv6 and family == AF_INET6:
            if dualstack_ipv6:
                sock.setsockopt(IPPROTO_IPV6, IPV6_V6ONLY, 0)
            elif hasattr(_socket, "IPV6_V6ONLY") and hasattr(_socket, "IPPROTO_IPV6"):
                sock.setsockopt(IPPROTO_IPV6, IPV6_V6ONLY, 1)
        try:
            sock.bind(address)
        except error as err:
            msg = '%s (while attempting to bind on address %r)' % (err.strerror, address)
            raise error(err.errno, msg) from None
        if backlog is None:
            sock.listen()
        else:
            sock.listen(backlog)
        return sock
    except error:
        sock.close()
        raise


def getaddrinfo(host, port, family=0, type=0, proto=0, flags=0):
    """Resolve (host, port) into a list of
    (family, type, proto, canonname, sockaddr) tuples, with family and type
    converted to AddressFamily / SocketKind members.
    """
    addrlist = []
    for af, socktype, proto, canonname, sa in _socket.getaddrinfo(
            host, port, family, type, proto, flags):
        addrlist.append((_intenum_converter(af, AddressFamily),
                         _intenum_converter(socktype, SocketKind),
                         proto, canonname, sa))
    return addrlist
