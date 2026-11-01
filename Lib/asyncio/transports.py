"""Abstract Transport class."""

__all__ = (
    'BaseTransport', 'ReadTransport', 'WriteTransport',
    'Transport', 'DatagramTransport', 'SubprocessTransport',
)


class BaseTransport:
    """Base class for transports."""

    __slots__ = ('_extra',)

    def __init__(self, extra=None):
        if extra is None:
            extra = {}
        self._extra = extra

    def get_extra_info(self, name, default=None):
        """Get optional transport information."""
        return self._extra.get(name, default)

    def is_closing(self):
        """Return True if the transport is closing or closed."""
        raise NotImplementedError

    def close(self):
        """Close the transport.

        Buffered data is flushed asynchronously; then the protocol's
        connection_lost() is called with None.
        """
        raise NotImplementedError

    def set_protocol(self, protocol):
        """Set a new protocol."""
        raise NotImplementedError

    def get_protocol(self):
        """Return the current protocol."""
        raise NotImplementedError


class ReadTransport(BaseTransport):
    """Interface for read-only transports."""

    __slots__ = ()

    def is_reading(self):
        """Return True if the transport is receiving."""
        raise NotImplementedError

    def pause_reading(self):
        """Stop calling the protocol's data_received() until resumed."""
        raise NotImplementedError

    def resume_reading(self):
        """Resume calling the protocol's data_received()."""
        raise NotImplementedError


class WriteTransport(BaseTransport):
    """Interface for write-only transports."""

    __slots__ = ()

    def set_write_buffer_limits(self, high=None, low=None):
        """Set the high- and low-water limits for write flow control.

        pause_writing() is called on the protocol when the buffer goes
        over *high*, resume_writing() when it drains to *low* or below.
        """
        raise NotImplementedError

    def get_write_buffer_size(self):
        """Return the current size of the write buffer."""
        raise NotImplementedError

    def get_write_buffer_limits(self):
        """Get the high and low watermarks for write flow control.
        Return a tuple (low, high) where low and high are
        positive number of bytes."""
        raise NotImplementedError

    def write(self, data):
        """Write some data bytes to the transport (never blocks)."""
        raise NotImplementedError

    def writelines(self, list_of_data):
        """Write a list (or any iterable) of data bytes to the transport."""
        data = b''.join(list_of_data)
        self.write(data)

    def write_eof(self):
        """Close the write end after flushing buffered data."""
        raise NotImplementedError

    def can_write_eof(self):
        """Return True if this transport supports write_eof()."""
        raise NotImplementedError

    def abort(self):
        """Close the transport immediately, discarding buffered data."""
        raise NotImplementedError


class Transport(ReadTransport, WriteTransport):
    """Interface representing a bidirectional transport."""

    __slots__ = ()


class DatagramTransport(BaseTransport):
    """Interface for datagram (UDP) transports."""

    __slots__ = ()

    def sendto(self, data, addr=None):
        """Send *data* (bytes) to *addr* (or the connected peer)."""
        raise NotImplementedError

    def abort(self):
        """Close the transport immediately, discarding buffered data."""
        raise NotImplementedError


class SubprocessTransport(BaseTransport):

    __slots__ = ()

    def get_pid(self):
        """Get subprocess id."""
        raise NotImplementedError

    def get_returncode(self):
        """Get subprocess returncode (None while running)."""
        raise NotImplementedError

    def get_pipe_transport(self, fd):
        """Get transport for pipe with number fd."""
        raise NotImplementedError

    def send_signal(self, signal):
        """Send signal to subprocess."""
        raise NotImplementedError

    def terminate(self):
        """Stop the subprocess (SIGTERM)."""
        raise NotImplementedError

    def kill(self):
        """Kill the subprocess (SIGKILL)."""
        raise NotImplementedError


class _FlowControlMixin(Transport):
    """Write flow control for transports.

    Subclasses implement get_write_buffer_size() and call
    _maybe_pause_protocol() after buffering data and
    _maybe_resume_protocol() after draining it.  They must define
    self._loop and self._protocol.
    """

    __slots__ = ('_loop', '_protocol_paused', '_high_water', '_low_water')

    def __init__(self, extra=None, loop=None):
        super().__init__(extra)
        assert loop is not None
        self._loop = loop
        self._protocol_paused = False
        self._set_write_buffer_limits()

    def _maybe_pause_protocol(self):
        size = self.get_write_buffer_size()
        if size <= self._high_water:
            return
        if not self._protocol_paused:
            self._protocol_paused = True
            try:
                self._protocol.pause_writing()
            except (SystemExit, KeyboardInterrupt):
                raise
            except BaseException as exc:
                self._loop.call_exception_handler({
                    'message': 'protocol.pause_writing() failed',
                    'exception': exc,
                    'transport': self,
                    'protocol': self._protocol,
                })

    def _maybe_resume_protocol(self):
        if (self._protocol_paused and
                self.get_write_buffer_size() <= self._low_water):
            self._protocol_paused = False
            try:
                self._protocol.resume_writing()
            except (SystemExit, KeyboardInterrupt):
                raise
            except BaseException as exc:
                self._loop.call_exception_handler({
                    'message': 'protocol.resume_writing() failed',
                    'exception': exc,
                    'transport': self,
                    'protocol': self._protocol,
                })

    def get_write_buffer_limits(self):
        return (self._low_water, self._high_water)

    def _set_write_buffer_limits(self, high=None, low=None):
        if high is None:
            if low is None:
                high = 64 * 1024
            else:
                high = 4 * low
        if low is None:
            low = high // 4

        if not high >= low >= 0:
            raise ValueError(
                f'high ({high!r}) must be >= low ({low!r}) must be >= 0')

        self._high_water = high
        self._low_water = low

    def set_write_buffer_limits(self, high=None, low=None):
        self._set_write_buffer_limits(high=high, low=low)
        self._maybe_pause_protocol()

    def get_write_buffer_size(self):
        raise NotImplementedError
