"""Event loop and transports built on the selectors module."""

__all__ = 'BaseSelectorEventLoop',

import collections
import contextvars
import errno
import functools
import itertools
import os
import selectors
import socket
import warnings
import weakref

try:
    import ssl
except ImportError:
    ssl = None

from . import base_events
from . import constants
from . import events
from . import futures
from . import protocols
from . import sslproto
from . import transports
from . import trsock
from .log import logger

_HAS_SENDMSG = hasattr(socket.socket, 'sendmsg')

if _HAS_SENDMSG:
    try:
        SC_IOV_MAX = os.sysconf('SC_IOV_MAX')
    except (OSError, ValueError, AttributeError):
        _HAS_SENDMSG = False


def _test_selector_event(selector, fd, event):
    # Is *fd* registered in *selector* for *event*?
    try:
        key = selector.get_key(fd)
    except KeyError:
        return False
    return bool(key.events & event)


_RESOURCE_ERRNOS = (errno.EMFILE, errno.ENFILE, errno.ENOBUFS, errno.ENOMEM)


class BaseSelectorEventLoop(base_events.BaseEventLoop):
    """Event loop driven by a selectors.BaseSelector.

    A socketpair (the "self-pipe") lets other threads and signal handlers
    wake the selector up.
    """

    def __init__(self, selector=None):
        super().__init__()
        if selector is None:
            selector = selectors.DefaultSelector()
        logger.debug('Using selector: %s', type(selector).__name__)
        self._selector = selector
        self._make_self_pipe()
        self._transports = weakref.WeakValueDictionary()

    # ------------------------------------------------------------ transports

    def _make_socket_transport(self, sock, protocol, waiter=None, *,
                               extra=None, server=None):
        self._ensure_fd_no_transport(sock)
        return _SelectorSocketTransport(self, sock, protocol, waiter,
                                        extra, server)

    def _make_ssl_transport(
            self, rawsock, protocol, sslcontext, waiter=None,
            *, server_side=False, server_hostname=None,
            extra=None, server=None,
            ssl_handshake_timeout=constants.SSL_HANDSHAKE_TIMEOUT,
            ssl_shutdown_timeout=constants.SSL_SHUTDOWN_TIMEOUT,
            call_connection_made=True):
        self._ensure_fd_no_transport(rawsock)
        tls = sslproto.SSLProtocol(
            self, protocol, sslcontext, waiter, server_side, server_hostname,
            ssl_handshake_timeout=ssl_handshake_timeout,
            ssl_shutdown_timeout=ssl_shutdown_timeout,
            call_connection_made=call_connection_made)
        _SelectorSocketTransport(self, rawsock, tls, extra=extra,
                                 server=server)
        return tls._app_transport

    def _make_datagram_transport(self, sock, protocol,
                                 address=None, waiter=None, extra=None):
        self._ensure_fd_no_transport(sock)
        return _SelectorDatagramTransport(self, sock, protocol,
                                          address, waiter, extra)

    def close(self):
        if self.is_running():
            raise RuntimeError("Cannot close a running event loop")
        if self.is_closed():
            return
        self._close_self_pipe()
        super().close()
        if self._selector is not None:
            self._selector.close()
            self._selector = None

    # ------------------------------------------------------------ self-pipe

    def _make_self_pipe(self):
        self._ssock, self._csock = socket.socketpair()
        self._ssock.setblocking(False)
        self._csock.setblocking(False)
        self._internal_fds += 1
        self._add_reader(self._ssock.fileno(), self._read_from_self)

    def _close_self_pipe(self):
        self._remove_reader(self._ssock.fileno())
        for attr in ('_ssock', '_csock'):
            getattr(self, attr).close()
            setattr(self, attr, None)
        self._internal_fds -= 1

    def _process_self_data(self, data):
        pass

    def _read_from_self(self):
        while True:
            try:
                chunk = self._ssock.recv(4096)
            except InterruptedError:
                continue
            except BlockingIOError:
                return
            if not chunk:
                return
            self._process_self_data(chunk)

    def _write_to_self(self):
        # May run in another thread or in a signal handler: the socket can
        # be closed concurrently, so read the attribute only once.
        wake = self._csock
        if wake is None:
            return
        try:
            wake.send(b'\0')
        except OSError:
            if self._debug:
                logger.debug("Fail to write a null byte into the self-pipe "
                             "socket", exc_info=True)

    # ------------------------------------------------------------ serving

    def _start_serving(self, protocol_factory, sock,
                       sslcontext=None, server=None, backlog=100,
                       ssl_handshake_timeout=constants.SSL_HANDSHAKE_TIMEOUT,
                       ssl_shutdown_timeout=constants.SSL_SHUTDOWN_TIMEOUT,
                       context=None):
        # accepting runs in the server's context (captured at its creation)
        self._add_reader(sock.fileno(), self._accept_connection,
                         protocol_factory, sock, sslcontext, server, backlog,
                         ssl_handshake_timeout, ssl_shutdown_timeout, context,
                         context=context)

    def _accept_connection(
            self, protocol_factory, sock,
            sslcontext=None, server=None, backlog=100,
            ssl_handshake_timeout=constants.SSL_HANDSHAKE_TIMEOUT,
            ssl_shutdown_timeout=constants.SSL_SHUTDOWN_TIMEOUT,
            context=None):
        # Accept up to backlog + 1 pending connections per wake-up.
        for _ in range(backlog + 1):
            try:
                conn, addr = sock.accept()
                if self._debug:
                    logger.debug("%r got a new connection from %r: %r",
                                 server, addr, conn)
                conn.setblocking(False)
            except ConnectionAbortedError:
                continue   # the client gave up already
            except (BlockingIOError, InterruptedError):
                return     # nothing more pending
            except OSError as exc:
                if exc.errno not in _RESOURCE_ERRNOS:
                    raise
                # out of descriptors/memory: stop accepting for a while
                self.call_exception_handler({
                    'message': 'socket.accept() out of system resource',
                    'exception': exc,
                    'socket': trsock.TransportSocket(sock),
                })
                self._remove_reader(sock.fileno())
                self.call_later(constants.ACCEPT_RETRY_DELAY,
                                self._start_serving, protocol_factory, sock,
                                sslcontext, server, backlog,
                                ssl_handshake_timeout, ssl_shutdown_timeout,
                                context)
                return
            else:
                self.create_task(self._accept_connection2(
                    protocol_factory, conn, {'peername': addr}, sslcontext,
                    server, ssl_handshake_timeout, ssl_shutdown_timeout))

    async def _accept_connection2(
            self, protocol_factory, conn, extra,
            sslcontext=None, server=None,
            ssl_handshake_timeout=constants.SSL_HANDSHAKE_TIMEOUT,
            ssl_shutdown_timeout=constants.SSL_SHUTDOWN_TIMEOUT):
        protocol = transport = None
        try:
            protocol = protocol_factory()
            waiter = self.create_future()
            if sslcontext:
                transport = self._make_ssl_transport(
                    conn, protocol, sslcontext, waiter=waiter,
                    server_side=True, extra=extra, server=server,
                    ssl_handshake_timeout=ssl_handshake_timeout,
                    ssl_shutdown_timeout=ssl_shutdown_timeout)
            else:
                transport = self._make_socket_transport(
                    conn, protocol, waiter=waiter, extra=extra, server=server)
            try:
                await waiter
            except BaseException:
                transport.close()
                raise
        except (SystemExit, KeyboardInterrupt):
            raise
        except BaseException as exc:
            if self._debug:
                report = {'message': 'Error on transport creation for '
                                     'incoming connection',
                          'exception': exc}
                if protocol is not None:
                    report['protocol'] = protocol
                if transport is not None:
                    report['transport'] = transport
                self.call_exception_handler(report)

    def _stop_serving(self, sock):
        self._remove_reader(sock.fileno())
        sock.close()

    # ------------------------------------------------------------ readers

    def _ensure_fd_no_transport(self, fd):
        fileno = fd
        if not isinstance(fileno, int):
            try:
                fileno = int(fileno.fileno())
            except (AttributeError, TypeError, ValueError):
                raise ValueError(f"Invalid file object: {fd!r}") from None
        owner = self._transports.get(fileno)
        if owner is not None and not owner.is_closing():
            raise RuntimeError(f'File descriptor {fd!r} is used by transport '
                               f'{owner!r}')

    def _set_callback(self, fd, event, handle):
        # data of a registration is (reader handle, writer handle)
        key = self._selector.get_map().get(fd)
        if key is None:
            if handle is None:
                return False
            pair = (handle, None) if event == selectors.EVENT_READ \
                else (None, handle)
            self._selector.register(fd, event, pair)
            return True
        reader, writer = key.data
        previous = reader if event == selectors.EVENT_READ else writer
        if event == selectors.EVENT_READ:
            reader = handle
        else:
            writer = handle
        mask = key.events | event if handle is not None \
            else key.events & ~event
        if mask:
            self._selector.modify(fd, mask, (reader, writer))
        else:
            self._selector.unregister(fd)
        if previous is not None:
            previous.cancel()
            return True
        return False

    def _add_reader(self, fd, callback, *args, context=None):
        self._check_closed()
        handle = events.Handle(callback, args, self, context)
        self._set_callback(fd, selectors.EVENT_READ, handle)
        return handle

    def _remove_reader(self, fd):
        if self.is_closed():
            return False
        return self._set_callback(fd, selectors.EVENT_READ, None)

    def _add_writer(self, fd, callback, *args, context=None):
        self._check_closed()
        handle = events.Handle(callback, args, self, context)
        self._set_callback(fd, selectors.EVENT_WRITE, handle)
        return handle

    def _remove_writer(self, fd):
        if self.is_closed():
            return False
        return self._set_callback(fd, selectors.EVENT_WRITE, None)

    def add_reader(self, fd, callback, *args):
        """Call callback(*args) whenever *fd* is readable."""
        self._ensure_fd_no_transport(fd)
        self._add_reader(fd, callback, *args)

    def remove_reader(self, fd):
        """Stop watching *fd* for reading; True if it was watched."""
        self._ensure_fd_no_transport(fd)
        return self._remove_reader(fd)

    def add_writer(self, fd, callback, *args):
        """Call callback(*args) whenever *fd* is writable."""
        self._ensure_fd_no_transport(fd)
        self._add_writer(fd, callback, *args)

    def remove_writer(self, fd):
        """Stop watching *fd* for writing; True if it was watched."""
        self._ensure_fd_no_transport(fd)
        return self._remove_writer(fd)

    def _process_events(self, event_list):
        for key, mask in event_list:
            reader, writer = key.data
            for flag, handle, remove in (
                    (selectors.EVENT_READ, reader, self._remove_reader),
                    (selectors.EVENT_WRITE, writer, self._remove_writer)):
                if mask & flag and handle is not None:
                    if handle._cancelled:
                        remove(key.fileobj)
                    else:
                        self._add_callback(handle)

    # ------------------------------------------------------------ sock_* API

    def _check_sock(self, sock):
        base_events._check_ssl_socket(sock)
        if self._debug and sock.gettimeout() != 0:
            raise ValueError("the socket must be non-blocking")

    def _sock_read_done(self, fd, fut, handle=None):
        if handle is None or not handle.cancelled():
            self.remove_reader(fd)

    def _sock_write_done(self, fd, fut, handle=None):
        if handle is None or not handle.cancelled():
            self.remove_writer(fd)

    async def _wait_readable(self, sock, attempt):
        """Retry attempt() whenever *sock* becomes readable until it stops
        raising BlockingIOError; returns its value."""
        fut = self.create_future()
        fd = sock.fileno()
        self._ensure_fd_no_transport(fd)

        def on_readable():
            if fut.done():
                return
            try:
                value = attempt()
            except (BlockingIOError, InterruptedError):
                return
            except (SystemExit, KeyboardInterrupt):
                raise
            except BaseException as exc:
                fut.set_exception(exc)
            else:
                fut.set_result(value)

        handle = self._add_reader(fd, on_readable)
        fut.add_done_callback(functools.partial(self._sock_read_done, fd,
                                                handle=handle))
        return await fut

    async def sock_recv(self, sock, n):
        """Receive up to *n* bytes from the non-blocking socket."""
        self._check_sock(sock)
        try:
            return sock.recv(n)
        except (BlockingIOError, InterruptedError):
            pass
        return await self._wait_readable(sock, lambda: sock.recv(n))

    async def sock_recv_into(self, sock, buf):
        """Receive into *buf*; returns the byte count."""
        self._check_sock(sock)
        try:
            return sock.recv_into(buf)
        except (BlockingIOError, InterruptedError):
            pass
        return await self._wait_readable(sock, lambda: sock.recv_into(buf))

    async def sock_recvfrom(self, sock, bufsize):
        """Receive a datagram; returns (data, address)."""
        self._check_sock(sock)
        try:
            return sock.recvfrom(bufsize)
        except (BlockingIOError, InterruptedError):
            pass
        return await self._wait_readable(sock, lambda: sock.recvfrom(bufsize))

    async def sock_recvfrom_into(self, sock, buf, nbytes=0):
        """Receive a datagram into *buf*; returns (nbytes, address)."""
        self._check_sock(sock)
        if not nbytes:
            nbytes = len(buf)
        try:
            return sock.recvfrom_into(buf, nbytes)
        except (BlockingIOError, InterruptedError):
            pass
        return await self._wait_readable(
            sock, lambda: sock.recvfrom_into(buf, nbytes))

    async def sock_sendall(self, sock, data):
        """Send all of *data* (waiting for buffer space as needed)."""
        self._check_sock(sock)
        try:
            sent = sock.send(data)
        except (BlockingIOError, InterruptedError):
            sent = 0
        if sent == len(data):
            return
        fut = self.create_future()
        fd = sock.fileno()
        self._ensure_fd_no_transport(fd)
        view = memoryview(data)
        offset = [sent]

        def on_writable():
            if fut.done():
                return
            try:
                n = sock.send(view[offset[0]:])
            except (BlockingIOError, InterruptedError):
                return
            except (SystemExit, KeyboardInterrupt):
                raise
            except BaseException as exc:
                fut.set_exception(exc)
                return
            offset[0] += n
            if offset[0] == len(view):
                fut.set_result(None)

        handle = self._add_writer(fd, on_writable)
        fut.add_done_callback(functools.partial(self._sock_write_done, fd,
                                                handle=handle))
        return await fut

    async def sock_sendto(self, sock, data, address):
        """Send a datagram to *address*."""
        self._check_sock(sock)
        try:
            return sock.sendto(data, address)
        except (BlockingIOError, InterruptedError):
            pass
        fut = self.create_future()
        fd = sock.fileno()
        self._ensure_fd_no_transport(fd)

        def on_writable():
            if fut.done():
                return
            try:
                n = sock.sendto(data, address)
            except (BlockingIOError, InterruptedError):
                return
            except (SystemExit, KeyboardInterrupt):
                raise
            except BaseException as exc:
                fut.set_exception(exc)
            else:
                fut.set_result(n)

        handle = self._add_writer(fd, on_writable)
        fut.add_done_callback(functools.partial(self._sock_write_done, fd,
                                                handle=handle))
        return await fut

    async def sock_connect(self, sock, address):
        """Connect the non-blocking socket to *address*."""
        self._check_sock(sock)
        if sock.family == socket.AF_INET or (
                base_events._HAS_IPv6 and sock.family == socket.AF_INET6):
            resolved = await self._ensure_resolved(
                address, family=sock.family, type=sock.type, proto=sock.proto,
                loop=self)
            address = resolved[0][4]
        fut = self.create_future()
        self._sock_connect(fut, sock, address)
        try:
            return await fut
        finally:
            fut = None

    def _sock_connect(self, fut, sock, address):
        fd = sock.fileno()
        try:
            sock.connect(address)
        except (BlockingIOError, InterruptedError):
            # in progress: completion is signalled by writability
            self._ensure_fd_no_transport(fd)
            handle = self._add_writer(fd, self._sock_connect_cb, fut, sock,
                                      address)
            fut.add_done_callback(functools.partial(self._sock_write_done, fd,
                                                    handle=handle))
        except (SystemExit, KeyboardInterrupt):
            raise
        except BaseException as exc:
            fut.set_exception(exc)
        else:
            fut.set_result(None)
        finally:
            fut = None

    def _sock_connect_cb(self, fut, sock, address):
        if fut.done():
            return
        try:
            err = sock.getsockopt(socket.SOL_SOCKET, socket.SO_ERROR)
            if err != 0:
                raise OSError(err, f'Connect call failed {address}')
        except (BlockingIOError, InterruptedError):
            pass   # spurious wake-up
        except (SystemExit, KeyboardInterrupt):
            raise
        except BaseException as exc:
            fut.set_exception(exc)
        else:
            fut.set_result(None)
        finally:
            fut = None

    async def sock_accept(self, sock):
        """Accept a connection; returns (conn, address)."""
        self._check_sock(sock)
        fut = self.create_future()
        self._sock_accept(fut, sock)
        return await fut

    def _sock_accept(self, fut, sock):
        if fut.done():
            # cancelled while the readiness callback was queued: leave the
            # pending connection for the next accept
            return
        fd = sock.fileno()
        try:
            conn, address = sock.accept()
            conn.setblocking(False)
        except (BlockingIOError, InterruptedError):
            self._ensure_fd_no_transport(fd)
            handle = self._add_reader(fd, self._sock_accept, fut, sock)
            fut.add_done_callback(functools.partial(self._sock_read_done, fd,
                                                    handle=handle))
        except (SystemExit, KeyboardInterrupt):
            raise
        except BaseException as exc:
            fut.set_exception(exc)
        else:
            fut.set_result((conn, address))

    async def _sendfile_native(self, transp, file, offset, count):
        del self._transports[transp._sock_fd]
        was_reading = transp.is_reading()
        transp.pause_reading()
        await transp._make_empty_waiter()
        try:
            return await self.sock_sendfile(transp._sock, file, offset, count,
                                            fallback=False)
        finally:
            transp._reset_empty_waiter()
            if was_reading:
                transp.resume_reading()
            self._transports[transp._sock_fd] = transp


