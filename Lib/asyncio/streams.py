"""High-level stream API: StreamReader / StreamWriter over transports."""

__all__ = (
    'StreamReader', 'StreamWriter', 'StreamReaderProtocol',
    'open_connection', 'start_server')

import collections
import socket
import sys
import warnings
import weakref

if hasattr(socket, 'AF_UNIX'):
    __all__ += ('open_unix_connection', 'start_unix_server')

from . import coroutines
from . import events
from . import exceptions
from . import format_helpers
from . import protocols
from .log import logger
from .tasks import sleep


_DEFAULT_LIMIT = 2 ** 16  # 64 KiB


async def open_connection(host=None, port=None, *,
                          limit=_DEFAULT_LIMIT, **kwds):
    """Connect to (host, port); return a (StreamReader, StreamWriter) pair.

    Extra keyword arguments go to loop.create_connection().
    """
    loop = events.get_running_loop()
    reader = StreamReader(limit=limit, loop=loop)
    protocol = StreamReaderProtocol(reader, loop=loop)
    transport, _ = await loop.create_connection(
        lambda: protocol, host, port, **kwds)
    return reader, StreamWriter(transport, protocol, reader, loop)


async def start_server(client_connected_cb, host=None, port=None, *,
                       limit=_DEFAULT_LIMIT, **kwds):
    """Start a TCP server calling client_connected_cb(reader, writer) for
    every connection (a coroutine function becomes a task)."""
    loop = events.get_running_loop()

    def protocol_factory():
        reader = StreamReader(limit=limit, loop=loop)
        return StreamReaderProtocol(reader, client_connected_cb, loop=loop)

    return await loop.create_server(protocol_factory, host, port, **kwds)


if hasattr(socket, 'AF_UNIX'):

    async def open_unix_connection(path=None, *,
                                   limit=_DEFAULT_LIMIT, **kwds):
        """open_connection() for UNIX domain sockets."""
        loop = events.get_running_loop()
        reader = StreamReader(limit=limit, loop=loop)
        protocol = StreamReaderProtocol(reader, loop=loop)
        transport, _ = await loop.create_unix_connection(
            lambda: protocol, path, **kwds)
        return reader, StreamWriter(transport, protocol, reader, loop)

    async def start_unix_server(client_connected_cb, path=None, *,
                                limit=_DEFAULT_LIMIT, **kwds):
        """start_server() for UNIX domain sockets."""
        loop = events.get_running_loop()

        def protocol_factory():
            reader = StreamReader(limit=limit, loop=loop)
            return StreamReaderProtocol(reader, client_connected_cb,
                                        loop=loop)

        return await loop.create_unix_server(protocol_factory, path, **kwds)


class FlowControlMixin(protocols.Protocol):
    """Protocol mixin implementing drain() support: pause_writing() and
    resume_writing() control _drain_helper()."""

    def __init__(self, loop=None):
        self._loop = events.get_event_loop() if loop is None else loop
        self._paused = False
        self._drain_waiters = collections.deque()
        self._connection_lost = False

    def pause_writing(self):
        assert not self._paused
        self._paused = True
        if self._loop.get_debug():
            logger.debug("%r pauses writing", self)

    def resume_writing(self):
        assert self._paused
        self._paused = False
        if self._loop.get_debug():
            logger.debug("%r resumes writing", self)
        for waiter in self._drain_waiters:
            if not waiter.done():
                waiter.set_result(None)

    def connection_lost(self, exc):
        self._connection_lost = True
        if not self._paused:
            return   # nobody can be waiting in drain()
        for waiter in self._drain_waiters:
            if waiter.done():
                continue
            if exc is None:
                waiter.set_result(None)
            else:
                waiter.set_exception(exc)

    async def _drain_helper(self):
        if self._connection_lost:
            raise ConnectionResetError('Connection lost')
        if not self._paused:
            return
        waiter = self._loop.create_future()
        self._drain_waiters.append(waiter)
        try:
            await waiter
        finally:
            self._drain_waiters.remove(waiter)

    def _get_close_waiter(self, stream):
        raise NotImplementedError


