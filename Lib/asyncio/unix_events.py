"""Unix flavour of the selector event loop: signals, pipes, AF_UNIX
sockets, subprocesses and os.sendfile()."""

import errno
import io
import itertools
import os
import selectors
import signal
import socket
import stat
import subprocess
import sys
import threading
import warnings

from . import base_events
from . import base_subprocess
from . import constants
from . import coroutines
from . import events
from . import exceptions
from . import futures
from . import selector_events
from . import tasks
from . import transports
from .log import logger


__all__ = (
    'SelectorEventLoop',
    'EventLoop',
)


if sys.platform == 'win32':  # pragma: no cover
    raise ImportError('Signals are not really supported on Windows')


def _sighandler_noop(signum, frame):
    """Python-level handler; the wakeup fd carries the signal to the loop."""


def waitstatus_to_exitcode(status):
    try:
        return os.waitstatus_to_exitcode(status)
    except ValueError:
        # stopped/continued statuses: keep the raw value
        return status


class _UnixSelectorEventLoop(selector_events.BaseSelectorEventLoop):
    """Unix event loop: adds signal handlers and process support."""

    def __init__(self, selector=None):
        super().__init__(selector)
        self._signal_handlers = {}
        self._unix_server_sockets = {}
        if can_use_pidfd():
            self._watcher = _PidfdChildWatcher()
        else:
            self._watcher = _ThreadedChildWatcher()

    def close(self):
        super().close()
        if not sys.is_finalizing():
            for sig in list(self._signal_handlers):
                self.remove_signal_handler(sig)
        elif self._signal_handlers:
            warnings.warn(f"Closing the loop {self!r} on interpreter shutdown "
                          f"stage, skipping signal handlers removal",
                          ResourceWarning, source=self)
            self._signal_handlers.clear()

    # ------------------------------------------------------------ signals

    def _process_self_data(self, data):
        # the wakeup fd writes one byte per received signal
        for signum in data:
            if signum:
                self._handle_signal(signum)

    def add_signal_handler(self, sig, callback, *args):
        """Run callback(*args) in the loop whenever signal *sig* arrives."""
        if coroutines.iscoroutine(callback) or \
                coroutines._iscoroutinefunction(callback):
            raise TypeError("coroutines cannot be used with "
                            "add_signal_handler()")
        self._check_signal(sig)
        self._check_closed()
        try:
            # only possible from the main thread
            signal.set_wakeup_fd(self._csock.fileno())
        except (ValueError, OSError) as exc:
            raise RuntimeError(str(exc))

        self._signal_handlers[sig] = events.Handle(callback, args, self, None)
        try:
            signal.signal(sig, _sighandler_noop)
            if hasattr(signal, 'siginterrupt'):
                # restart interrupted system calls
                signal.siginterrupt(sig, False)
        except OSError as exc:
            del self._signal_handlers[sig]
            if not self._signal_handlers:
                try:
                    signal.set_wakeup_fd(-1)
                except (ValueError, OSError) as nexc:
                    logger.info('set_wakeup_fd(-1) failed: %s', nexc)
            if exc.errno == errno.EINVAL:
                raise RuntimeError(f'sig {sig:d} cannot be caught')
            raise

    def _handle_signal(self, sig):
        handle = self._signal_handlers.get(sig)
        if handle is None:
            return
        if handle._cancelled:
            self.remove_signal_handler(sig)
        else:
            self._add_callback_signalsafe(handle)

    def remove_signal_handler(self, sig):
        """Remove the handler of *sig*; True if there was one."""
        self._check_signal(sig)
        if self._signal_handlers.pop(sig, None) is None:
            return False
        default = signal.default_int_handler if sig == signal.SIGINT \
            else signal.SIG_DFL
        try:
            signal.signal(sig, default)
        except OSError as exc:
            if exc.errno == errno.EINVAL:
                raise RuntimeError(f'sig {sig:d} cannot be caught')
            raise
        if not self._signal_handlers:
            try:
                signal.set_wakeup_fd(-1)
            except (ValueError, OSError) as exc:
                logger.info('set_wakeup_fd(-1) failed: %s', exc)
        return True

    def _check_signal(self, sig):
        if not isinstance(sig, int):
            raise TypeError(f'sig must be an int, not {sig!r}')
        if sig not in signal.valid_signals():
            raise ValueError(f'invalid signal number {sig}')

    # ------------------------------------------------------------ pipes

    def _make_read_pipe_transport(self, pipe, protocol, waiter=None,
                                  extra=None):
        return _UnixReadPipeTransport(self, pipe, protocol, waiter, extra)

    def _make_write_pipe_transport(self, pipe, protocol, waiter=None,
                                   extra=None):
        return _UnixWritePipeTransport(self, pipe, protocol, waiter, extra)

    # ------------------------------------------------------------ processes

    async def _make_subprocess_transport(self, protocol, args, shell,
                                         stdin, stdout, stderr, bufsize,
                                         extra=None, **kwargs):
        waiter = self.create_future()
        transp = _UnixSubprocessTransport(self, protocol, args, shell,
                                          stdin, stdout, stderr, bufsize,
                                          waiter=waiter, extra=extra,
                                          **kwargs)
        self._watcher.add_child_handler(transp.get_pid(),
                                        self._child_watcher_callback, transp)
        try:
            await waiter
        except (SystemExit, KeyboardInterrupt):
            raise
        except BaseException:
            transp.close()
            await transp._wait()
            raise
        return transp

    def _child_watcher_callback(self, pid, returncode, transp):
        # Runs in the loop thread right after the child was reaped.
        if returncode is None:
            # somebody else (Popen.poll) reaped it first
            known = transp._proc.returncode if transp._proc else None
            returncode = 255 if known is None else known
        if transp._returncode is None:
            transp._process_exited(returncode)

    # ------------------------------------------------------------ AF_UNIX

    async def create_unix_connection(
            self, protocol_factory, path=None, *,
            ssl=None, sock=None,
            server_hostname=None,
            ssl_handshake_timeout=None,
            ssl_shutdown_timeout=None):
        assert server_hostname is None or isinstance(server_hostname, str)
        if ssl:
            if server_hostname is None:
                raise ValueError(
                    'you have to pass server_hostname when using ssl')
        else:
            if server_hostname is not None:
                raise ValueError('server_hostname is only meaningful with ssl')
            if ssl_handshake_timeout is not None:
                raise ValueError(
                    'ssl_handshake_timeout is only meaningful with ssl')
            if ssl_shutdown_timeout is not None:
                raise ValueError(
                    'ssl_shutdown_timeout is only meaningful with ssl')

        if path is not None:
            if sock is not None:
                raise ValueError(
                    'path and sock can not be specified at the same time')
            path = os.fspath(path)
            sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM, 0)
            try:
                sock.setblocking(False)
                await self.sock_connect(sock, path)
            except:
                sock.close()
                raise
        else:
            if sock is None:
                raise ValueError('no path and sock were specified')
            if (sock.family != socket.AF_UNIX or
                    sock.type != socket.SOCK_STREAM):
                raise ValueError(
                    f'A UNIX Domain Stream Socket was expected, got {sock!r}')
            sock.setblocking(False)

        return await self._create_connection_transport(
            sock, protocol_factory, ssl, server_hostname,
            ssl_handshake_timeout=ssl_handshake_timeout,
            ssl_shutdown_timeout=ssl_shutdown_timeout)

    async def create_unix_server(
            self, protocol_factory, path=None, *,
            sock=None, backlog=100, ssl=None,
            ssl_handshake_timeout=None,
            ssl_shutdown_timeout=None,
            start_serving=True, cleanup_socket=True):
        if isinstance(ssl, bool):
            raise TypeError('ssl argument must be an SSLContext or None')
        if ssl_handshake_timeout is not None and not ssl:
            raise ValueError(
                'ssl_handshake_timeout is only meaningful with ssl')
        if ssl_shutdown_timeout is not None and not ssl:
            raise ValueError(
                'ssl_shutdown_timeout is only meaningful with ssl')

        if path is not None:
            if sock is not None:
                raise ValueError(
                    'path and sock can not be specified at the same time')
            path = os.fspath(path)
            sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            # A stale socket file left by a dead server is removed, but a
            # regular file is never touched.
            if path[0] not in (0, '\x00'):
                try:
                    if stat.S_ISSOCK(os.stat(path).st_mode):
                        os.remove(path)
                except FileNotFoundError:
                    pass
                except OSError as err:
                    logger.error('Unable to check or remove stale UNIX socket '
                                 '%r: %r', path, err)
            try:
                sock.bind(path)
            except OSError as exc:
                sock.close()
                if exc.errno == errno.EADDRINUSE:
                    raise OSError(errno.EADDRINUSE,
                                  f'Address {path!r} is already in use') \
                        from None
                raise
            except:
                sock.close()
                raise
        else:
            if sock is None:
                raise ValueError('path was not specified, and no sock '
                                 'specified')
            if (sock.family != socket.AF_UNIX or
                    sock.type != socket.SOCK_STREAM):
                raise ValueError(
                    f'A UNIX Domain Stream Socket was expected, got {sock!r}')

        if cleanup_socket:
            path = sock.getsockname()
            # abstract-namespace addresses are bytes/empty: nothing to clean
            if isinstance(path, str) and path and path[0] != '\x00':
                try:
                    self._unix_server_sockets[sock] = os.stat(path).st_ino
                except FileNotFoundError:
                    pass

        sock.setblocking(False)
        server = base_events.Server(self, [sock], protocol_factory, ssl,
                                    backlog, ssl_handshake_timeout,
                                    ssl_shutdown_timeout)
        if start_serving:
            server._start_serving()
            await tasks.sleep(0)
        return server

    def _stop_serving(self, sock):
        inode = self._unix_server_sockets.pop(sock, None)
        path = None
        if inode is not None:
            try:
                path = sock.getsockname()
            except OSError:
                path = None
        super()._stop_serving(sock)
        if path:
            try:
                if os.stat(path).st_ino == inode:
                    os.unlink(path)
            except FileNotFoundError:
                pass
            except OSError as err:
                logger.error('Unable to clean up listening UNIX socket '
                             '%r: %r', path, err)

    # ------------------------------------------------------------ sendfile

    async def _sock_sendfile_native(self, sock, file, offset, count):
        try:
            os.sendfile
        except AttributeError:
            raise exceptions.SendfileNotAvailableError(
                "os.sendfile() is not available")
        try:
            fileno = file.fileno()
        except (AttributeError, io.UnsupportedOperation) as err:
            raise exceptions.SendfileNotAvailableError("not a regular file")
        try:
            fsize = os.fstat(fileno).st_size
        except OSError:
            raise exceptions.SendfileNotAvailableError("not a regular file")
        blocksize = count if count else fsize
        if not blocksize:
            return 0   # empty file

        fut = self.create_future()
        self._sock_sendfile_native_impl(fut, None, sock, fileno,
                                        offset, count, blocksize, 0)
        return await fut

    def _sock_sendfile_native_impl(self, fut, registered_fd, sock, fileno,
                                   offset, count, blocksize, total_sent):
        fd = sock.fileno()
        if registered_fd is not None:
            # unregister first: the socket may be closed by the time the
            # future callbacks run
            self.remove_writer(registered_fd)
        if fut.cancelled():
            self._sock_sendfile_update_filepos(fileno, offset, total_sent)
            return
        if count:
            blocksize = count - total_sent
            if blocksize <= 0:
                self._sock_sendfile_update_filepos(fileno, offset, total_sent)
                fut.set_result(total_sent)
                return
        # cap one call at 1 GiB to avoid overflows
        blocksize = min(blocksize, 2 ** 30)

        try:
            sent = os.sendfile(fd, fileno, offset, blocksize)
        except (BlockingIOError, InterruptedError):
            if registered_fd is None:
                self._sock_add_cancellation_callback(fut, sock)
            self.add_writer(fd, self._sock_sendfile_native_impl, fut,
                            fd, sock, fileno, offset, count, blocksize,
                            total_sent)
        except OSError as exc:
            if (registered_fd is not None and exc.errno == errno.ENOTCONN and
                    type(exc) is not ConnectionError):
                # the peer went away while we waited for writability
                new_exc = ConnectionError("socket is not connected",
                                          errno.ENOTCONN)
                new_exc.__cause__ = exc
                exc = new_exc
            if total_sent == 0:
                # nothing sent: the fallback can still do the job
                err = exceptions.SendfileNotAvailableError(
                    "os.sendfile call failed")
                self._sock_sendfile_update_filepos(fileno, offset, total_sent)
                fut.set_exception(err)
            else:
                self._sock_sendfile_update_filepos(fileno, offset, total_sent)
                fut.set_exception(exc)
        except (SystemExit, KeyboardInterrupt):
            raise
        except BaseException as exc:
            self._sock_sendfile_update_filepos(fileno, offset, total_sent)
            fut.set_exception(exc)
        else:
            if sent == 0:
                # end of file
                self._sock_sendfile_update_filepos(fileno, offset, total_sent)
                fut.set_result(total_sent)
            else:
                offset += sent
                total_sent += sent
                if registered_fd is None:
                    self._sock_add_cancellation_callback(fut, sock)
                self.add_writer(fd, self._sock_sendfile_native_impl, fut,
                                fd, sock, fileno, offset, count, blocksize,
                                total_sent)

    def _sock_sendfile_update_filepos(self, fileno, offset, total_sent):
        # the file position ends after the data sent (even if none was)
        os.lseek(fileno, offset, os.SEEK_SET)

    def _sock_add_cancellation_callback(self, fut, sock):
        def cb(fut):
            if fut.cancelled():
                fd = sock.fileno()
                if fd != -1:
                    self.remove_writer(fd)
        fut.add_done_callback(cb)


