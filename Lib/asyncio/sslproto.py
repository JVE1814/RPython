"""TLS for asyncio stream transports.

SSLProtocol is installed as the protocol of a plain (socket) transport.
Ciphertext received from that transport goes into an incoming
ssl.MemoryBIO; an ssl.SSLObject turns it into plaintext for the
application protocol.  The application writes through an
_SSLProtocolTransport; its data is queued, encrypted into the outgoing
MemoryBIO and handed to the plain transport.

The TLS layer moves through these states:

    UNWRAPPED --connection_made--> DO_HANDSHAKE --success--> WRAPPED
    WRAPPED --close() / close_notify / EOF--> FLUSHING --> SHUTDOWN
    SHUTDOWN --unwrap done--> (plain transport closed)

Aborts and lost connections go straight back to UNWRAPPED.  Separately,
the application protocol sees connection_made, at most one
eof_received, then connection_lost (AppProtocolState).
"""

import collections
import enum
import warnings

try:
    import ssl
except ImportError:  # interpreter built without TLS
    ssl = None

from . import constants
from . import exceptions
from . import protocols
from . import transports
from .log import logger

if ssl is not None:
    # Errors meaning "nothing more can be done until the peer sends more".
    SSLAgainErrors = (ssl.SSLWantReadError, ssl.SSLSyscallError)


class SSLProtocolState(enum.Enum):
    UNWRAPPED = "UNWRAPPED"
    DO_HANDSHAKE = "DO_HANDSHAKE"
    WRAPPED = "WRAPPED"
    FLUSHING = "FLUSHING"
    SHUTDOWN = "SHUTDOWN"


class AppProtocolState(enum.Enum):
    STATE_INIT = "STATE_INIT"            # connection_made() not called yet
    STATE_CON_MADE = "STATE_CON_MADE"
    STATE_EOF = "STATE_EOF"              # eof_received() was called
    STATE_CON_LOST = "STATE_CON_LOST"


_U = SSLProtocolState

# The only forward moves; anything may drop back to UNWRAPPED.
_NEXT_STATES = {
    _U.UNWRAPPED: _U.DO_HANDSHAKE,
    _U.DO_HANDSHAKE: _U.WRAPPED,
    _U.WRAPPED: _U.FLUSHING,
    _U.FLUSHING: _U.SHUTDOWN,
}


def _create_transport_context(server_side, server_hostname):
    """The context used for ``ssl=True`` (client side only)."""
    if server_side:
        raise ValueError('Server side SSL needs a valid SSLContext')
    context = ssl.create_default_context()
    if not server_hostname:
        context.check_hostname = False
    return context


def add_flowcontrol_defaults(high, low, kb):
    """Complete a (high, low) water-mark pair; *kb* KiB is the default
    high mark, and a missing mark is derived from the other one."""
    if high is None:
        high = kb * 1024 if low is None else 4 * low
    if low is None:
        low = high // 4
    if not high >= low >= 0:
        raise ValueError(
            f'high ({high!r}) must be >= low ({low!r}) must be >= 0')
    return high, low


def _checked_timeout(value, default, what):
    if value is None:
        return default
    if value <= 0:
        raise ValueError(f"{what} should be a positive number, got {value}")
    return value


