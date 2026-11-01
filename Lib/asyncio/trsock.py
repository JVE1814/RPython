import socket


class TransportSocket:

    """A safe wrapper around the socket of a transport.

    Only operations that cannot break the transport (no I/O, no state
    changes like blocking mode) are exposed.
    """

    __slots__ = ('_sock',)

    def __init__(self, sock: socket.socket):
        self._sock = sock

    @property
    def family(self):
        return self._sock.family

    @property
    def type(self):
        return self._sock.type

    @property
    def proto(self):
        return self._sock.proto

    def __repr__(self):
        s = (
            f"<asyncio.TransportSocket fd={self.fileno()}, "
            f"family={self.family!s}, type={self.type!s}, "
            f"proto={self.proto}"
        )

        if self.fileno() != -1:
            try:
                laddr = self.getsockname()
                if laddr:
                    s = f"{s}, laddr={laddr}"
            except socket.error:
                pass
            try:
                raddr = self.getpeername()
                if raddr:
                    s = f"{s}, raddr={raddr}"
            except socket.error:
                pass

        return f"{s}>"

    def __getstate__(self):
        raise TypeError("Cannot serialize asyncio.TransportSocket object")

    def fileno(self):
        return self._sock.fileno()

    def dup(self):
        return self._sock.dup()

    def get_inheritable(self):
        return self._sock.get_inheritable()

    def shutdown(self, how):
        # Used by e.g. aiohttp; it is a plain socket operation.
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

    def settimeout(self, value):
        if value == 0:
            return
        raise ValueError(
            'settimeout(): only 0 timeout is allowed on transport sockets')

    def gettimeout(self):
        return 0

    def setblocking(self, flag):
        if not flag:
            return
        raise ValueError(
            'setblocking(): transport sockets cannot be switched '
            'to blocking mode')
