#
# multiprocessing.reduction: the pickler used to send objects to children,
# plus file-descriptor passing over Unix sockets.
#

from abc import ABCMeta
import copyreg
import functools
import io
import os
import pickle
import socket
import struct
import sys

from . import context

__all__ = ['send_handle', 'recv_handle', 'ForkingPickler', 'register', 'dump']


HAVE_SEND_HANDLE = (sys.platform == 'win32' or
                    (hasattr(socket, 'CMSG_LEN') and
                     hasattr(socket, 'SCM_RIGHTS') and
                     hasattr(socket.socket, 'sendmsg')))


class ForkingPickler(pickle.Pickler):
    """Pickler that knows how to reduce the objects multiprocessing passes
    to children (connections, sockets, methods, ...)."""

    _extra_reducers = {}
    _copyreg_dispatch_table = copyreg.dispatch_table

    def __init__(self, *args, **kwds):
        super().__init__(*args, **kwds)
        table = self._copyreg_dispatch_table.copy()
        table.update(self._extra_reducers)
        self.dispatch_table = table

    @classmethod
    def register(cls, type, reduce):
        """Use *reduce(obj)* to pickle instances of *type*."""
        cls._extra_reducers[type] = reduce

    @classmethod
    def dumps(cls, obj, protocol=None):
        buf = io.BytesIO()
        cls(buf, protocol).dump(obj)
        return buf.getbuffer()

    loads = pickle.loads


register = ForkingPickler.register


def dump(obj, file, protocol=None):
    """Pickle *obj* to *file* with the ForkingPickler."""
    ForkingPickler(file, protocol).dump(obj)


# ---------------------------------------------------------------- handles

if sys.platform == 'win32':
    raise ImportError('multiprocessing.reduction: Windows is not supported')

__all__ += ['DupFd', 'sendfds', 'recvfds']

_INT = struct.Struct('i')
# macOS needs an explicit acknowledgement before the sender may close the fds
ACKNOWLEDGE = sys.platform == 'darwin'


def sendfds(sock, fds):
    """Send the file descriptors *fds* over the Unix socket *sock*."""
    fds = list(fds)
    payload = b''.join(_INT.pack(fd) for fd in fds)
    header = bytes([len(fds) % 256])
    sock.sendmsg([header], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, payload)])
    if ACKNOWLEDGE and sock.recv(1) != b'A':
        raise RuntimeError('did not receive acknowledgement of fd')


def recvfds(sock, size):
    """Receive *size* file descriptors from the Unix socket *sock*."""
    nbytes = _INT.size * size
    msg, ancdata, flags, addr = sock.recvmsg(1, socket.CMSG_SPACE(nbytes))
    if not msg and not ancdata:
        raise EOFError
    try:
        if ACKNOWLEDGE:
            sock.send(b'A')
        if len(ancdata) != 1:
            raise RuntimeError('received %d items of ancdata' % len(ancdata))
        level, kind, data = ancdata[0]
        if level != socket.SOL_SOCKET or kind != socket.SCM_RIGHTS:
            raise RuntimeError('Invalid data received')
        usable = len(data) - len(data) % _INT.size
        fds = [_INT.unpack_from(data, i)[0] for i in range(0, usable, _INT.size)]
        if len(fds) % 256 != msg[0]:
            raise AssertionError(
                "Len is {0:n} but msg[0] is {1!r}".format(len(fds), msg[0]))
        return fds
    except (ValueError, IndexError):
        pass
    raise RuntimeError('Invalid data received')


def send_handle(conn, handle, destination_pid):
    """Send the file descriptor *handle* over the connection *conn*."""
    with socket.fromfd(conn.fileno(), socket.AF_UNIX, socket.SOCK_STREAM) as s:
        sendfds(s, [handle])


def recv_handle(conn):
    """Receive a file descriptor sent with send_handle()."""
    with socket.fromfd(conn.fileno(), socket.AF_UNIX, socket.SOCK_STREAM) as s:
        return recvfds(s, 1)[0]


def DupFd(fd):
    """Return an object which, once unpickled in a child, yields a
    duplicate of *fd*."""
    popen_obj = context.get_spawning_popen()
    if popen_obj is not None:
        return popen_obj.DupFd(popen_obj.duplicate_for_child(fd))
    elif HAVE_SEND_HANDLE:
        from . import resource_sharer
        return resource_sharer.DupFd(fd)
    else:
        raise ValueError('SCM_RIGHTS appears not to be available')


# ---------------------------------------------------------------- reducers

def _reduce_method(m):
    if m.__self__ is None:
        return getattr, (m.__class__, m.__func__.__name__)
    return getattr, (m.__self__, m.__func__.__name__)


class _C:
    def f(self):
        pass


register(type(_C().f), _reduce_method)


def _reduce_method_descriptor(m):
    return getattr, (m.__objclass__, m.__name__)


register(type(list.append), _reduce_method_descriptor)
register(type(int.__add__), _reduce_method_descriptor)


def _reduce_partial(p):
    return _rebuild_partial, (p.func, p.args, p.keywords or {})


def _rebuild_partial(func, args, keywords):
    return functools.partial(func, *args, **keywords)




def _reduce_socket(s):
    df = DupFd(s.fileno())
    return _rebuild_socket, (df, s.family, s.type, s.proto)


def _rebuild_socket(df, family, type, proto):
    fd = df.detach()
    return socket.socket(family, type, proto, fileno=fd)


register(socket.socket, _reduce_socket)


class AbstractReducer(metaclass=ABCMeta):
    """Base for custom reducers: a context's `reducer` may be set to an
    instance exposing the same functions as this module."""

    ForkingPickler = ForkingPickler
    register = register
    dump = dump
    send_handle = send_handle
    recv_handle = recv_handle
    sendfds = sendfds
    recvfds = recvfds
    DupFd = DupFd
    _reduce_method = _reduce_method
    _reduce_method_descriptor = _reduce_method_descriptor
    _rebuild_partial = _rebuild_partial
    _reduce_socket = _reduce_socket
    _rebuild_socket = _rebuild_socket

    def __init__(self, *args):
        register(type(_C().f), _reduce_method)
        register(type(list.append), _reduce_method_descriptor)
        register(type(int.__add__), _reduce_method_descriptor)
        register(socket.socket, _reduce_socket)