class _SSLProtocolTransport(transports._FlowControlMixin,
                            transports.Transport):
    """What the application protocol of a TLS connection writes to."""

    _start_tls_compatible = True
    _sendfile_compatible = constants._SendfileMode.FALLBACK

    def __init__(self, loop, ssl_protocol):
        self._loop = loop
        self._ssl_protocol = ssl_protocol
        self._closed = False

    def get_extra_info(self, name, default=None):
        return self._ssl_protocol._get_extra_info(name, default)

    def set_protocol(self, protocol):
        self._ssl_protocol._set_app_protocol(protocol)

    def get_protocol(self):
        return self._ssl_protocol._app_protocol

    def is_closing(self):
        return self._closed or self._ssl_protocol._is_transport_closing()

    def close(self):
        """Shut TLS down cleanly (queued data is sent first), then close
        the underlying transport."""
        if self._closed:
            # second call: just let go of the protocol (reference cycle)
            self._ssl_protocol = None
            return
        self._closed = True
        self._ssl_protocol._start_shutdown()

    def __del__(self, _warnings=warnings):
        if not self._closed:
            self._closed = True
            _warnings.warn(f"unclosed transport {self!r}", ResourceWarning,
                           source=self)

    def is_reading(self):
        return not self._ssl_protocol._app_reading_paused

    def pause_reading(self):
        self._ssl_protocol._pause_reading()

    def resume_reading(self):
        self._ssl_protocol._resume_reading()

    def set_write_buffer_limits(self, high=None, low=None):
        tls = self._ssl_protocol
        tls._set_write_buffer_limits(high, low)
        tls._control_app_writing()

    def get_write_buffer_limits(self):
        tls = self._ssl_protocol
        return tls._outgoing_low_water, tls._outgoing_high_water

    def get_write_buffer_size(self):
        return self._ssl_protocol._get_write_buffer_size()

    def set_read_buffer_limits(self, high=None, low=None):
        tls = self._ssl_protocol
        tls._set_read_buffer_limits(high, low)
        tls._control_ssl_reading()

    def get_read_buffer_limits(self):
        tls = self._ssl_protocol
        return tls._incoming_low_water, tls._incoming_high_water

    def get_read_buffer_size(self):
        return self._ssl_protocol._get_read_buffer_size()

    @property
    def _protocol_paused(self):
        # read by the sendfile() fallback
        return self._ssl_protocol._app_writing_paused

    def write(self, data):
        if not isinstance(data, (bytes, bytearray, memoryview)):
            raise TypeError(f"data: expecting a bytes-like instance, "
                            f"got {type(data).__name__}")
        if data:
            self._ssl_protocol._write_appdata((data,))

    def writelines(self, list_of_data):
        self._ssl_protocol._write_appdata(list_of_data)

    def write_eof(self):
        raise NotImplementedError("SSL transport does not support write_eof()")

    def can_write_eof(self):
        return False

    def abort(self):
        """Drop the connection at once, without a TLS shutdown."""
        self._force_close(None)

    def _force_close(self, exc):
        self._closed = True
        if self._ssl_protocol is not None:
            self._ssl_protocol._abort(exc)

    def _test__append_write_backlog(self, data):
        # lets tests fill the queue without going through write()
        self._ssl_protocol._write_backlog.append(data)
        self._ssl_protocol._write_buffer_size += len(data)