class _UnixReadPipeTransport(transports.ReadTransport):

    max_size = 256 * 1024   # bytes per read

    def __init__(self, loop, pipe, protocol, waiter=None, extra=None):
        super().__init__(extra)
        self._extra['pipe'] = pipe
        self._loop = loop
        self._pipe = pipe
        self._fileno = pipe.fileno()
        self._protocol = protocol
        self._closing = False
        self._paused = False

        mode = os.fstat(self._fileno).st_mode
        if not (stat.S_ISFIFO(mode) or stat.S_ISSOCK(mode) or
                stat.S_ISCHR(mode)):
            self._pipe = self._fileno = self._protocol = None
            raise ValueError("Pipe transport is for pipes/sockets only.")

        os.set_blocking(self._fileno, False)
        self._loop.call_soon(self._protocol.connection_made, self)
        self._loop.call_soon(self._add_reader, self._fileno, self._read_ready)
        if waiter is not None:
            self._loop.call_soon(futures._set_result_unless_cancelled,
                                 waiter, None)

    def _add_reader(self, fd, callback):
        if self.is_reading():
            self._loop._add_reader(fd, callback)

    def is_reading(self):
        return not self._paused and not self._closing

    def __repr__(self):
        words = [type(self).__name__]
        if self._pipe is None:
            words.append('closed')
        elif self._closing:
            words.append('closing')
        words.append(f'fd={self._fileno}')
        selector = getattr(self._loop, '_selector', None)
        if self._pipe is not None and selector is not None:
            polling = selector_events._test_selector_event(
                selector, self._fileno, selectors.EVENT_READ)
            words.append('polling' if polling else 'idle')
        elif self._pipe is not None:
            words.append('open')
        else:
            words.append('closed')
        return '<{}>'.format(' '.join(words))

    def _read_ready(self):
        try:
            data = os.read(self._fileno, self.max_size)
        except (BlockingIOError, InterruptedError):
            return
        except OSError as exc:
            self._fatal_error(exc, 'Fatal read error on pipe transport')
            return
        if data:
            self._protocol.data_received(data)
            return
        if self._loop.get_debug():
            logger.info("%r was closed by peer", self)
        self._closing = True
        self._loop._remove_reader(self._fileno)
        self._loop.call_soon(self._protocol.eof_received)
        self._loop.call_soon(self._call_connection_lost, None)

    def pause_reading(self):
        if not self.is_reading():
            return
        self._paused = True
        self._loop._remove_reader(self._fileno)
        if self._loop.get_debug():
            logger.debug("%r pauses reading", self)

    def resume_reading(self):
        if self._closing or not self._paused:
            return
        self._paused = False
        self._loop._add_reader(self._fileno, self._read_ready)
        if self._loop.get_debug():
            logger.debug("%r resumes reading", self)

    def set_protocol(self, protocol):
        self._protocol = protocol

    def get_protocol(self):
        return self._protocol

    def is_closing(self):
        return self._closing

    def close(self):
        if not self._closing:
            self._close(None)

    def __del__(self, _warn=warnings.warn):
        if self._pipe is not None:
            _warn(f"unclosed transport {self!r}", ResourceWarning, source=self)
            self._pipe.close()

    def _fatal_error(self, exc, message='Fatal error on pipe transport'):
        # EIO is what a pty reports once the other side is gone
        if isinstance(exc, OSError) and exc.errno == errno.EIO:
            if self._loop.get_debug():
                logger.debug("%r: %s", self, message, exc_info=True)
        else:
            self._loop.call_exception_handler({
                'message': message,
                'exception': exc,
                'transport': self,
                'protocol': self._protocol,
            })
        self._close(exc)

    def _close(self, exc):
        self._closing = True
        self._loop._remove_reader(self._fileno)
        self._loop.call_soon(self._call_connection_lost, exc)

    def _call_connection_lost(self, exc):
        try:
            self._protocol.connection_lost(exc)
        finally:
            self._pipe.close()
            self._pipe = None
            self._protocol = None
            self._loop = None


