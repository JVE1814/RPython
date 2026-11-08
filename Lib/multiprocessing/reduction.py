#
# multiprocessing.reduction -- pickling for inter-process transfer.
#
# ForkingPickler is an ordinary pickler with an extra table of reducers
# (bound methods, method descriptors, sockets, connections, ...).  This
# module also passes open file descriptors between processes over Unix
# domain sockets (SCM_RIGHTS).
#

from abc import ABCMeta
import array
import copyreg
import io
import os
import pickle
import socket
import sys

from . import context

__all__ = ['send_handle', 'recv_handle', 'ForkingPickler', 'register', 'dump']


HAVE_SEND_HANDLE = (sys.platform == 'win32' or
                    (hasattr(socket, 'CMSG_LEN') and
                     hasattr(socket, 'SCM_RIGHTS') and
                     hasattr(socket.socket, 'sendmsg')))


class ForkingPickler(pickle.Pickler):
    """Pickler that also applies the reducers registered with register()."""

    # type -> reduce function, shared by every ForkingPickler
    _extra_reducers = {}
    _copyreg_dispatch_table = copyreg.dispatch_table

    def __init__(self, *args, **kwds):
        super().__init__(*args, **kwds)
        # copyreg's table first, ours on top; built per pickler so that later
        # registrations are seen by later picklers.
        self.dispatch_table = {**self._copyreg_dispatch_table,
                               **self._extra_reducers}

    @classmethod
    def register(cls, type, reduce):
        """Pickle instances of *type* through reduce(obj)."""
        cls._extra_reducers[type] = reduce

    @classmethod
    def dumps(cls, obj, protocol=None):
        out = io.BytesIO()
        cls(out, protocol).dump(obj)
        return out.getbuffer()

    loads = pickle.loads


register = ForkingPickler.register


def dump(obj, file, protocol=None):
    """Write *obj* to *file* using ForkingPickler."""
    ForkingPickler(file, protocol).dump(obj)


if sys.platform == 'win32':
    raise ImportError('multiprocessing.reduction: Windows is not supported')

__all__ += ['DupFd', 'sendfds', 'recvfds']


# --- passing file descriptors ----------------------------------------------
#
# One sendmsg() carries the descriptors as ancillary data plus a single data
# byte holding their count modulo 256, which the receiver checks.  macOS may
# drop descriptors whose sender closes them too early, so there the
# receiver confirms with one byte before the sender may go on.

ACKNOWLEDGE = sys.platform == 'darwin'
_ACK = b'A'


def sendfds(sock, fds):
    """Send the descriptors in *fds* over the AF_UNIX socket *sock*."""
    packed = array.array('i', fds)
    sock.sendmsg([bytes([len(packed) & 0xff])],
                 [(socket.SOL_SOCKET, socket.SCM_RIGHTS, packed.tobytes())])
    if ACKNOWLEDGE and sock.recv(1) != _ACK:
        raise RuntimeError('did not receive acknowledgement of fd')


def recvfds(sock, size):
    """Receive up to *size* descriptors sent by sendfds()."""
    packed = array.array('i')
    room = socket.CMSG_SPACE(size * packed.itemsize)
    data, ancillary, _flags, _addr = sock.recvmsg(1, room)
    if not data and not ancillary:
        raise EOFError
    try:
        if ACKNOWLEDGE:
            sock.send(_ACK)
        if len(ancillary) != 1:
            raise RuntimeError('received %d items of ancdata' % len(ancillary))
        level, kind, payload = ancillary[0]
        if level != socket.SOL_SOCKET or kind != socket.SCM_RIGHTS:
            raise RuntimeError('Invalid data received')
        whole = len(payload) - len(payload) % packed.itemsize
        packed.frombytes(payload[:whole])
        if len(packed) & 0xff != data[0]:
            raise AssertionError(
                "Len is {0:n} but msg[0] is {1!r}".format(len(packed), data[0]))
        return list(packed)
    except (ValueError, IndexError):
        pass
    raise RuntimeError('Invalid data received')


def _as_unix_socket(conn):
    return socket.fromfd(conn.fileno(), socket.AF_UNIX, socket.SOCK_STREAM)


def send_handle(conn, handle, destination_pid):
    """Pass the descriptor *handle* to the process at the other end of the
    connection *conn*."""
    with _as_unix_socket(conn) as sock:
        sendfds(sock, [handle])


def recv_handle(conn):
    """Receive a descriptor passed with send_handle()."""
    with _as_unix_socket(conn) as sock:
        return recvfds(sock, 1)[0]


def DupFd(fd):
    """Return a picklable stand-in for *fd*; unpickling it in another
    process yields a duplicate of the descriptor there."""
    starting = context.get_spawning_popen()
    if starting is not None:
        # Part of starting a child: the Popen object knows how to hand the
        # descriptor over.
        return starting.DupFd(starting.duplicate_for_child(fd))
    if HAVE_SEND_HANDLE:
        from . import resource_sharer
        return resource_sharer.DupFd(fd)
    raise ValueError('SCM_RIGHTS appears not to be available')


# --- reducers for objects plain pickle refuses -----------------------------

def _reduce_method(m):
    # A bound method is rebuilt by looking the name up on its object again.
    owner = m.__class__ if m.__self__ is None else m.__self__
    return getattr, (owner, m.__func__.__name__)


def _reduce_method_descriptor(m):
    return getattr, (m.__objclass__, m.__name__)


def _reduce_socket(s):
    return _rebuild_socket, (DupFd(s.fileno()), s.family, s.type, s.proto)


def _rebuild_socket(df, family, type, proto):
    return socket.socket(family, type, proto, fileno=df.detach())


class _Sample:
    def method(self):
        pass


_BOUND_METHOD = type(_Sample().method)
_METHOD_DESCRIPTORS = (type(list.append), type(int.__add__))
del _Sample


def _register_builtin_reducers():
    register(_BOUND_METHOD, _reduce_method)
    for kind in _METHOD_DESCRIPTORS:
        register(kind, _reduce_method_descriptor)
    register(socket.socket, _reduce_socket)


_register_builtin_reducers()


class AbstractReducer(metaclass=ABCMeta):
    """Base for a replacement reduction module: set an instance as a
    context's `reducer`.  It offers the same functions as this module;
    subclasses override what they need."""

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
    _reduce_socket = _reduce_socket
    _rebuild_socket = _rebuild_socket

    def __init__(self, *args):
        _register_builtin_reducers()