class StreamReaderProtocol(FlowControlMixin, protocols.Protocol):
    """Adapter feeding a transport's data into a StreamReader."""

    _source_traceback = None

    def __init__(self, stream_reader, client_connected_cb=None, loop=None):
        super().__init__(loop=loop)
        if stream_reader is not None:
            # weak: an abandoned reader must be collectable
            self._stream_reader_wr = weakref.ref(stream_reader)
            self._source_traceback = stream_reader._source_traceback
        else:
            self._stream_reader_wr = None
        if client_connected_cb is not None:
            # server side: keep the reader alive until a client connects
            self._strong_reader = stream_reader
        self._reject_connection = False
        self._task = None
        self._transport = None
        self._client_connected_cb = client_connected_cb
        self._over_ssl = False
        self._closed = self._loop.create_future()

    @property
    def _stream_reader(self):
        if self._stream_reader_wr is None:
            return None
        return self._stream_reader_wr()

    def _replace_transport(self, transport):
        self._transport = transport
        self._over_ssl = transport.get_extra_info('sslcontext') is not None
        reader = self._stream_reader
        if reader is not None:
            reader._transport = transport

    def connection_made(self, transport):
        if self._reject_connection:
            report = {'message': ('An open stream was garbage collected prior '
                                  'to establishing network connection; call '
                                  '"stream.close()" explicitly.')}
            if self._source_traceback:
                report['source_traceback'] = self._source_traceback
            self._loop.call_exception_handler(report)
            transport.abort()
            return
        self._transport = transport
        reader = self._stream_reader
        if reader is not None:
            reader.set_transport(transport)
        self._over_ssl = transport.get_extra_info('sslcontext') is not None
        if self._client_connected_cb is None:
            return
        writer = StreamWriter(transport, self, reader, self._loop)
        try:
            outcome = self._client_connected_cb(reader, writer)
        except (SystemExit, KeyboardInterrupt):
            raise
        except BaseException as exc:
            # a plain callback failing is reported like a failing coroutine
            self._loop.call_exception_handler({
                'message': 'Unhandled exception in client_connected_cb',
                'exception': exc,
                'transport': transport,
            })
            transport.close()
            self._strong_reader = None
            return
        if coroutines.iscoroutine(outcome):
            def on_handler_done(task):
                if task.cancelled():
                    transport.close()
                    return
                exc = task.exception()
                if exc is not None:
                    self._loop.call_exception_handler({
                        'message': 'Unhandled exception in client_connected_cb',
                        'exception': exc,
                        'transport': transport,
                    })
                    transport.close()

            self._task = self._loop.create_task(outcome)
            self._task.add_done_callback(on_handler_done)
        self._strong_reader = None

    def connection_lost(self, exc):
        reader = self._stream_reader
        if reader is not None:
            if exc is None:
                reader.feed_eof()
            else:
                reader.set_exception(exc)
        if not self._closed.done():
            if exc is None:
                self._closed.set_result(None)
            else:
                self._closed.set_exception(exc)
        super().connection_lost(exc)
        self._stream_reader_wr = None
        self._stream_writer = None
        self._task = None
        self._transport = None

    def data_received(self, data):
        reader = self._stream_reader
        if reader is not None:
            reader.feed_data(data)

    def eof_received(self):
        reader = self._stream_reader
        if reader is not None:
            reader.feed_eof()
        # TLS cannot be half-closed: let the transport close
        return not self._over_ssl

    def _get_close_waiter(self, stream):
        return self._closed

    def __del__(self):
        closed = getattr(self, '_closed', None)
        if closed is not None and closed.done() and not closed.cancelled():
            closed.exception()   # retrieved: no "never retrieved" warning


