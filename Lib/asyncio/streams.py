"""Streams: coroutine-friendly reading and writing over a transport.

open_connection()/start_server() (and their UNIX-socket twins) hand out a
StreamReader and a StreamWriter.  The StreamReaderProtocol installed on
the transport feeds received bytes into the reader and gives the writer
flow control (drain()).
"""

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


def _client_pair(loop, limit):
    reader = StreamReader(limit=limit, loop=loop)
    return reader, StreamReaderProtocol(reader, loop=loop)


def _server_protocol_factory(loop, limit, client_connected_cb):
    def make_protocol():
        reader = StreamReader(limit=limit, loop=loop)
        return StreamReaderProtocol(reader, client_connected_cb, loop=loop)
    return make_protocol


async def open_connection(host=None, port=None, *,
                          limit=_DEFAULT_LIMIT, **kwds):
    """Open a TCP connection; returns (StreamReader, StreamWriter).

    *limit* bounds the reader's buffer; the other keyword arguments are
    those of loop.create_connection().
    """
    loop = events.get_running_loop()
    reader, protocol = _client_pair(loop, limit)
    transport, _ = await loop.create_connection(
        lambda: protocol, host, port, **kwds)
    return reader, StreamWriter(transport, protocol, reader, loop)


async def start_server(client_connected_cb, host=None, port=None, *,
                       limit=_DEFAULT_LIMIT, **kwds):
    """Start a TCP server; each connection calls
    client_connected_cb(reader, writer), and a coroutine it returns runs
    as a task.  Returns the Server (see loop.create_server())."""
    loop = events.get_running_loop()
    return await loop.create_server(
        _server_protocol_factory(loop, limit, client_connected_cb),
        host, port, **kwds)


if hasattr(socket, 'AF_UNIX'):
    async def open_unix_connection(path=None, *,
                                   limit=_DEFAULT_LIMIT, **kwds):
        """open_connection() over a UNIX domain socket."""
        loop = events.get_running_loop()
        reader, protocol = _client_pair(loop, limit)
        transport, _ = await loop.create_unix_connection(
            lambda: protocol, path, **kwds)
        return reader, StreamWriter(transport, protocol, reader, loop)

    async def start_unix_server(client_connected_cb, path=None, *,
                                limit=_DEFAULT_LIMIT, **kwds):
        """start_server() on a UNIX domain socket."""
        loop = events.get_running_loop()
        return await loop.create_unix_server(
            _server_protocol_factory(loop, limit, client_connected_cb),
            path, **kwds)


class FlowControlMixin(protocols.Protocol):
    """Protocol half of StreamWriter.drain().

    The transport calls pause_writing()/resume_writing(); _drain_helper()
    waits while writing is paused and fails once the connection is gone.
    Subclasses must call this connection_lost().
    """

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
        self._release_drainers(None)

    def connection_lost(self, exc):
        self._connection_lost = True
        if self._paused:
            # drainers wake up: with the error, or to find the flag set
            self._release_drainers(exc)

    def _release_drainers(self, exc):
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
    """Feeds a transport into a StreamReader; on the server side it also
    creates the StreamWriter and calls the connection callback."""

    _source_traceback = None

    def __init__(self, stream_reader, client_connected_cb=None, loop=None):
        super().__init__(loop=loop)
        # Only a weak reference: a reader nobody uses any more must be
        # collectable (and its transport closed) even while connected.
        self._stream_reader_wr = (None if stream_reader is None
                                  else weakref.ref(stream_reader))
        if stream_reader is not None:
            self._source_traceback = stream_reader._source_traceback
        if client_connected_cb is not None:
            # a server's reader has no other owner until the client connects
            self._strong_reader = stream_reader
        self._reject_connection = False
        self._client_connected_cb = client_connected_cb
        self._task = None
        self._transport = None
        self._over_ssl = False
        self._closed = self._loop.create_future()

    @property
    def _stream_reader(self):
        ref = self._stream_reader_wr
        return None if ref is None else ref()

    def _note_transport(self, transport):
        self._transport = transport
        # TLS cannot be half-closed (see eof_received())
        self._over_ssl = transport.get_extra_info('sslcontext') is not None

    def _replace_transport(self, transport):
        """After start_tls(): the TLS transport takes the old one's place."""
        self._note_transport(transport)
        reader = self._stream_reader
        if reader is not None:
            reader._transport = transport

    def connection_made(self, transport):
        if self._reject_connection:
            report = {'message': 'An open stream was garbage collected prior '
                                 'to establishing network connection; call '
                                 '"stream.close()" explicitly.'}
            if self._source_traceback:
                report['source_traceback'] = self._source_traceback
            self._loop.call_exception_handler(report)
            transport.abort()
            return
        self._note_transport(transport)
        reader = self._stream_reader
        if reader is not None:
            reader.set_transport(transport)
        if self._client_connected_cb is not None:
            self._start_client_handler(transport, reader)
            self._strong_reader = None

    def _report_handler_failure(self, transport, exc):
        self._loop.call_exception_handler({
            'message': 'Unhandled exception in client_connected_cb',
            'exception': exc,
            'transport': transport,
        })
        transport.close()

    def _start_client_handler(self, transport, reader):
        writer = StreamWriter(transport, self, reader, self._loop)
        try:
            outcome = self._client_connected_cb(reader, writer)
        except (SystemExit, KeyboardInterrupt):
            raise
        except BaseException as exc:
            # a plain function failing is treated like a failing coroutine
            self._report_handler_failure(transport, exc)
            return
        if not coroutines.iscoroutine(outcome):
            return

        def on_handler_done(task):
            if task.cancelled():
                transport.close()
            elif task.exception() is not None:
                self._report_handler_failure(transport, task.exception())

        self._task = self._loop.create_task(outcome)
        self._task.add_done_callback(on_handler_done)

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
        # drop references that could keep a cycle alive
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
        # Returning True keeps a plain transport half-open (we may still
        # write); a TLS transport has to close.
        return not self._over_ssl

    def _get_close_waiter(self, stream):
        return self._closed

    def __del__(self):
        closed = getattr(self, '_closed', None)
        if closed is not None and closed.done() and not closed.cancelled():
            # mark a connection error as seen: nobody awaited wait_closed()
            closed.exception()


