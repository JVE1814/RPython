"""Restricted view of a transport's socket."""

import socket


class TransportSocket:
    """Wrapper exposing only socket operations that cannot disturb the
    owning transport (no I/O, no blocking-mode changes)."""

    __slots__ = ('_sock',)

    def __init__(self, sock: socket.socket):
        self._sock = sock

    family = property(lambda self: self._sock.family)
    type = property(lambda self: self._sock.type)
    proto = property(lambda self: self._sock.proto)

    def __repr__(self):
        text = (f"<asyncio.TransportSocket fd={self.fileno()}, "
                f"family={self.family!s}, type={self.type!s}, "
                f"proto={self.proto}")
        if self.fileno() != -1:
            for label, getter in (('laddr', self.getsockname),
                                  ('raddr', self.getpeername)):
                try:
                    addr = getter()
                except socket.error:
                    continue
                if addr:
                    text += f", {label}={addr}"
        return text + ">"

    def __getstate__(self):
        raise TypeError("Cannot serialize asyncio.TransportSocket object")

    def fileno(self):
        return self._sock.fileno()

    def dup(self):
        return self._sock.dup()

    def get_inheritable(self):
        return self._sock.get_inheritable()

    def shutdown(self, how):
        self._sock.shutdown(how)

    def getsockopt(self, *args, **kwargs):
        return self._sock.getsockopt(*args, **kwargs)

    def setsockopt(self, *args, **kwargs):
        self._sock.setsockopt(*args, **kwargs)

    def getpeername(self):
        return self._sock.getpeername()

    def getsockname(self):
        return self._sock.getsockname()

    def getsockbyname(self):
        return self._sock.getsockbyname()

    def gettimeout(self):
        return 0

    def settimeout(self, value):
        if value != 0:
            raise ValueError('settimeout(): only 0 timeout is allowed on '
                             'transport sockets')

    def setblocking(self, flag):
        if flag:
            raise ValueError('setblocking(): transport sockets cannot be '
                             'switched to blocking mode')