class _UnixWritePipeTransport(transports._FlowControlMixin,
                              transports.WriteTransport):

    def __init__(self, loop, pipe, protocol, waiter=None, extra=None):
        super().__init__(extra, loop)
        self._extra['pipe'] = pipe
        self._pipe = pipe
        self._fileno = pipe.fileno()
        self._protocol = protocol
        self._buffer = bytearray()
        self._conn_lost = 0
        self._closing = False   # set by close() and write_eof()

        mode = os.fstat(self._fileno).st_mode
        is_char = stat.S_ISCHR(mode)
        is_fifo = stat.S_ISFIFO(mode)
        is_socket = stat.S_ISSOCK(mode)
        if not (is_char or is_fifo or is_socket):
            self._pipe = self._fileno = self._protocol = None
            raise ValueError("Pipe transport is only for "
                             "pipes, sockets and character devices")

        os.set_blocking(self._fileno, False)
        self._loop.call_soon(self._protocol.connection_made, self)

        # A readable write end means the reader closed it: watch for that
        # (not possible for character devices).
        if is_socket or (is_fifo and not sys.platform.startswith("aix")):
            self._loop.call_soon(self._loop._add_reader,
                                 self._fileno, self._read_ready)
        if waiter is not None:
            self._loop.call_soon(futures._set_result_unless_cancelled,
                                 waiter, None)

    def __repr__(self):
        words = [type(self).__name__]
        if self._pipe is None:
            words.append('closed')
        elif self._closing:
            words.append('closing')
        words.append(f'fd={self._fileno}')
        selector = getattr(self._loop, '_selector', None)
        if self._pipe is not None and selector is not None:
            polling = selector_events._test_selector_event(
                selector, self._fileno, selectors.EVENT_WRITE)
            words.append('polling' if polling else 'idle')
            words.append(f'bufsize={self.get_write_buffer_size()}')
        elif self._pipe is not None:
            words.append('open')
        else:
            words.append('closed')
        return '<{}>'.format(' '.join(words))

    def get_write_buffer_size(self):
        return len(self._buffer)

    def _read_ready(self):
        # the reading end was closed
        if self._loop.get_debug():
            logger.info("%r was closed by peer", self)
        if self._buffer:
            self._close(BrokenPipeError())
        else:
            self._close()

    def write(self, data):
        assert isinstance(data, (bytes, bytearray, memoryview)), repr(data)
        if isinstance(data, bytearray):
            data = memoryview(data)
        if not data:
            return
        if self._conn_lost or self._closing:
            if self._conn_lost >= constants.LOG_THRESHOLD_FOR_CONNLOST_WRITES:
                logger.warning('pipe closed by peer or '
                               'os.write(pipe, data) raised exception.')
            self._conn_lost += 1
            return

        if not self._buffer:
            try:
                written = os.write(self._fileno, data)
            except (BlockingIOError, InterruptedError):
                written = 0
            except (SystemExit, KeyboardInterrupt):
                raise
            except BaseException as exc:
                self._conn_lost += 1
                self._fatal_error(exc, 'Fatal write error on pipe transport')
                return
            if written == len(data):
                return
            if written > 0:
                data = memoryview(data)[written:]
            self._loop._add_writer(self._fileno, self._write_ready)

        self._buffer += data
        self._maybe_pause_protocol()

    def _write_ready(self):
        assert self._buffer, 'Data should not be empty'
        try:
            written = os.write(self._fileno, self._buffer)
        except (BlockingIOError, InterruptedError):
            return
        except (SystemExit, KeyboardInterrupt):
            raise
        except BaseException as exc:
            self._buffer.clear()
            self._conn_lost += 1
            # The writer is removed before _fatal_error(): it may defer
            # the actual close.
            self._loop._remove_writer(self._fileno)
            self._fatal_error(exc, 'Fatal write error on pipe transport')
            return
        if written == len(self._buffer):
            self._buffer.clear()
            self._loop._remove_writer(self._fileno)
            self._maybe_resume_protocol()
            if self._closing:
                self._loop._remove_reader(self._fileno)
                self._call_connection_lost(None)
        elif written > 0:
            del self._buffer[:written]

    def can_write_eof(self):
        return True

    def write_eof(self):
        if self._closing:
            return
        assert self._pipe
        self._closing = True
        if not self._buffer:
            self._loop._remove_reader(self._fileno)
            self._loop.call_soon(self._call_connection_lost, None)

    def set_protocol(self, protocol):
        self._protocol = protocol

    def get_protocol(self):
        return self._protocol

    def is_closing(self):
        return self._closing

    def close(self):
        if self._pipe is not None and not self._closing:
            # flushes what is buffered, then closes
            self.write_eof()

    def __del__(self, _warn=warnings.warn):
        if self._pipe is not None:
            _warn(f"unclosed transport {self!r}", ResourceWarning, source=self)
            self._pipe.close()

    def abort(self):
        self._close(None)

    def _fatal_error(self, exc, message='Fatal error on pipe transport'):
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
        self._close(exc)

    def _close(self, exc=None):
        self._closing = True
        if self._buffer:
            self._loop._remove_writer(self._fileno)
        self._buffer.clear()
        self._loop._remove_reader(self._fileno)
        self._loop.call_soon(self._call_connection_lost, exc)

    def _call_connection_lost(self, exc):
        try:
            self._protocol.connection_lost(exc)
        finally:
            self._pipe.close()
            self._pipe = None
            self._protocol = None
            self._loop = None


