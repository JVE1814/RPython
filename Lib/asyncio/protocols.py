"""Abstract protocol classes."""

__all__ = (
    'BaseProtocol', 'Protocol', 'DatagramProtocol',
    'SubprocessProtocol', 'BufferedProtocol',
)


class BaseProtocol:
    """Common base class for protocol interfaces.

    Callbacks are invoked by transports; the default implementations do
    nothing.
    """

    __slots__ = ()

    def connection_made(self, transport):
        """Called when a connection is made; *transport* represents it."""

    def connection_lost(self, exc):
        """Called when the connection is lost or closed (exc is None on
        EOF or close)."""

    def pause_writing(self):
        """Called when the transport's buffer goes over the high-water mark."""

    def resume_writing(self):
        """Called when the transport's buffer drains below the low-water
        mark."""


class Protocol(BaseProtocol):
    """Interface for stream protocols.

    State machine: start -> connection_made [-> data_received]*
    [-> eof_received]? -> connection_lost -> end
    """

    __slots__ = ()

    def data_received(self, data):
        """Called when some data (bytes) is received."""

    def eof_received(self):
        """Called when the other end called write_eof().

        Returning a true value keeps the transport open (half-closed).
        """


class BufferedProtocol(BaseProtocol):
    """Interface for stream protocols with manual control of the receive
    buffer.

    State machine: start -> connection_made
    [-> get_buffer [-> buffer_updated]?]* [-> eof_received]?
    -> connection_lost -> end
    """

    __slots__ = ()

    def get_buffer(self, sizehint):
        """Return a non-empty writable buffer to receive data into
        (*sizehint* is a hint, -1 meaning any size)."""

    def buffer_updated(self, nbytes):
        """Called when *nbytes* bytes were written into the buffer."""

    def eof_received(self):
        """Called when the other end called write_eof()."""


class DatagramProtocol(BaseProtocol):
    """Interface for datagram protocols."""

    __slots__ = ()

    def datagram_received(self, data, addr):
        """Called when a datagram is received."""

    def error_received(self, exc):
        """Called when a send or receive operation fails (OSError)."""


class SubprocessProtocol(BaseProtocol):
    """Interface for protocols of subprocess transports."""

    __slots__ = ()

    def pipe_data_received(self, fd, data):
        """Called when the child writes *data* to its stdout/stderr (*fd*)."""

    def pipe_connection_lost(self, fd, exc):
        """Called when a pipe of the child is closed."""

    def process_exited(self):
        """Called when the subprocess has exited."""


def _feed_data_to_buffered_proto(proto, data):
    data_len = len(data)
    while data_len:
        buf = proto.get_buffer(data_len)
        buf_len = len(buf)
        if not buf_len:
            raise RuntimeError('get_buffer() returned an empty buffer')

        if buf_len >= data_len:
            buf[:data_len] = data
            proto.buffer_updated(data_len)
            return
        else:
            buf[:buf_len] = data[:buf_len]
            proto.buffer_updated(buf_len)
            data = data[buf_len:]
            data_len = len(data)