class StreamWriter:
    """The writing side of a stream: a transport plus drain()."""

    def __init__(self, transport, protocol, reader, loop):
        assert reader is None or isinstance(reader, StreamReader)
        self._transport = transport
        self._protocol = protocol
        self._reader = reader
        self._loop = loop
        self._complete_fut = loop.create_future()
        self._complete_fut.set_result(None)

    def __repr__(self):
        text = f'<{type(self).__name__} transport={self._transport!r}'
        if self._reader is not None:
            text += f' reader={self._reader!r}'
        return text + '>'

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
        """Wait until the transport's buffer can take more data:

            writer.write(data)
            await writer.drain()

        Raises the reader's error if the connection failed.
        """
        reader = self._reader
        exc = None if reader is None else reader.exception()
        if exc is not None:
            raise exc
        if self._transport.is_closing():
            # One loop turn lets connection_lost() run, so that
            # _drain_helper() can report the lost connection.
            await sleep(0)
        await self._protocol._drain_helper()

    async def start_tls(self, sslcontext, *,
                        server_hostname=None,
                        ssl_handshake_timeout=None,
                        ssl_shutdown_timeout=None):
        """Switch the connection to TLS (client or server side, matching
        how the stream was created)."""
        protocol = self._protocol
        server_side = protocol._client_connected_cb is not None
        await self.drain()
        # (hang1) Bytes the reader already holds are the beginning of the
        # TLS stream (the peer's ClientHello, typically); the TLS layer
        # must see them or the handshake waits for data that never comes.
        extra = {}
        reader = getattr(protocol, '_stream_reader', None)
        if reader is not None and reader._buffer:
            extra['_buffered'] = bytes(reader._buffer)
            reader._buffer.clear()
        tls_transport = await self._loop.start_tls(
            self._transport, protocol, sslcontext,
            server_side=server_side, server_hostname=server_hostname,
            ssl_handshake_timeout=ssl_handshake_timeout,
            ssl_shutdown_timeout=ssl_shutdown_timeout, **extra)
        self._transport = tls_transport
        protocol._replace_transport(tls_transport)

    def __del__(self, warnings=warnings):
        if self._transport.is_closing():
            return
        if self._loop.is_closed():
            warnings.warn("loop is closed", ResourceWarning)
        else:
            self.close()
            warnings.warn(f"unclosed {self!r}", ResourceWarning)