class _SelectorTransport(transports._FlowControlMixin,
                         transports.Transport):

    max_size = 256 * 1024   # bytes read per receive call

    _buffer_factory = collections.deque

    # Reference to the socket kept until connection_lost() completes (so
    # that close() can be called from __del__ safely).
    _sock = None

    def __init__(self, loop, sock, protocol, extra=None, server=None):
        super().__init__(extra, loop)
        # callbacks of this transport run in the context it was created in,
        # whoever (re)registers them
        self._context = contextvars.copy_context()
        self._extra['socket'] = trsock.TransportSocket(sock)
        for key, getter in (('sockname', sock.getsockname),
                            ('peername', sock.getpeername)):
            if key in self._extra:
                continue
            try:
                self._extra[key] = getter()
            except OSError:
                self._extra[key] = None
        self._sock = sock
        self._sock_fd = sock.fileno()
        self._protocol_connected = False
        self.set_protocol(protocol)
        self._server = server
        self._buffer = self._buffer_factory()
        self._conn_lost = 0     # failed writes after the connection broke
        self._closing = False
        self._paused = False
        if self._server is not None:
            self._server._attach(self)
        loop._transports[self._sock_fd] = self

    def __repr__(self):
        words = [type(self).__name__]
        if self._sock is None:
            words.append('closed')
        elif self._closing:
            words.append('closing')
        words.append(f'fd={self._sock_fd}')
        if self._loop is not None and not self._loop.is_closed():
            selector = self._loop._selector
            if _test_selector_event(selector, self._sock_fd,
                                    selectors.EVENT_READ):
                words.append('read=polling')
            else:
                words.append('read=idle')
            writing = _test_selector_event(selector, self._sock_fd,
                                           selectors.EVENT_WRITE)
            words.append('write=<%s, bufsize=%s>'
                         % ('polling' if writing else 'idle',
                            self.get_write_buffer_size()))
        return '<{}>'.format(' '.join(words))

    def abort(self):
        self._force_close(None)

    def set_protocol(self, protocol):
        self._protocol = protocol
        self._protocol_connected = True

    def get_protocol(self):
        return self._protocol

    def is_closing(self):
        return self._closing

    def is_reading(self):
        return not self.is_closing() and not self._paused

    def pause_reading(self):
        if not self.is_reading():
            return
        self._paused = True
        self._loop._remove_reader(self._sock_fd)
        if self._loop.get_debug():
            logger.debug("%r pauses reading", self)

    def resume_reading(self):
        if self._closing or not self._paused:
            return
        self._paused = False
        self._add_reader(self._sock_fd, self._read_ready)
        if self._loop.get_debug():
            logger.debug("%r resumes reading", self)

    def close(self):
        if self._closing:
            return
        self._closing = True
        self._loop._remove_reader(self._sock_fd)
        if not self._buffer:
            # nothing left to flush: finish now
            self._conn_lost += 1
            self._loop._remove_writer(self._sock_fd)
            self._loop.call_soon(self._call_connection_lost, None,
                                 context=self._context)

    def __del__(self, _warn=warnings.warn):
        if self._sock is not None:
            _warn(f"unclosed transport {self!r}", ResourceWarning, source=self)
            self._sock.close()
            if self._server is not None:
                self._server._detach(self)

    def _fatal_error(self, exc, message='Fatal error on transport'):
        # Plain connection errors are expected: only debug-log them.
        if isinstance(exc, OSError):
            if self._loop.get_debug():
                logger.debug("%r: %s", self, message, exc_info=True)
        else:
            self._loop.call_exception_handler({
                'message': message,
                'exception': exc,
                'transport': self,
                'protocol': self._protocol,
            })
        self._force_close(exc)

    def _force_close(self, exc):
        if self._conn_lost:
            return
        if self._buffer:
            self._buffer.clear()
            self._loop._remove_writer(self._sock_fd)
        if not self._closing:
            self._closing = True
            self._loop._remove_reader(self._sock_fd)
        self._conn_lost += 1
        self._loop.call_soon(self._call_connection_lost, exc,
                                 context=self._context)

    def _call_connection_lost(self, exc):
        try:
            if self._protocol_connected:
                self._protocol.connection_lost(exc)
        finally:
            self._sock.close()
            self._sock = None
            self._protocol = None
            self._loop = None
            server, self._server = self._server, None
            if server is not None:
                server._detach(self)

    def get_write_buffer_size(self):
        return sum(map(len, self._buffer))

    def _add_reader(self, fd, callback, *args):
        if not self.is_reading():
            return
        self._loop._add_reader(fd, callback, *args, context=self._context)