class _UnixSubprocessTransport(base_subprocess.BaseSubprocessTransport):

    def _start(self, args, shell, stdin, stdout, stderr, bufsize, **kwargs):
        stdin_w = None
        if stdin == subprocess.PIPE and sys.platform.startswith('aix'):
            # AIX pipes don't report the reader closing: use a socketpair
            stdin, stdin_w = socket.socketpair()
        try:
            self._proc = subprocess.Popen(
                args, shell=shell, stdin=stdin, stdout=stdout, stderr=stderr,
                universal_newlines=False, bufsize=bufsize, **kwargs)
            if stdin_w is not None:
                stdin.close()
                self._proc.stdin = open(stdin_w.detach(), 'wb',
                                        buffering=bufsize)
                stdin_w = None
        finally:
            if stdin_w is not None:
                stdin.close()
                stdin_w.close()


def can_use_pidfd():
    """True if pidfd_open() works on this system."""
    if not hasattr(os, 'pidfd_open'):
        return False
    try:
        pid = os.getpid()
        os.close(os.pidfd_open(pid, 0))
    except OSError:
        return False
    return True


def _reap(pid):
    """waitpid() the (exited) child; None if it was reaped elsewhere."""
    try:
        _, status = os.waitpid(pid, 0)
    except ChildProcessError:
        return None
    return waitstatus_to_exitcode(status)