class StreamReader:
    """Received bytes, read with coroutines.

    *limit* bounds what readline()/readuntil() return and, doubled, how
    much may be buffered before the transport is asked to pause.
    """

    _source_traceback = None

    def __init__(self, limit=_DEFAULT_LIMIT, loop=None):
        if limit <= 0:
            raise ValueError('Limit cannot be <= 0')
        self._limit = limit
        self._loop = events.get_event_loop() if loop is None else loop
        self._buffer = bytearray()
        self._eof = False
        self._waiter = None         # future of the coroutine waiting for data
        self._exception = None
        self._transport = None
        self._paused = False        # we paused the transport's reading
        if self._loop.get_debug():
            self._source_traceback = format_helpers.extract_stack(
                sys._getframe(1))

    def __repr__(self):
        details = []
        if self._buffer:
            details.append(f'{len(self._buffer)} bytes')
        if self._eof:
            details.append('eof')
        if self._limit != _DEFAULT_LIMIT:
            details.append(f'limit={self._limit}')
        for label, value in (('waiter', self._waiter),
                             ('exception', self._exception),
                             ('transport', self._transport)):
            if value:
                details.append(f'{label}={value!r}')
        if self._paused:
            details.append('paused')
        return '<StreamReader%s>' % ''.join(' ' + d for d in details)

    def exception(self):
        return self._exception

    def _pop_waiter(self):
        waiter, self._waiter = self._waiter, None
        if waiter is None or waiter.cancelled():
            return None
        return waiter

    def set_exception(self, exc):
        self._exception = exc
        waiter = self._pop_waiter()
        if waiter is not None:
            waiter.set_exception(exc)

    def _wakeup_waiter(self):
        waiter = self._pop_waiter()
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
        """True once EOF was fed and the buffer is empty."""
        return self._eof and not self._buffer

    def feed_data(self, data):
        assert not self._eof, 'feed_data after feed_eof'
        if not data:
            return
        self._buffer.extend(data)
        self._wakeup_waiter()
        transport = self._transport
        if (transport is None or self._paused
                or len(self._buffer) <= 2 * self._limit):
            return
        try:
            transport.pause_reading()
        except NotImplementedError:
            # this transport cannot pause: stop asking
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
        # Reading was paused at twice the limit; a reader that wants more
        # than what is buffered would otherwise wait forever.
        if self._paused:
            self._paused = False
            self._transport.resume_reading()
        self._waiter = self._loop.create_future()
        try:
            await self._waiter
        finally:
            self._waiter = None

    def _take(self, n):
        """Remove and return the first *n* buffered bytes."""
        buf = self._buffer
        if n >= len(buf):
            chunk = bytes(buf)
            buf.clear()
        else:
            # one copy; the temporary view is gone before the buffer shrinks
            chunk = bytes(memoryview(buf)[:n])
            del buf[:n]
        self._maybe_resume_transport()
        return chunk

    def _check_error(self):
        if self._exception is not None:
            raise self._exception

    async def readline(self):
        """Read up to and including b'\\n'.  At EOF the partial line (or
        b'') is returned; a line longer than the limit raises ValueError
        (and is dropped)."""
        try:
            return await self.readuntil(b'\n')
        except exceptions.IncompleteReadError as exc:
            return exc.partial
        except exceptions.LimitOverrunError as exc:
            if self._buffer.startswith(b'\n', exc.consumed):
                del self._buffer[:exc.consumed + 1]
            else:
                self._buffer.clear()
            self._maybe_resume_transport()
            raise ValueError(exc.args[0])

    @staticmethod
    def _separators(separator):
        if isinstance(separator, (bytes, bytearray)):
            seps = [bytes(separator)]
        else:
            seps = [bytes(s) for s in separator]
            if not seps:
                raise ValueError(
                    'Separator should contain at least one element')
        if not all(seps):
            raise ValueError('Separator should be at least one-byte string')
        return seps

    def _earliest_match(self, seps, scanned):
        """(start, end) of the separator match that ends first, looking
        only where a match can end beyond *scanned*; None if none."""
        best = None
        for sep in seps:
            pos = self._buffer.find(sep, max(0, scanned - len(sep) + 1))
            if pos != -1 and (best is None or pos + len(sep) < best[1]):
                best = (pos, pos + len(sep))
        return best

    async def readuntil(self, separator=b'\n'):
        """Read up to and including *separator* (bytes, or a sequence of
        alternatives: the match that ends first wins).

        At EOF before a separator: IncompleteReadError carrying the data.
        When the data before the separator exceeds the limit:
        LimitOverrunError, and the data stays buffered.
        """
        seps = self._separators(separator)
        self._check_error()
        longest = max(len(s) for s in seps)
        scanned = 0
        while True:
            match = self._earliest_match(seps, scanned)
            if match is not None:
                start, end = match
                if start > self._limit:
                    raise exceptions.LimitOverrunError(
                        'Separator is found, but chunk is longer than limit',
                        start)
                return self._take(end)
            # a match must end in data that arrives later
            scanned = max(0, len(self._buffer) + 1 - longest)
            if scanned > self._limit:
                raise exceptions.LimitOverrunError(
                    'Separator is not found, and chunk exceed the limit',
                    scanned)
            if self._eof:
                partial = bytes(self._buffer)
                self._buffer.clear()
                raise exceptions.IncompleteReadError(partial, None)
            await self._wait_for_data('readuntil')

    async def read(self, n=-1):
        """Read at most *n* bytes (as soon as some are available); n < 0
        reads until EOF.  b'' means EOF."""
        if self._exception is not None:     # (inline: hot path)
            raise self._exception
        if n == 0:
            return b''
        if n < 0:
            # in chunks of the limit, so the transport keeps flowing
            chunks = []
            while chunk := await self.read(self._limit):
                chunks.append(chunk)
            return b''.join(chunks)
        if not self._buffer and not self._eof:
            await self._wait_for_data('read')
        return self._take(n)

    async def readexactly(self, n):
        """Read exactly *n* bytes; IncompleteReadError (with what was
        read) if EOF comes first."""
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
        return self._take(n)

    def __aiter__(self):
        return self

    async def __anext__(self):
        line = await self.readline()
        if not line:
            raise StopAsyncIteration
        return line
