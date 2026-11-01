"""Protocol interfaces: the callbacks transports invoke."""

__all__ = (
    'BaseProtocol', 'Protocol', 'DatagramProtocol',
    'SubprocessProtocol', 'BufferedProtocol',
)


class BaseProtocol:
    """Callbacks common to all protocols (all no-ops by default)."""

    __slots__ = ()

    def connection_made(self, transport):
        """The connection is up; *transport* is the way to talk to it."""

    def connection_lost(self, exc):
        """The connection is gone (exc is None for a normal close/EOF)."""

    def pause_writing(self):
        """The transport's write buffer went above the high-water mark."""

    def resume_writing(self):
        """The transport's write buffer drained to the low-water mark."""


class Protocol(BaseProtocol):
    """Stream protocol: connection_made, data_received*, eof_received?,
    connection_lost."""

    __slots__ = ()

    def data_received(self, data):
        """Some bytes arrived."""

    def eof_received(self):
        """The peer will send nothing more; a true return value keeps our
        side open for writing."""


class BufferedProtocol(BaseProtocol):
    """Stream protocol that lends its own buffer for receiving."""

    __slots__ = ()

    def get_buffer(self, sizehint):
        """Return a writable, non-empty buffer (sizehint -1: any size)."""

    def buffer_updated(self, nbytes):
        """*nbytes* bytes were written into the last buffer returned."""

    def eof_received(self):
        """The peer will send nothing more."""


class DatagramProtocol(BaseProtocol):
    """Datagram (UDP-like) protocol."""

    __slots__ = ()

    def datagram_received(self, data, addr):
        """A datagram arrived from *addr*."""

    def error_received(self, exc):
        """A send or receive failed with OSError *exc*."""


class SubprocessProtocol(BaseProtocol):
    """Protocol of a subprocess transport."""

    __slots__ = ()

    def pipe_data_received(self, fd, data):
        """The child wrote *data* on its stdout (fd 1) or stderr (fd 2)."""

    def pipe_connection_lost(self, fd, exc):
        """One of the child's pipes was closed."""

    def process_exited(self):
        """The child process terminated."""


def _feed_data_to_buffered_proto(proto, data):
    """Deliver *data* through get_buffer()/buffer_updated()."""
    view = memoryview(data)
    while view:
        buf = proto.get_buffer(len(view))
        room = len(buf)
        if not room:
            raise RuntimeError('get_buffer() returned an empty buffer')
        n = min(room, len(view))
        buf[:n] = view[:n]
        proto.buffer_updated(n)
        view = view[n:]