class _SelectorSocketTransport(_SelectorTransport):

    _start_tls_compatible = True
    _sendfile_compatible = constants._SendfileMode.TRY_NATIVE

    def __init__(self, loop, sock, protocol, waiter=None,
                 extra=None, server=None):
        self._read_ready_cb = None
        super().__init__(loop, sock, protocol, extra, server)
        self._eof = False
        self._empty_waiter = None
        self._write_ready = self._write_sendmsg if _HAS_SENDMSG \
            else self._write_send
        # small writes must not wait for Nagle's algorithm
        base_events._set_nodelay(self._sock)

        self._loop.call_soon(self._protocol.connection_made, self,
                             context=self._context)
        self._loop.call_soon(self._add_reader, self._sock_fd, self._read_ready)
        if waiter is not None:
            self._loop.call_soon(futures._set_result_unless_cancelled,
                                 waiter, None)

    def set_protocol(self, protocol):
        if isinstance(protocol, protocols.BufferedProtocol):
            self._read_ready_cb = self._read_ready__get_buffer
        else:
            self._read_ready_cb = self._read_ready__data_received
        super().set_protocol(protocol)

    def _read_ready(self):
        self._read_ready_cb()

    def _call_protocol(self, method, *args, what):
        """Call a protocol callback; fatal error (False) if it raises."""
        try:
            return True, method(*args)
        except (SystemExit, KeyboardInterrupt):
            raise
        except BaseException as exc:
            self._fatal_error(exc, f'Fatal error: protocol.{what}() call '
                                   f'failed.')
            return False, None

    def _read_ready__get_buffer(self):
        if self._conn_lost:
            return
        try:
            buf = self._protocol.get_buffer(-1)
            if not len(buf):
                raise RuntimeError('get_buffer() returned an empty buffer')
        except (SystemExit, KeyboardInterrupt):
            raise
        except BaseException as exc:
            self._fatal_error(exc, 'Fatal error: protocol.get_buffer() '
                                   'call failed.')
            return
        try:
            nbytes = self._sock.recv_into(buf)
        except (BlockingIOError, InterruptedError):
            return
        except (SystemExit, KeyboardInterrupt):
            raise
        except BaseException as exc:
            self._fatal_error(exc, 'Fatal read error on socket transport')
            return
        if not nbytes:
            self._read_ready__on_eof()
            return
        self._call_protocol(self._protocol.buffer_updated, nbytes,
                            what='buffer_updated')

    def _read_ready__data_received(self):
        if self._conn_lost:
            return
        try:
            data = self._sock.recv(self.max_size)
        except (BlockingIOError, InterruptedError):
            return
        except (SystemExit, KeyboardInterrupt):
            raise
        except BaseException as exc:
            self._fatal_error(exc, 'Fatal read error on socket transport')
            return
        if not data:
            self._read_ready__on_eof()
            return
        self._call_protocol(self._protocol.data_received, data,
                            what='data_received')

    def _read_ready__on_eof(self):
        if self._loop.get_debug():
            logger.debug("%r received EOF", self)
        ok, keep_open = self._call_protocol(self._protocol.eof_received,
                                            what='eof_received')
        if not ok:
            return
        if keep_open:
            # half-closed: stop reading, writing stays possible
            self._loop._remove_reader(self._sock_fd)
        else:
            self.close()

    def _log_lost_write(self):
        if self._conn_lost >= constants.LOG_THRESHOLD_FOR_CONNLOST_WRITES:
            logger.warning('socket.send() raised exception.')
        self._conn_lost += 1

    def write(self, data):
        if not isinstance(data, (bytes, bytearray, memoryview)):
            raise TypeError(f'data argument must be a bytes, bytearray, or '
                            f'memoryview object, not {type(data).__name__!r}')
        if self._eof:
            raise RuntimeError('Cannot call write() after write_eof()')
        if self._empty_waiter is not None:
            raise RuntimeError('unable to write; sendfile is in progress')
        if not data:
            return
        if self._conn_lost:
            self._log_lost_write()
            return

        if not self._buffer:
            # try a direct send first
            try:
                sent = self._sock.send(data)
            except (BlockingIOError, InterruptedError):
                sent = 0
            except (SystemExit, KeyboardInterrupt):
                raise
            except BaseException as exc:
                self._fatal_error(exc, 'Fatal write error on socket transport')
                return
            if sent == len(data):
                return
            data = memoryview(data)[sent:]
            self._loop._add_writer(self._sock_fd, self._write_ready,
                                  context=self._context)

        # keep a private copy: the caller may reuse its buffer
        self._buffer.append(memoryview(bytes(data)))
        self._maybe_pause_protocol()

    def _get_sendmsg_buffer(self):
        return itertools.islice(self._buffer, SC_IOV_MAX)

    def _adjust_leftover_buffer(self, nbytes):
        buffer = self._buffer
        while nbytes:
            head = buffer.popleft()
            if len(head) > nbytes:
                buffer.appendleft(head[nbytes:])
                return
            nbytes -= len(head)

    def _after_flush_attempt(self):
        self._maybe_resume_protocol()
        if self._buffer:
            return
        self._loop._remove_writer(self._sock_fd)
        if self._empty_waiter is not None:
            self._empty_waiter.set_result(None)
        if self._conn_lost:
            return      # e.g. closed by resume_writing(): already scheduled
        if self._closing:
            self._call_connection_lost(None)
        elif self._eof:
            self._sock.shutdown(socket.SHUT_WR)

    def _write_failed(self, exc):
        self._loop._remove_writer(self._sock_fd)
        self._buffer.clear()
        self._fatal_error(exc, 'Fatal write error on socket transport')
        if self._empty_waiter is not None:
            self._empty_waiter.set_exception(exc)

    def _write_sendmsg(self):
        assert self._buffer, 'Data should not be empty'
        if self._conn_lost:
            return
        try:
            sent = self._sock.sendmsg(self._get_sendmsg_buffer())
            self._adjust_leftover_buffer(sent)
        except (BlockingIOError, InterruptedError):
            pass
        except (SystemExit, KeyboardInterrupt):
            raise
        except BaseException as exc:
            self._write_failed(exc)
        else:
            self._after_flush_attempt()

    def _write_send(self):
        assert self._buffer, 'Data should not be empty'
        if self._conn_lost:
            return
        try:
            head = self._buffer.popleft()
            sent = self._sock.send(head)
            if sent != len(head):
                self._buffer.appendleft(head[sent:])
        except (BlockingIOError, InterruptedError):
            self._buffer.appendleft(head)
        except (SystemExit, KeyboardInterrupt):
            raise
        except BaseException as exc:
            self._write_failed(exc)
        else:
            self._after_flush_attempt()

    def write_eof(self):
        if self._closing or self._eof:
            return
        self._eof = True
        if not self._buffer:
            self._sock.shutdown(socket.SHUT_WR)

    def writelines(self, list_of_data):
        if self._eof:
            raise RuntimeError('Cannot call writelines() after write_eof()')
        if self._empty_waiter is not None:
            raise RuntimeError('unable to writelines; sendfile is in progress')
        if not list_of_data:
            return
        if self._conn_lost:
            self._log_lost_write()
            return
        self._buffer.extend(memoryview(bytes(chunk)) for chunk in list_of_data)
        self._write_ready()
        if self._buffer:
            self._loop._add_writer(self._sock_fd, self._write_ready,
                                  context=self._context)
            self._maybe_pause_protocol()

    def can_write_eof(self):
        return True

    def _call_connection_lost(self, exc):
        try:
            super()._call_connection_lost(exc)
        finally:
            self._write_ready = None
            if self._empty_waiter is not None:
                self._empty_waiter.set_exception(
                    ConnectionError("Connection is closed by peer"))

    def _make_empty_waiter(self):
        if self._empty_waiter is not None:
            raise RuntimeError("Empty waiter is already set")
        self._empty_waiter = self._loop.create_future()
        if not self._buffer:
            self._empty_waiter.set_result(None)
        return self._empty_waiter

    def _reset_empty_waiter(self):
        self._empty_waiter = None

    def close(self):
        self._read_ready_cb = None
        super().close()


