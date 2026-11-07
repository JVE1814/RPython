"""TLS on top of an asyncio transport.

SSLProtocol sits between a (plain) transport and the application protocol.
Encrypted bytes from the transport are fed into an ssl.MemoryBIO and
decrypted with an SSLObject; application writes go the other way.  The
application sees an _SSLProtocolTransport.

States of the TLS layer:

    UNWRAPPED -> DO_HANDSHAKE -> WRAPPED -> FLUSHING -> SHUTDOWN -> UNWRAPPED

(any state may fall back to UNWRAPPED when the connection is aborted).
"""

import collections
import enum
import warnings

try:
    import ssl
except ImportError:
    ssl = None

from . import constants
from . import exceptions
from . import protocols
from . import transports
from .log import logger

if ssl is not None:
    # "try again later" conditions of non-blocking TLS operations
    SSLAgainErrors = (ssl.SSLWantReadError, ssl.SSLSyscallError)


class SSLProtocolState(enum.Enum):
    UNWRAPPED = "UNWRAPPED"
    DO_HANDSHAKE = "DO_HANDSHAKE"
    WRAPPED = "WRAPPED"
    FLUSHING = "FLUSHING"
    SHUTDOWN = "SHUTDOWN"


class AppProtocolState(enum.Enum):
    # Application protocol callbacks: connection_made, then optionally
    # eof_received, then connection_lost.
    STATE_INIT = "STATE_INIT"
    STATE_CON_MADE = "STATE_CON_MADE"
    STATE_EOF = "STATE_EOF"
    STATE_CON_LOST = "STATE_CON_LOST"


_ALLOWED_TRANSITIONS = {
    (SSLProtocolState.UNWRAPPED, SSLProtocolState.DO_HANDSHAKE),
    (SSLProtocolState.DO_HANDSHAKE, SSLProtocolState.WRAPPED),
    (SSLProtocolState.WRAPPED, SSLProtocolState.FLUSHING),
    (SSLProtocolState.FLUSHING, SSLProtocolState.SHUTDOWN),
}


def _create_transport_context(server_side, server_hostname):
    if server_side:
        raise ValueError('Server side SSL needs a valid SSLContext')
    # Client side may pass ssl=True to use a default context.
    context = ssl.create_default_context()
    if not server_hostname:
        context.check_hostname = False
    return context


def add_flowcontrol_defaults(high, low, kb):
    """Fill in missing high/low water marks (high defaults to *kb* KiB)."""
    if high is None:
        high = kb * 1024 if low is None else 4 * low
    if low is None:
        low = high // 4
    if not high >= low >= 0:
        raise ValueError(f'high ({high!r}) must be >= low ({low!r}) must be '
                         f'>= 0')
    return high, low


class _SSLProtocolTransport(transports._FlowControlMixin,
                            transports.Transport):
    """The transport the application protocol of an SSLProtocol uses."""

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
        """Close the connection after a TLS shutdown (buffered data is
        flushed first)."""
        if not self._closed:
            self._closed = True
            self._ssl_protocol._start_shutdown()
        else:
            self._ssl_protocol = None

    def __del__(self, _warnings=warnings):
        if not self._closed:
            self._closed = True
            _warnings.warn(f'unclosed transport {self!r}', ResourceWarning,
                           source=self)

    def is_reading(self):
        return not self._ssl_protocol._app_reading_paused

    def pause_reading(self):
        self._ssl_protocol._pause_reading()

    def resume_reading(self):
        self._ssl_protocol._resume_reading()

    def set_write_buffer_limits(self, high=None, low=None):
        self._ssl_protocol._set_write_buffer_limits(high, low)
        self._ssl_protocol._control_app_writing()

    def get_write_buffer_limits(self):
        return (self._ssl_protocol._outgoing_low_water,
                self._ssl_protocol._outgoing_high_water)

    def get_write_buffer_size(self):
        return self._ssl_protocol._get_write_buffer_size()

    def set_read_buffer_limits(self, high=None, low=None):
        self._ssl_protocol._set_read_buffer_limits(high, low)
        self._ssl_protocol._control_ssl_reading()

    def get_read_buffer_limits(self):
        return (self._ssl_protocol._incoming_low_water,
                self._ssl_protocol._incoming_high_water)

    def get_read_buffer_size(self):
        return self._ssl_protocol._get_read_buffer_size()

    @property
    def _protocol_paused(self):
        # Needed by sendfile's fallback implementation.
        return self._ssl_protocol._app_writing_paused

    def write(self, data):
        """Encrypt and send *data* (bytes-like)."""
        if not isinstance(data, (bytes, bytearray, memoryview)):
            raise TypeError(f'data: expecting a bytes-like instance, '
                            f'got {type(data).__name__}')
        if not data:
            return
        self._ssl_protocol._write_appdata((data,))

    def writelines(self, list_of_data):
        self._ssl_protocol._write_appdata(list_of_data)

    def write_eof(self):
        raise NotImplementedError(
            'SSL transport does not support write_eof()')

    def can_write_eof(self):
        return False

    def abort(self):
        """Close immediately, dropping buffered data."""
        self._force_close(None)

    def _force_close(self, exc):
        self._closed = True
        if self._ssl_protocol is not None:
            self._ssl_protocol._abort(exc)

    def _test__append_write_backlog(self, data):
        # used by tests only
        self._ssl_protocol._write_backlog.append(data)
        self._ssl_protocol._write_buffer_size += len(data)