class StreamWriter:
    """Writing side of a stream connection (wraps the transport)."""

    def __init__(self, transport, protocol, reader, loop):
        self._transport = transport
        self._protocol = protocol
        assert reader is None or isinstance(reader, StreamReader)
        self._reader = reader
        self._loop = loop
        self._complete_fut = self._loop.create_future()
        self._complete_fut.set_result(None)

    def __repr__(self):
        words = [type(self).__name__, f'transport={self._transport!r}']
        if self._reader is not None:
            words.append(f'reader={self._reader!r}')
        return '<{}>'.format(' '.join(words))

    @property
    def transport(self):
        return self._transport

    def write(self, data):
        self._transport.write(data)

    def writelines(self, data):
        self._transport.writelines(data)

    def write_eof(self):
        return self._transport.write_eof()

    def can_write_eof(self):
        return self._transport.can_write_eof()

    def close(self):
        return self._transport.close()

    def is_closing(self):
        return self._transport.is_closing()

    async def wait_closed(self):
        await self._protocol._get_close_waiter(self)

    def get_extra_info(self, name, default=None):
        return self._transport.get_extra_info(name, default)

    async def drain(self):
        """Wait until the write buffer is below the high-water mark.

            w.write(data)
            await w.drain()
        """
        if self._reader is not None:
            exc = self._reader.exception()
            if exc is not None:
                raise exc
        if self._transport.is_closing():
            # let connection_lost() run, so _drain_helper() reports it
            await sleep(0)
        await self._protocol._drain_helper()

    async def start_tls(self, sslcontext, *,
                        server_hostname=None,
                        ssl_handshake_timeout=None,
                        ssl_shutdown_timeout=None):
        """Upgrade the connection to TLS in place."""
        server_side = self._protocol._client_connected_cb is not None
        protocol = self._protocol
        await self.drain()
        # (hang1) bytes the reader already holds are the start of the TLS
        # stream (e.g. the peer's ClientHello): the TLS layer gets them, or
        # the handshake would wait for data that never comes again
        extra = {}
        reader = getattr(protocol, '_stream_reader', None)
        if reader is not None and reader._buffer:
            extra['_buffered'] = bytes(reader._buffer)
            reader._buffer.clear()
        new_transport = await self._loop.start_tls(
            self._transport, protocol, sslcontext,
            server_side=server_side, server_hostname=server_hostname,
            ssl_handshake_timeout=ssl_handshake_timeout,
            ssl_shutdown_timeout=ssl_shutdown_timeout, **extra)
        self._transport = new_transport
        protocol._replace_transport(new_transport)

    def __del__(self, warnings=warnings):
        if self._transport.is_closing():
            return
        if self._loop.is_closed():
            warnings.warn("loop is closed", ResourceWarning)
        else:
            self.close()
            warnings.warn(f"unclosed {self!r}", ResourceWarning)