class _PidfdChildWatcher:
    """Waits for children through pidfds registered with the event loop.

    The child is reaped in the loop thread when its pidfd becomes readable,
    so the exit is published at the moment the pid is released.
    """

    def add_child_handler(self, pid, callback, *args):
        loop = events.get_running_loop()
        pidfd = os.pidfd_open(pid)
        loop._add_reader(pidfd, self._do_wait, pid, pidfd, callback, args)

    def _do_wait(self, pid, pidfd, callback, args):
        loop = events.get_running_loop()
        loop._remove_reader(pidfd)
        try:
            returncode = _reap(pid)
        finally:
            os.close(pidfd)
        callback(pid, returncode, *args)


class _ThreadedChildWatcher:
    """Waits for children in helper threads.

    Each thread only observes the exit (waitid with WNOWAIT); the child is
    reaped later in the loop thread, so its pid cannot be recycled while
    asyncio may still signal it.
    """

    def __init__(self):
        self._pid_counter = itertools.count(0)
        self._threads = {}

    def __del__(self, _warn=warnings.warn):
        alive = [t for t in list(self._threads.values()) if t.is_alive()]
        if alive:
            _warn(f"{self.__class__} has registered but not finished child "
                  f"processes", ResourceWarning, source=self)

    def add_child_handler(self, pid, callback, *args):
        loop = events.get_running_loop()
        thread = threading.Thread(
            target=self._do_waitpid,
            name=f"asyncio-waitpid-{next(self._pid_counter)}",
            args=(loop, pid, callback, args),
            daemon=True)
        self._threads[pid] = thread
        thread.start()

    def _wait_exit(self, pid):
        """Block until *pid* terminated (not merely stopped)."""
        exited = {getattr(os, name) for name in
                  ('CLD_EXITED', 'CLD_KILLED', 'CLD_DUMPED')
                  if hasattr(os, name)}
        while True:
            try:
                info = os.waitid(os.P_PID, pid, os.WEXITED | os.WNOWAIT)
            except InterruptedError:
                continue
            except ChildProcessError:
                return
            if info is None or not exited or info.si_code in exited:
                return

    def _do_waitpid(self, loop, pid, callback, args):
        try:
            if hasattr(os, 'waitid') and hasattr(os, 'WNOWAIT'):
                self._wait_exit(pid)
                deliver = (self._finish, pid, callback, args)
            else:
                returncode = _reap(pid)
                deliver = (callback, pid, returncode, *args)
            if loop.is_closed():
                logger.warning("Loop %r that handles pid %r is closed",
                               loop, pid)
            else:
                loop.call_soon_threadsafe(*deliver)
        finally:
            self._threads.pop(pid, None)

    def _finish(self, pid, callback, args):
        callback(pid, _reap(pid), *args)


class _UnixDefaultEventLoopPolicy(events._BaseDefaultEventLoopPolicy):
    """UNIX event loop policy"""
    _loop_factory = _UnixSelectorEventLoop


SelectorEventLoop = _UnixSelectorEventLoop
_DefaultEventLoopPolicy = _UnixDefaultEventLoopPolicy
EventLoop = SelectorEventLoop