class _SelectorDatagramTransport(_SelectorTransport,
                                 transports.DatagramTransport):

    _buffer_factory = collections.deque
    _header_size = 8   # accounting overhead per queued datagram

    def __init__(self, loop, sock, protocol, address=None,
                 waiter=None, extra=None):
        super().__init__(loop, sock, protocol, extra)
        self._address = address
        self._buffer_size = 0
        self._loop.call_soon(self._protocol.connection_made, self,
                             context=self._context)
        self._loop.call_soon(self._add_reader, self._sock_fd, self._read_ready)
        if waiter is not None:
            self._loop.call_soon(futures._set_result_unless_cancelled,
                                 waiter, None)

    def get_write_buffer_size(self):
        return self._buffer_size

    def _read_ready(self):
        if self._conn_lost:
            return
        try:
            data, addr = self._sock.recvfrom(self.max_size)
        except (BlockingIOError, InterruptedError):
            pass
        except OSError as exc:
            self._protocol.error_received(exc)
        except (SystemExit, KeyboardInterrupt):
            raise
        except BaseException as exc:
            self._fatal_error(exc, 'Fatal read error on datagram transport')
        else:
            self._protocol.datagram_received(data, addr)

    def _send_one(self, data, addr):
        if self._address:
            return self._sock.send(data)
        return self._sock.sendto(data, addr)

    def sendto(self, data, addr=None):
        if not isinstance(data, (bytes, bytearray, memoryview)):
            raise TypeError(f'data argument must be a bytes-like object, '
                            f'not {type(data).__name__!r}')
        if self._address:
            if addr not in (None, self._address):
                raise ValueError(
                    f'Invalid address: must be None or {self._address}')
            addr = self._address

        if self._conn_lost and self._address:
            if self._conn_lost >= constants.LOG_THRESHOLD_FOR_CONNLOST_WRITES:
                logger.warning('socket.send() raised exception.')
            self._conn_lost += 1
            return

        if not self._buffer:
            try:
                self._send_one(data, addr)
                return
            except (BlockingIOError, InterruptedError):
                self._loop._add_writer(self._sock_fd, self._sendto_ready,
                                  context=self._context)
            except OSError as exc:
                self._protocol.error_received(exc)
                return
            except (SystemExit, KeyboardInterrupt):
                raise
            except BaseException as exc:
                self._fatal_error(
                    exc, 'Fatal write error on datagram transport')
                return

        self._buffer.append((bytes(data), addr))
        self._buffer_size += len(data) + self._header_size
        self._maybe_pause_protocol()

    def _sendto_ready(self):
        while self._buffer:
            data, addr = self._buffer.popleft()
            self._buffer_size -= len(data) + self._header_size
            try:
                self._send_one(data, addr)
            except (BlockingIOError, InterruptedError):
                self._buffer.appendleft((data, addr))
                self._buffer_size += len(data) + self._header_size
                break
            except OSError as exc:
                self._protocol.error_received(exc)
                return
            except (SystemExit, KeyboardInterrupt):
                raise
            except BaseException as exc:
                self._fatal_error(
                    exc, 'Fatal write error on datagram transport')
                return

        self._maybe_resume_protocol()
        if not self._buffer:
            self._loop._remove_writer(self._sock_fd)
            if self._closing:
                self._call_connection_lost(None)
