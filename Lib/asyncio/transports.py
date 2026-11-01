"""Transport interfaces and write flow control."""

__all__ = (
    'BaseTransport', 'ReadTransport', 'WriteTransport',
    'Transport', 'DatagramTransport', 'SubprocessTransport',
)


def _abstract(name, doc):
    def method(self, *args, **kwargs):
        raise NotImplementedError
    method.__name__ = name
    method.__doc__ = doc
    return method


class BaseTransport:
    """Common base of all transports."""

    __slots__ = ('_extra',)

    def __init__(self, extra=None):
        self._extra = {} if extra is None else extra

    def get_extra_info(self, name, default=None):
        """Optional information about the transport (peername, socket...)."""
        return self._extra.get(name, default)

    is_closing = _abstract('is_closing', "True once close() was called.")
    close = _abstract('close', "Close; buffered data is still flushed.")
    set_protocol = _abstract('set_protocol', "Switch to another protocol.")
    get_protocol = _abstract('get_protocol', "The current protocol.")


class ReadTransport(BaseTransport):
    """Receiving side of a transport."""

    __slots__ = ()

    is_reading = _abstract('is_reading', "True unless reading is paused.")
    pause_reading = _abstract('pause_reading',
                              "Stop delivering data to the protocol.")
    resume_reading = _abstract('resume_reading',
                               "Deliver data to the protocol again.")


class WriteTransport(BaseTransport):
    """Sending side of a transport."""

    __slots__ = ()

    set_write_buffer_limits = _abstract(
        'set_write_buffer_limits',
        "Set the high/low water marks driving pause/resume_writing().")
    get_write_buffer_size = _abstract('get_write_buffer_size',
                                      "Bytes waiting in the write buffer.")
    get_write_buffer_limits = _abstract('get_write_buffer_limits',
                                        "The (low, high) water marks.")
    write = _abstract('write', "Queue bytes for sending (never blocks).")
    write_eof = _abstract('write_eof',
                          "Half-close after the buffered data is sent.")
    can_write_eof = _abstract('can_write_eof',
                              "True if write_eof() is supported.")
    abort = _abstract('abort', "Close at once, dropping buffered data.")

    def writelines(self, list_of_data):
        """Write an iterable of bytes-like objects."""
        self.write(b''.join(list_of_data))


class Transport(ReadTransport, WriteTransport):
    """Bidirectional stream transport."""

    __slots__ = ()


class DatagramTransport(BaseTransport):
    """Datagram transport."""

    __slots__ = ()

    sendto = _abstract('sendto', "Send a datagram to addr (or the peer).")
    abort = _abstract('abort', "Close at once, dropping buffered data.")


class SubprocessTransport(BaseTransport):
    """Transport controlling a child process."""

    __slots__ = ()

    get_pid = _abstract('get_pid', "The child's process id.")
    get_returncode = _abstract('get_returncode',
                               "The exit status, None while running.")
    get_pipe_transport = _abstract('get_pipe_transport',
                                   "Transport of the child's pipe fd.")
    send_signal = _abstract('send_signal', "Send a signal to the child.")
    terminate = _abstract('terminate', "Ask the child to terminate.")
    kill = _abstract('kill', "Kill the child.")


class _FlowControlMixin(Transport):
    """Write flow control driven by get_write_buffer_size().

    Users call _maybe_pause_protocol() after buffering and
    _maybe_resume_protocol() after flushing; self._protocol must be set.
    """

    __slots__ = ('_loop', '_protocol_paused', '_high_water', '_low_water')

    def __init__(self, extra=None, loop=None):
        super().__init__(extra)
        assert loop is not None
        self._loop = loop
        self._protocol_paused = False
        self._set_write_buffer_limits()

    def _notify_protocol(self, name):
        try:
            getattr(self._protocol, name)()
        except (SystemExit, KeyboardInterrupt):
            raise
        except BaseException as exc:
            self._loop.call_exception_handler({
                'message': f'protocol.{name}() failed',
                'exception': exc,
                'transport': self,
                'protocol': self._protocol,
            })

    def _maybe_pause_protocol(self):
        if self.get_write_buffer_size() > self._high_water and \
                not self._protocol_paused:
            self._protocol_paused = True
            self._notify_protocol('pause_writing')

    def _maybe_resume_protocol(self):
        if self._protocol_paused and \
                self.get_write_buffer_size() <= self._low_water:
            self._protocol_paused = False
            self._notify_protocol('resume_writing')

    def get_write_buffer_limits(self):
        return (self._low_water, self._high_water)

    def _set_write_buffer_limits(self, high=None, low=None):
        if high is None:
            high = 64 * 1024 if low is None else 4 * low
        if low is None:
            low = high // 4
        if not high >= low >= 0:
            raise ValueError(
                f'high ({high!r}) must be >= low ({low!r}) must be >= 0')
        self._high_water, self._low_water = high, low

    def set_write_buffer_limits(self, high=None, low=None):
        self._set_write_buffer_limits(high=high, low=low)
        self._maybe_pause_protocol()

    def get_write_buffer_size(self):
        raise NotImplementedError