class StreamReader:
    """Buffer of received bytes with coroutine read methods."""

    _source_traceback = None

    def __init__(self, limit=_DEFAULT_LIMIT, loop=None):
        # The limit bounds lines/chunks returned by readline()/readuntil()
        # and the buffer size before reading is paused (2 * limit).
        if limit <= 0:
            raise ValueError('Limit cannot be <= 0')
        self._limit = limit
        self._loop = events.get_event_loop() if loop is None else loop
        self._buffer = bytearray()
        self._eof = False
        self._waiter = None
        self._exception = None
        self._transport = None
        self._paused = False
        if self._loop.get_debug():
            self._source_traceback = format_helpers.extract_stack(
                sys._getframe(1))

    def __repr__(self):
        words = ['StreamReader']
        if self._buffer:
            words.append(f'{len(self._buffer)} bytes')
        if self._eof:
            words.append('eof')
        if self._limit != _DEFAULT_LIMIT:
            words.append(f'limit={self._limit}')
        if self._waiter:
            words.append(f'waiter={self._waiter!r}')
        if self._exception:
            words.append(f'exception={self._exception!r}')
        if self._transport:
            words.append(f'transport={self._transport!r}')
        if self._paused:
            words.append('paused')
        return '<{}>'.format(' '.join(words))

    def exception(self):
        return self._exception

    def _take_waiter(self):
        waiter, self._waiter = self._waiter, None
        if waiter is not None and not waiter.cancelled():
            return waiter
        return None

    def set_exception(self, exc):
        self._exception = exc
        waiter = self._take_waiter()
        if waiter is not None:
            waiter.set_exception(exc)

    def _wakeup_waiter(self):
        waiter = self._take_waiter()
        if waiter is not None:
            waiter.set_result(None)

    def set_transport(self, transport):
        assert self._transport is None, 'Transport already set'
        self._transport = transport

    def _maybe_resume_transport(self):
        if self._paused and len(self._buffer) <= self._limit:
            self._paused = False
            self._transport.resume_reading()

    def feed_eof(self):
        self._eof = True
        self._wakeup_waiter()

    def at_eof(self):
        """True if the buffer is empty and EOF was received."""
        return self._eof and not self._buffer

    def feed_data(self, data):
        assert not self._eof, 'feed_data after feed_eof'
        if not data:
            return
        self._buffer.extend(data)
        self._wakeup_waiter()
        if (self._transport is not None and not self._paused and
                len(self._buffer) > 2 * self._limit):
            try:
                self._transport.pause_reading()
            except NotImplementedError:
                # cannot pause: stop trying
                self._transport = None
            else:
                self._paused = True

    async def _wait_for_data(self, func_name):
        """Wait until feed_data() or feed_eof() is called."""
        if self._waiter is not None:
            raise RuntimeError(
                f'{func_name}() called while another coroutine is '
                f'already waiting for incoming data')
        assert not self._eof, '_wait_for_data after EOF'
        if self._paused:
            # the buffer may never reach the limit again: read more now
            self._paused = False
            self._transport.resume_reading()
        self._waiter = self._loop.create_future()
        try:
            await self._waiter
        finally:
            self._waiter = None

    def _consume(self, n):
        chunk = bytes(self._buffer[:n])
        del self._buffer[:n]
        self._maybe_resume_transport()
        return chunk

    async def readline(self):
        """Read one line (ending with b'\\n'); at EOF the partial line (or
        b'') is returned.  ValueError if the line exceeds the limit."""
        sep = b'\n'
        try:
            return await self.readuntil(sep)
        except exceptions.IncompleteReadError as e:
            return e.partial
        except exceptions.LimitOverrunError as e:
            if self._buffer.startswith(sep, e.consumed):
                del self._buffer[:e.consumed + len(sep)]
            else:
                self._buffer.clear()
            self._maybe_resume_transport()
            raise ValueError(e.args[0])

    async def readuntil(self, separator=b'\n'):
        """Read up to and including the first *separator* (or one of a
        tuple of separators, the earliest-ending match winning).

        IncompleteReadError at EOF; LimitOverrunError if the data before
        the separator exceeds the limit (the data stays buffered).
        """
        if isinstance(separator, (bytes, bytearray)):
            separators = [bytes(separator)]
        else:
            separators = [bytes(s) for s in separator]
            if not separators:
                raise ValueError('Separator should contain at least one '
                                 'element')
        if any(not s for s in separators):
            raise ValueError('Separator should be at least one-byte string')
        if self._exception is not None:
            raise self._exception

        longest = max(map(len, separators))
        scanned = 0   # bytes known not to contain the end of a match
        while True:
            buf = self._buffer
            match_end = match_start = None
            for sep in separators:
                start = max(0, scanned - len(sep) + 1)
                pos = buf.find(sep, start)
                if pos != -1:
                    end = pos + len(sep)
                    if match_end is None or end < match_end:
                        match_end = end
                        match_start = pos
            if match_end is not None:
                if match_start > self._limit:
                    # "consumed" is where the separator starts
                    raise exceptions.LimitOverrunError(
                        "Separator is found, but chunk is longer than limit",
                        match_start)
                return self._consume(match_end)

            scanned = max(0, len(buf) + 1 - longest)
            if scanned > self._limit:
                raise exceptions.LimitOverrunError(
                    'Separator is not found, and chunk exceed the limit',
                    scanned)
            if self._eof:
                partial = bytes(buf)
                buf.clear()
                raise exceptions.IncompleteReadError(partial, None)
            await self._wait_for_data('readuntil')

    async def read(self, n=-1):
        """Read up to *n* bytes (n < 0: until EOF).  Returns b'' at EOF."""
        if self._exception is not None:
            raise self._exception
        if n == 0:
            return b''
        if n < 0:
            parts = []
            while True:
                part = await self.read(self._limit)
                if not part:
                    return b''.join(parts)
                parts.append(part)
        if not self._buffer and not self._eof:
            await self._wait_for_data('read')
        return self._consume(n)

    async def readexactly(self, n):
        """Read exactly *n* bytes; IncompleteReadError if EOF comes first."""
        if n < 0:
            raise ValueError('readexactly size can not be less than zero')
        if self._exception is not None:
            raise self._exception
        if n == 0:
            return b''
        while len(self._buffer) < n:
            if self._eof:
                partial = bytes(self._buffer)
                self._buffer.clear()
                raise exceptions.IncompleteReadError(partial, n)
            await self._wait_for_data('readexactly')
        return self._consume(n)

    def __aiter__(self):
        return self

    async def __anext__(self):
        line = await self.readline()
        if line == b'':
            raise StopAsyncIteration
        return line