class SSLProtocol(protocols.BufferedProtocol):
    """TLS layer between a transport and the application protocol."""

    max_size = 256 * 1024       # largest TLS read at once

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

        if ssl_handshake_timeout is None:
            ssl_handshake_timeout = constants.SSL_HANDSHAKE_TIMEOUT
        elif ssl_handshake_timeout <= 0:
            raise ValueError(f"ssl_handshake_timeout should be a positive "
                             f"number, got {ssl_handshake_timeout}")
        if ssl_shutdown_timeout is None:
            ssl_shutdown_timeout = constants.SSL_SHUTDOWN_TIMEOUT
        elif ssl_shutdown_timeout <= 0:
            raise ValueError(f"ssl_shutdown_timeout should be a positive "
                             f"number, got {ssl_shutdown_timeout}")

        if not sslcontext:
            sslcontext = _create_transport_context(server_side,
                                                   server_hostname)

        self._server_side = server_side
        if server_hostname and not server_side:
            self._server_hostname = server_hostname
        else:
            self._server_hostname = None
        if (not server_side and self._server_hostname is None and
                getattr(sslcontext, 'check_hostname', False)):
            raise ValueError('check_hostname requires server_hostname')
        self._sslcontext = sslcontext
        # SSL-specific extra info (more is added after the handshake)
        self._extra = dict(sslcontext=sslcontext)

        # application data not yet passed to the SSL object
        self._write_backlog = collections.deque()
        self._write_buffer_size = 0

        self._waiter = waiter
        self._loop = loop
        # contextvars context of the code that made the connection: the
        # callbacks this protocol schedules for the application run in it
        self._context = context
        self._set_app_protocol(app_protocol)
        self._app_transport = None
        self._app_transport_created = False
        self._transport = None
        self._ssl_handshake_timeout = ssl_handshake_timeout
        self._ssl_shutdown_timeout = ssl_shutdown_timeout

        self._incoming = ssl.MemoryBIO()
        self._outgoing = ssl.MemoryBIO()
        self._state = SSLProtocolState.UNWRAPPED
        self._conn_lost = 0
        if call_connection_made:
            self._app_state = AppProtocolState.STATE_INIT
        else:
            self._app_state = AppProtocolState.STATE_CON_MADE
        self._sslobj = self._sslcontext.wrap_bio(
            self._incoming, self._outgoing,
            server_side=self._server_side,
            server_hostname=self._server_hostname)

        # flow control, writing
        self._ssl_writing_paused = False
        self._app_writing_paused = False
        self._outgoing_high_water = 0
        self._outgoing_low_water = 0
        self._set_write_buffer_limits()

        # flow control, reading
        self._app_reading_paused = False
        self._ssl_reading_paused = False
        self._incoming_high_water = 0
        self._incoming_low_water = 0
        self._set_read_buffer_limits()
        self._eof_received = False

        self._get_app_transport()

    def _set_app_protocol(self, app_protocol):
        self._app_protocol = app_protocol
        if (hasattr(app_protocol, 'get_buffer') and
                isinstance(app_protocol, protocols.BufferedProtocol)):
            self._app_protocol_get_buffer = app_protocol.get_buffer
            self._app_protocol_buffer_updated = app_protocol.buffer_updated
            self._app_protocol_is_buffer = True
        else:
            self._app_protocol_is_buffer = False

    def _wakeup_waiter(self, exc=None):
        waiter = self._waiter
        if waiter is None:
            return
        self._waiter = None
        if not waiter.cancelled():
            if exc is not None:
                waiter.set_exception(exc)
            else:
                waiter.set_result(None)

    def _get_app_transport(self):
        if self._app_transport is None:
            if self._app_transport_created:
                raise RuntimeError('Creating _SSLProtocolTransport twice')
            self._app_transport = _SSLProtocolTransport(self._loop, self)
            self._app_transport_created = True
        return self._app_transport

    def _is_transport_closing(self):
        return self._transport is not None and self._transport.is_closing()

    # -- callbacks from the underlying transport

    def connection_made(self, transport):
        """Start the TLS handshake on the new connection."""
        self._transport = transport
        self._start_handshake()

    def connection_lost(self, exc):
        """The underlying connection is gone (*exc*: error or None)."""
        self._write_backlog.clear()
        self._outgoing.read()
        self._conn_lost += 1

        # Only tell the application if it knows about the connection.
        if self._app_transport is not None:
            self._app_transport._closed = True

        if self._state != SSLProtocolState.DO_HANDSHAKE:
            if self._app_state in (AppProtocolState.STATE_CON_MADE,
                                   AppProtocolState.STATE_EOF):
                self._app_state = AppProtocolState.STATE_CON_LOST
                self._loop.call_soon(self._app_protocol.connection_lost, exc,
                                     context=self._context)
        self._set_state(SSLProtocolState.UNWRAPPED)
        self._transport = None
        self._app_transport = None
        self._app_protocol = None
        self._wakeup_waiter(exc)

        if self._shutdown_timeout_handle:
            self._shutdown_timeout_handle.cancel()
            self._shutdown_timeout_handle = None
        if self._handshake_timeout_handle:
            self._handshake_timeout_handle.cancel()
            self._handshake_timeout_handle = None

    def get_buffer(self, n):
        want = n
        if want <= 0 or want > self.max_size:
            want = self.max_size
        if len(self._ssl_buffer) < want:
            self._ssl_buffer = bytearray(want)
            self._ssl_buffer_view = memoryview(self._ssl_buffer)
        return self._ssl_buffer_view

    def buffer_updated(self, nbytes):
        self._incoming.write(self._ssl_buffer_view[:nbytes])
        self._advance()

    def _advance(self):
        state = self._state
        if state == SSLProtocolState.DO_HANDSHAKE:
            self._do_handshake()
        elif state == SSLProtocolState.WRAPPED:
            self._do_read()
        elif state == SSLProtocolState.FLUSHING:
            self._do_flush()
        elif state == SSLProtocolState.SHUTDOWN:
            self._do_shutdown()

    def eof_received(self):
        """The peer closed its end of the plain connection."""
        self._eof_received = True
        try:
            if self._loop.get_debug():
                logger.debug("%r received EOF", self)

            if self._state == SSLProtocolState.DO_HANDSHAKE:
                self._on_handshake_complete(ConnectionResetError)
            elif self._state == SSLProtocolState.WRAPPED:
                self._set_state(SSLProtocolState.FLUSHING)
                if self._app_reading_paused:
                    return True
                self._do_flush()
            elif self._state == SSLProtocolState.FLUSHING:
                self._do_write()
                self._set_state(SSLProtocolState.SHUTDOWN)
                self._do_shutdown()
            elif self._state == SSLProtocolState.SHUTDOWN:
                self._do_shutdown()
        except Exception:
            self._transport.close()
            raise

    def _get_extra_info(self, name, default=None):
        if name in self._extra:
            return self._extra[name]
        if self._transport is not None:
            return self._transport.get_extra_info(name, default)
        return default

    def _set_state(self, new_state):
        if (new_state == SSLProtocolState.UNWRAPPED or
                (self._state, new_state) in _ALLOWED_TRANSITIONS):
            self._state = new_state
            return
        raise RuntimeError(
            f'cannot switch state from {self._state} to {new_state}')

    # -- handshake

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
            msg = (f"SSL handshake is taking longer than "
                   f"{self._ssl_handshake_timeout} seconds: "
                   f"aborting the connection")
            self._fatal_error(ConnectionAbortedError(msg))

    def _do_handshake(self):
        try:
            self._sslobj.do_handshake()
        except SSLAgainErrors:
            self._process_outgoing()
        except ssl.SSLError as exc:
            self._on_handshake_complete(exc)
        else:
            self._on_handshake_complete(None)

    def _on_handshake_complete(self, handshake_exc):
        if self._handshake_timeout_handle is not None:
            self._handshake_timeout_handle.cancel()
            self._handshake_timeout_handle = None

        sslobj = self._sslobj
        try:
            if handshake_exc is not None:
                raise handshake_exc
            self._set_state(SSLProtocolState.WRAPPED)
            peercert = sslobj.getpeercert()
        except Exception as exc:
            self._set_state(SSLProtocolState.UNWRAPPED)
            if ssl is not None and isinstance(exc, ssl.CertificateError):
                msg = 'SSL handshake failed on verifying the certificate'
            else:
                msg = 'SSL handshake failed'
            self._fatal_error(exc, msg)
            self._wakeup_waiter(exc)
            return

        if self._loop.get_debug():
            elapsed = self._loop.time() - self._handshake_start_time
            logger.debug("%r: SSL handshake took %.1f ms", self,
                         elapsed * 1e3)

        self._extra.update(peercert=peercert,
                           cipher=sslobj.cipher(),
                           compression=sslobj.compression(),
                           ssl_object=sslobj)
        if self._app_state == AppProtocolState.STATE_INIT:
            self._app_state = AppProtocolState.STATE_CON_MADE
            self._app_protocol.connection_made(self._get_app_transport())
        self._wakeup_waiter()
        self._do_read()

    # -- shutdown

    def _start_shutdown(self):
        if self._state in (SSLProtocolState.FLUSHING,
                           SSLProtocolState.SHUTDOWN,
                           SSLProtocolState.UNWRAPPED):
            return
        if self._app_transport is not None:
            self._app_transport._closed = True
        if self._state == SSLProtocolState.DO_HANDSHAKE:
            self._abort(None)
        else:
            self._set_state(SSLProtocolState.FLUSHING)
            self._shutdown_timeout_handle = self._loop.call_later(
                self._ssl_shutdown_timeout, self._check_shutdown_timeout)
            self._do_flush()

    def _check_shutdown_timeout(self):
        if self._state in (SSLProtocolState.FLUSHING,
                           SSLProtocolState.SHUTDOWN):
            self._transport._force_close(
                exceptions.TimeoutError('SSL shutdown timed out'))

    def _do_flush(self):
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
        pending = False
        if not shutdown_exc and self._eof_received and transport is not None:
            size = transport.get_write_buffer_size()
            pending = isinstance(size, int) and size > 0
        if pending:
            # (hang1) the peer still has to take what is queued (it may have
            # sent EOF and stopped reading): ssl_shutdown_timeout bounds that
            # too, instead of waiting for the transport to drain forever
            if self._shutdown_timeout_handle is None:
                self._shutdown_timeout_handle = self._loop.call_later(
                    self._ssl_shutdown_timeout, self._check_shutdown_timeout)
        elif self._shutdown_timeout_handle is not None:
            self._shutdown_timeout_handle.cancel()
            self._shutdown_timeout_handle = None
        if shutdown_exc:
            self._fatal_error(shutdown_exc)
        else:
            self._loop.call_soon(self._transport.close)

    def _abort(self, exc):
        self._set_state(SSLProtocolState.UNWRAPPED)
        if self._transport is not None:
            self._transport._force_close(exc)

    # -- writing

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
        backlog = self._write_backlog
        try:
            while backlog:
                data = backlog[0]
                written = self._sslobj.write(data)
                size = len(data)
                if written < size:
                    backlog[0] = data[written:]
                    self._write_buffer_size -= written
                else:
                    backlog.popleft()
                    self._write_buffer_size -= size
        except SSLAgainErrors:
            pass
        self._process_outgoing()

    def _process_outgoing(self):
        if not self._ssl_writing_paused:
            data = self._outgoing.read()
            if len(data):
                self._transport.write(data)
        self._control_app_writing()

    # -- reading

    def _do_read(self):
        if self._state not in (SSLProtocolState.WRAPPED,
                               SSLProtocolState.FLUSHING):
            return
        try:
            if not self._app_reading_paused:
                if self._app_protocol_is_buffer:
                    self._do_read_into_buffer()
                else:
                    self._do_read_copy()
                if self._write_backlog:
                    self._do_write()
                else:
                    self._process_outgoing()
            self._control_ssl_reading()
        except Exception as exc:
            self._fatal_error(exc, 'Fatal error on SSL protocol')

    def _do_read_into_buffer(self):
        filled = 0
        count = 1
        buf = self._app_protocol_get_buffer(self._get_read_buffer_size())
        wants = len(buf)
        try:
            count = self._sslobj.read(wants, buf)
            if count > 0:
                filled = count
                while filled < wants:
                    count = self._sslobj.read(wants - filled, buf[filled:])
                    if count > 0:
                        filled += count
                    else:
                        break
                else:
                    # the buffer is full: continue with a fresh one later
                    self._loop.call_soon(self._do_read)
        except SSLAgainErrors:
            pass
        if filled > 0:
            self._app_protocol_buffer_updated(filled)
        if not count:
            # a zero-length read is the TLS close_notify
            self._call_eof_received()
            self._start_shutdown()

    def _do_read_copy(self):
        chunks = []
        chunk = b'1'
        try:
            while True:
                chunk = self._sslobj.read(self.max_size)
                if not chunk:
                    break
                chunks.append(chunk)
        except SSLAgainErrors:
            pass
        if len(chunks) == 1:
            self._app_protocol.data_received(chunks[0])
        elif chunks:
            self._app_protocol.data_received(b''.join(chunks))
        if not chunk:
            self._call_eof_received()
            self._start_shutdown()

    def _call_eof_received(self):
        try:
            if self._app_state == AppProtocolState.STATE_CON_MADE:
                self._app_state = AppProtocolState.STATE_EOF
                keep_open = self._app_protocol.eof_received()
                if keep_open:
                    logger.warning('returning true from eof_received() '
                                   'has no effect when using ssl')
        except (KeyboardInterrupt, SystemExit):
            raise
        except BaseException as exc:
            self._fatal_error(exc, 'Error calling eof_received()')

    # -- flow control, writing (app -> TLS -> transport)

    def _control_app_writing(self):
        size = self._get_write_buffer_size()
        if size >= self._outgoing_high_water and not self._app_writing_paused:
            self._app_writing_paused = True
            self._call_app('pause_writing')
        elif size <= self._outgoing_low_water and self._app_writing_paused:
            self._app_writing_paused = False
            self._call_app('resume_writing')

    def _call_app(self, name):
        try:
            getattr(self._app_protocol, name)()
        except (KeyboardInterrupt, SystemExit):
            raise
        except BaseException as exc:
            self._loop.call_exception_handler({
                'message': f'protocol.{name}() failed',
                'exception': exc,
                'transport': self._app_transport,
                'protocol': self,
            })

    def _get_write_buffer_size(self):
        return self._outgoing.pending + self._write_buffer_size

    def _set_write_buffer_limits(self, high=None, low=None):
        high, low = add_flowcontrol_defaults(
            high, low, constants.FLOW_CONTROL_HIGH_WATER_SSL_WRITE)
        self._outgoing_high_water = high
        self._outgoing_low_water = low

    # the underlying transport asks us to stop/restart writing

    def pause_writing(self):
        assert not self._ssl_writing_paused
        self._ssl_writing_paused = True

    def resume_writing(self):
        assert self._ssl_writing_paused
        self._ssl_writing_paused = False
        self._process_outgoing()

    # -- flow control, reading (transport -> TLS -> app)

    def _pause_reading(self):
        self._app_reading_paused = True

    def _resume_reading(self):
        if self._app_reading_paused:
            self._app_reading_paused = False
            self._loop.call_soon(self._advance_after_resume)

    def _advance_after_resume(self):
        if self._state == SSLProtocolState.WRAPPED:
            self._do_read()
        elif self._state == SSLProtocolState.FLUSHING:
            self._do_flush()
        elif self._state == SSLProtocolState.SHUTDOWN:
            self._do_shutdown()

    def _control_ssl_reading(self):
        size = self._get_read_buffer_size()
        if size >= self._incoming_high_water and not self._ssl_reading_paused:
            self._ssl_reading_paused = True
            self._transport.pause_reading()
        elif size <= self._incoming_low_water and self._ssl_reading_paused:
            self._ssl_reading_paused = False
            self._transport.resume_reading()

    def _set_read_buffer_limits(self, high=None, low=None):
        high, low = add_flowcontrol_defaults(
            high, low, constants.FLOW_CONTROL_HIGH_WATER_SSL_READ)
        self._incoming_high_water = high
        self._incoming_low_water = low

    def _get_read_buffer_size(self):
        return self._incoming.pending

    # -- errors

    def _fatal_error(self, exc, message='Fatal error on transport'):
        if self._transport:
            self._transport._force_close(exc)

        if isinstance(exc, OSError):
            if self._loop.get_debug():
                logger.debug("%r: %s", self, message, exc_info=True)
        elif not isinstance(exc, exceptions.CancelledError):
            self._loop.call_exception_handler({
                'message': message,
                'exception': exc,
                'transport': self._transport,
                'protocol': self,
            })