class SSLProtocol(protocols.BufferedProtocol):
    """Protocol of the plain transport carrying a TLS connection."""

    max_size = 256 * 1024   # bytes read from the transport / SSL object at once

    _handshake_start_time = None
    _handshake_timeout_handle = None
    _shutdown_timeout_handle = None

    def __init__(self, loop, app_protocol, sslcontext, waiter,
                 server_side=False, server_hostname=None,
                 call_connection_made=True,
                 ssl_handshake_timeout=None,
                 ssl_shutdown_timeout=None,
                 context=None):
        if ssl is None:
            raise RuntimeError("stdlib ssl module not available")

        self._ssl_buffer = bytearray(self.max_size)
        self._ssl_buffer_view = memoryview(self._ssl_buffer)

        self._ssl_handshake_timeout = _checked_timeout(
            ssl_handshake_timeout, constants.SSL_HANDSHAKE_TIMEOUT,
            'ssl_handshake_timeout')
        self._ssl_shutdown_timeout = _checked_timeout(
            ssl_shutdown_timeout, constants.SSL_SHUTDOWN_TIMEOUT,
            'ssl_shutdown_timeout')

        if not sslcontext:
            sslcontext = _create_transport_context(server_side,
                                                   server_hostname)
        self._server_side = server_side
        # an empty name means "no name" (and no hostname check)
        self._server_hostname = (server_hostname
                                 if server_hostname and not server_side
                                 else None)
        if (not server_side and self._server_hostname is None
                and getattr(sslcontext, 'check_hostname', False)):
            raise ValueError('check_hostname requires server_hostname')
        self._sslcontext = sslcontext
        self._extra = {'sslcontext': sslcontext}

        self._loop = loop
        self._waiter = waiter
        # Context of whoever made the connection: application callbacks
        # scheduled from here run in it.
        self._context = context
        self._set_app_protocol(app_protocol)
        self._app_transport = None
        self._app_transport_created = False
        self._transport = None
        self._conn_lost = 0
        self._eof_received = False

        self._state = SSLProtocolState.UNWRAPPED
        self._app_state = (AppProtocolState.STATE_INIT if call_connection_made
                           else AppProtocolState.STATE_CON_MADE)

        self._incoming = ssl.MemoryBIO()
        self._outgoing = ssl.MemoryBIO()
        self._sslobj = sslcontext.wrap_bio(
            self._incoming, self._outgoing,
            server_side=server_side, server_hostname=self._server_hostname)

        # application data not yet given to the SSL object
        self._write_backlog = collections.deque()
        self._write_buffer_size = 0

        # Four independent pause flags: the application asked us to stop
        # delivering (app_reading), we asked the transport to stop reading
        # (ssl_reading), the transport asked us to stop writing
        # (ssl_writing), we asked the application to stop writing
        # (app_writing).
        self._app_reading_paused = False
        self._ssl_reading_paused = False
        self._ssl_writing_paused = False
        self._app_writing_paused = False
        self._set_read_buffer_limits()
        self._set_write_buffer_limits()

        self._get_app_transport()

    # -- plumbing towards the application ----------------------------------

    def _set_app_protocol(self, app_protocol):
        self._app_protocol = app_protocol
        get_buffer = getattr(app_protocol, 'get_buffer', None)
        buffer_updated = getattr(app_protocol, 'buffer_updated', None)
        self._app_protocol_is_buffer = (get_buffer is not None
                                        and buffer_updated is not None)
        if self._app_protocol_is_buffer:
            self._app_protocol_get_buffer = get_buffer
            self._app_protocol_buffer_updated = buffer_updated

    def _get_app_transport(self):
        if self._app_transport is None:
            if self._app_transport_created:
                raise RuntimeError('Creating _SSLProtocolTransport twice')
            self._app_transport = _SSLProtocolTransport(self._loop, self)
            self._app_transport_created = True
        return self._app_transport

    def _is_transport_closing(self):
        return self._transport is not None and self._transport.is_closing()

    def _get_extra_info(self, name, default=None):
        if name in self._extra:
            return self._extra[name]
        if self._transport is not None:
            return self._transport.get_extra_info(name, default)
        return default

    def _wakeup_waiter(self, exc=None):
        waiter, self._waiter = self._waiter, None
        if waiter is None or waiter.cancelled():
            return
        if exc is None:
            waiter.set_result(None)
        else:
            waiter.set_exception(exc)

    def _schedule_app(self, callback, *args):
        self._loop.call_soon(callback, *args, context=self._context)

    # -- callbacks from the plain transport --------------------------------

    def connection_made(self, transport):
        self._transport = transport
        self._start_handshake()

    def connection_lost(self, exc):
        self._write_backlog.clear()
        self._outgoing.read()          # discard unsent ciphertext
        self._conn_lost += 1

        if self._app_transport is not None:
            self._app_transport._closed = True
        # The application only hears about it if it heard of the
        # connection in the first place (not during the handshake).
        if (self._state != SSLProtocolState.DO_HANDSHAKE
                and self._app_state in (AppProtocolState.STATE_CON_MADE,
                                        AppProtocolState.STATE_EOF)):
            self._app_state = AppProtocolState.STATE_CON_LOST
            self._schedule_app(self._app_protocol.connection_lost, exc)

        self._set_state(SSLProtocolState.UNWRAPPED)
        self._transport = None
        self._app_transport = None
        self._app_protocol = None
        self._wakeup_waiter(exc)
        self._cancel_timers()

    def get_buffer(self, n):
        size = n if 0 < n <= self.max_size else self.max_size
        if len(self._ssl_buffer) < size:
            self._ssl_buffer = bytearray(size)
            self._ssl_buffer_view = memoryview(self._ssl_buffer)
        return self._ssl_buffer_view

    def buffer_updated(self, nbytes):
        self._incoming.write(self._ssl_buffer_view[:nbytes])
        self._advance()

    def eof_received(self):
        """The peer closed its side of the plain connection."""
        self._eof_received = True
        try:
            if self._loop.get_debug():
                logger.debug("%r received EOF", self)
            state = self._state
            if state == SSLProtocolState.DO_HANDSHAKE:
                self._on_handshake_complete(ConnectionResetError())
            elif state == SSLProtocolState.WRAPPED:
                self._set_state(SSLProtocolState.FLUSHING)
                if self._app_reading_paused:
                    # keep the transport open until the app reads again
                    return True
                self._do_flush()
            elif state == SSLProtocolState.FLUSHING:
                self._do_write()
                self._set_state(SSLProtocolState.SHUTDOWN)
                self._do_shutdown()
            elif state == SSLProtocolState.SHUTDOWN:
                self._do_shutdown()
        except Exception:
            self._transport.close()
            raise

    def pause_writing(self):
        """The plain transport's buffer is full."""
        self._ssl_writing_paused = True

    def resume_writing(self):
        self._ssl_writing_paused = False
        self._process_outgoing()

    # -- state machine --------------------------------------------------------

    def _set_state(self, new_state):
        if (new_state != SSLProtocolState.UNWRAPPED
                and _NEXT_STATES.get(self._state) != new_state):
            raise RuntimeError(
                f'cannot switch state from {self._state} to {new_state}')
        self._state = new_state

    def _advance(self):
        """Do whatever the current state allows with the bytes received."""
        step = {
            SSLProtocolState.DO_HANDSHAKE: self._do_handshake,
            SSLProtocolState.WRAPPED: self._do_read,
            SSLProtocolState.FLUSHING: self._do_flush,
            SSLProtocolState.SHUTDOWN: self._do_shutdown,
        }.get(self._state)
        if step is not None:
            step()

    def _cancel_timers(self):
        for name in ('_handshake_timeout_handle', '_shutdown_timeout_handle'):
            handle = getattr(self, name)
            if handle is not None:
                handle.cancel()
                setattr(self, name, None)

    def _abort(self, exc):
        self._set_state(SSLProtocolState.UNWRAPPED)
        if self._transport is not None:
            self._transport._force_close(exc)

    # -- handshake --------------------------------------------------------------

    def _start_handshake(self):
        if self._loop.get_debug():
            logger.debug("%r starts SSL handshake", self)
            self._handshake_start_time = self._loop.time()
        else:
            self._handshake_start_time = None
        self._set_state(SSLProtocolState.DO_HANDSHAKE)
        self._handshake_timeout_handle = self._loop.call_later(
            self._ssl_handshake_timeout, self._check_handshake_timeout)
        self._do_handshake()

    def _check_handshake_timeout(self):
        if self._state == SSLProtocolState.DO_HANDSHAKE:
            self._fatal_error(ConnectionAbortedError(
                f"SSL handshake is taking longer than "
                f"{self._ssl_handshake_timeout} seconds: "
                f"aborting the connection"))

    def _do_handshake(self):
        try:
            self._sslobj.do_handshake()
        except SSLAgainErrors:
            self._process_outgoing()
        except ssl.SSLError as exc:
            self._on_handshake_complete(exc)
        else:
            self._on_handshake_complete(None)

    def _on_handshake_complete(self, failure):
        handle, self._handshake_timeout_handle = (
            self._handshake_timeout_handle, None)
        if handle is not None:
            handle.cancel()

        sslobj = self._sslobj
        if failure is None:
            try:
                self._set_state(SSLProtocolState.WRAPPED)
                peercert = sslobj.getpeercert()
            except Exception as exc:
                failure = exc
        if failure is not None:
            self._handshake_failed(failure)
            # This frame ends up in the traceback of the failure: keep it
            # from referring to the exception itself.
            failure = None
            return

        if self._handshake_start_time is not None:
            elapsed = self._loop.time() - self._handshake_start_time
            logger.debug("%r: SSL handshake took %.1f ms", self, elapsed * 1e3)

        self._extra.update(peercert=peercert,
                           cipher=sslobj.cipher(),
                           compression=sslobj.compression(),
                           ssl_object=sslobj)
        if self._app_state == AppProtocolState.STATE_INIT:
            self._app_state = AppProtocolState.STATE_CON_MADE
            self._app_protocol.connection_made(self._get_app_transport())
        self._wakeup_waiter()
        self._do_read()

    def _handshake_failed(self, exc):
        self._set_state(SSLProtocolState.UNWRAPPED)
        if isinstance(exc, ssl.CertificateError):
            message = 'SSL handshake failed on verifying the certificate'
        else:
            message = 'SSL handshake failed'
        self._fatal_error(exc, message)
        self._wakeup_waiter(exc)

    # -- shutdown ---------------------------------------------------------------

    def _start_shutdown(self):
        if self._state in (SSLProtocolState.FLUSHING,
                           SSLProtocolState.SHUTDOWN,
                           SSLProtocolState.UNWRAPPED):
            return
        if self._app_transport is not None:
            self._app_transport._closed = True
        if self._state == SSLProtocolState.DO_HANDSHAKE:
            # nothing to flush: drop the connection
            self._abort(None)
            return
        self._set_state(SSLProtocolState.FLUSHING)
        self._arm_shutdown_timer()
        self._do_flush()

    def _arm_shutdown_timer(self):
        if self._shutdown_timeout_handle is None:
            self._shutdown_timeout_handle = self._loop.call_later(
                self._ssl_shutdown_timeout, self._check_shutdown_timeout)

    def _check_shutdown_timeout(self):
        if self._state in (SSLProtocolState.FLUSHING,
                           SSLProtocolState.SHUTDOWN):
            self._transport._force_close(
                exceptions.TimeoutError('SSL shutdown timed out'))

    def _do_flush(self):
        # deliver what is still readable, then send close_notify
        self._do_read()
        self._set_state(SSLProtocolState.SHUTDOWN)
        self._do_shutdown()

    def _do_shutdown(self):
        try:
            if not self._eof_received:
                self._sslobj.unwrap()
        except SSLAgainErrors:
            self._process_outgoing()
        except ssl.SSLError as exc:
            self._on_shutdown_complete(exc)
        else:
            self._process_outgoing()
            self._call_eof_received()
            self._on_shutdown_complete(None)

    def _on_shutdown_complete(self, shutdown_exc):
        transport = self._transport
        backlog = 0
        if shutdown_exc is None and self._eof_received and transport is not None:
            size = transport.get_write_buffer_size()
            backlog = size if isinstance(size, int) else 0
        if backlog > 0:
            # (hang1) A peer that sent EOF may never read what is still
            # queued: the shutdown timeout also bounds that wait.
            self._arm_shutdown_timer()
        else:
            handle, self._shutdown_timeout_handle = (
                self._shutdown_timeout_handle, None)
            if handle is not None:
                handle.cancel()
        if shutdown_exc is not None:
            self._fatal_error(shutdown_exc)
        else:
            self._loop.call_soon(transport.close)

    # -- writing (application -> SSL object -> transport) -----------------------

    def _write_appdata(self, list_of_data):
        if self._state in (SSLProtocolState.FLUSHING,
                           SSLProtocolState.SHUTDOWN,
                           SSLProtocolState.UNWRAPPED):
            if self._conn_lost >= constants.LOG_THRESHOLD_FOR_CONNLOST_WRITES:
                logger.warning('SSL connection is closed')
            self._conn_lost += 1
            return
        for data in list_of_data:
            self._write_backlog.append(data)
            self._write_buffer_size += len(data)
        try:
            if self._state == SSLProtocolState.WRAPPED:
                self._do_write()
        except Exception as exc:
            self._fatal_error(exc, 'Fatal error on SSL protocol')

    def _do_write(self):
        """Encrypt as much of the backlog as the SSL object accepts."""
        backlog = self._write_backlog
        try:
            while backlog:
                chunk = backlog[0]
                taken = self._sslobj.write(chunk)
                self._write_buffer_size -= taken
                if taken < len(chunk):
                    backlog[0] = chunk[taken:]
                else:
                    backlog.popleft()
        except SSLAgainErrors:
            pass
        self._process_outgoing()

    def _process_outgoing(self):
        """Hand the pending ciphertext to the transport (unless it asked us
        to wait) and update the application's write flow control."""
        if not self._ssl_writing_paused:
            ciphertext = self._outgoing.read()
            if ciphertext:
                self._transport.write(ciphertext)
        self._control_app_writing()

    def _get_write_buffer_size(self):
        return self._outgoing.pending + self._write_buffer_size

    def _set_write_buffer_limits(self, high=None, low=None):
        self._outgoing_high_water, self._outgoing_low_water = (
            add_flowcontrol_defaults(
                high, low, constants.FLOW_CONTROL_HIGH_WATER_SSL_WRITE))

    def _control_app_writing(self):
        size = self._get_write_buffer_size()
        if not self._app_writing_paused and size >= self._outgoing_high_water:
            self._app_writing_paused = True
            self._notify_app_writing('pause_writing')
        elif self._app_writing_paused and size <= self._outgoing_low_water:
            self._app_writing_paused = False
            self._notify_app_writing('resume_writing')

    def _notify_app_writing(self, method):
        try:
            getattr(self._app_protocol, method)()
        except (KeyboardInterrupt, SystemExit):
            raise
        except BaseException as exc:
            self._loop.call_exception_handler({
                'message': f'protocol.{method}() failed',
                'exception': exc,
                'transport': self._app_transport,
                'protocol': self,
            })

    # -- reading (transport -> SSL object -> application) -----------------------

    def _do_read(self):
        if self._state not in (SSLProtocolState.WRAPPED,
                               SSLProtocolState.FLUSHING):
            return
        try:
            if not self._app_reading_paused:
                if self._app_protocol_is_buffer:
                    more = self._read_into_app_buffer()
                else:
                    more = self._read_and_deliver()
                if not more:
                    # close_notify from the peer
                    self._call_eof_received()
                    self._start_shutdown()
                # reading may have produced handshake/renegotiation data
                if self._write_backlog:
                    self._do_write()
                else:
                    self._process_outgoing()
            self._control_ssl_reading()
        except Exception as exc:
            self._fatal_error(exc, 'Fatal error on SSL protocol')

    def _read_into_app_buffer(self):
        """Decrypt straight into the buffer a BufferedProtocol hands out.
        Returns False once the peer's close_notify was read."""
        buf = memoryview(
            self._app_protocol_get_buffer(self._get_read_buffer_size()))
        room = len(buf)
        filled = 0
        alive = True
        try:
            while filled < room:
                got = self._sslobj.read(room - filled, buf[filled:])
                if not got:
                    alive = False
                    break
                filled += got
            else:
                # buffer full: more plaintext may be waiting
                self._loop.call_soon(self._do_read)
        except SSLAgainErrors:
            pass
        if filled:
            self._app_protocol_buffer_updated(filled)
        return alive

    def _read_and_deliver(self):
        """Decrypt everything available and pass it to data_received() in
        one call.  Returns False once the peer's close_notify was read."""
        pieces = []
        alive = True
        try:
            while True:
                piece = self._sslobj.read(self.max_size)
                if not piece:
                    alive = False
                    break
                pieces.append(piece)
        except SSLAgainErrors:
            pass
        if pieces:
            self._app_protocol.data_received(
                pieces[0] if len(pieces) == 1 else b''.join(pieces))
        return alive

    def _call_eof_received(self):
        try:
            if self._app_state == AppProtocolState.STATE_CON_MADE:
                self._app_state = AppProtocolState.STATE_EOF
                if self._app_protocol.eof_received():
                    logger.warning('returning true from eof_received() '
                                   'has no effect when using ssl')
        except (KeyboardInterrupt, SystemExit):
            raise
        except BaseException as exc:
            self._fatal_error(exc, 'Error calling eof_received()')

    def _pause_reading(self):
        self._app_reading_paused = True

    def _resume_reading(self):
        if self._app_reading_paused:
            self._app_reading_paused = False
            # Not from inside the caller's stack (it may be the app's own
            # data_received()): deliver on the next loop iteration.
            self._loop.call_soon(self._advance_after_resume)

    def _advance_after_resume(self):
        if self._state in (SSLProtocolState.WRAPPED,
                           SSLProtocolState.FLUSHING,
                           SSLProtocolState.SHUTDOWN):
            self._advance()

    def _get_read_buffer_size(self):
        return self._incoming.pending

    def _set_read_buffer_limits(self, high=None, low=None):
        self._incoming_high_water, self._incoming_low_water = (
            add_flowcontrol_defaults(
                high, low, constants.FLOW_CONTROL_HIGH_WATER_SSL_READ))

    def _control_ssl_reading(self):
        size = self._get_read_buffer_size()
        if not self._ssl_reading_paused and size >= self._incoming_high_water:
            self._ssl_reading_paused = True
            self._transport.pause_reading()
        elif self._ssl_reading_paused and size <= self._incoming_low_water:
            self._ssl_reading_paused = False
            self._transport.resume_reading()

    # -- errors -----------------------------------------------------------------

    def _fatal_error(self, exc, message='Fatal error on transport'):
        if self._transport:
            self._transport._force_close(exc)
        if isinstance(exc, OSError):
            # connection problems are expected; only debug mode reports them
            if self._loop.get_debug():
                logger.debug("%r: %s", self, message, exc_info=True)
        elif not isinstance(exc, exceptions.CancelledError):
            self._loop.call_exception_handler({
                'message': message,
                'exception': exc,
                'transport': self._transport,
                'protocol': self,
            })
