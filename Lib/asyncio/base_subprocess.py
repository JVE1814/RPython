"""Subprocess transport machinery shared by the event loop flavours."""

import collections
import subprocess
import warnings

from . import protocols
from . import transports
from .log import logger


class BaseSubprocessTransport(transports.SubprocessTransport):
    """Transport for a child process; its pipes get their own pipe
    transports, whose events are forwarded to the subprocess protocol."""

    def __init__(self, loop, protocol, args, shell,
                 stdin, stdout, stderr, bufsize,
                 waiter=None, extra=None, **kwargs):
        super().__init__(extra)
        self._closed = False
        self._protocol = protocol
        self._loop = loop
        self._proc = None
        self._pid = None
        self._returncode = None
        self._exit_waiters = []
        # protocol callbacks issued before connection_made() wait here
        self._pending_calls = collections.deque()
        self._pipes = {}     # child fd -> pipe protocol (None until wired)
        self._finished = False
        for fd, spec in ((0, stdin), (1, stdout), (2, stderr)):
            if spec == subprocess.PIPE:
                self._pipes[fd] = None
        try:
            self._start(args=args, shell=shell, stdin=stdin, stdout=stdout,
                        stderr=stderr, bufsize=bufsize, **kwargs)
        except:
            self.close()
            raise
        self._pid = self._proc.pid
        self._extra['subprocess'] = self._proc
        if self._loop.get_debug():
            kind = 'shell' if shell else 'exec'
            logger.debug('process %r created: pid %s', args, self._pid)
            del kind
        self._loop.create_task(self._connect_pipes(waiter))

    def __repr__(self):
        words = [type(self).__name__]
        if self._closed:
            words.append('closed')
        if self._pid is not None:
            words.append(f'pid={self._pid}')
        if self._returncode is not None:
            words.append(f'returncode={self._returncode}')
        elif self._pid is not None:
            words.append('running')
        else:
            words.append('not started')
        for fd, name in ((0, 'stdin'), (1, 'stdout'), (2, 'stderr')):
            proto = self._pipes.get(fd)
            if proto is not None:
                words.append(f'{name}={proto.pipe}')
        return '<{}>'.format(' '.join(words))

    def _start(self, args, shell, stdin, stdout, stderr, bufsize, **kwargs):
        raise NotImplementedError

    def set_protocol(self, protocol):
        self._protocol = protocol

    def get_protocol(self):
        return self._protocol

    def is_closing(self):
        return self._closed

    def close(self):
        if self._closed:
            return
        self._closed = True
        for proto in self._pipes.values():
            if proto is not None and proto.pipe is not None:
                proto.pipe.close()
        if (self._proc is not None and self._returncode is None
                and self._proc.poll() is None):
            if self._loop.get_debug():
                logger.warning('Close running child process: kill %r', self)
            try:
                self._proc.kill()
            except (ProcessLookupError, PermissionError):
                pass

    def __del__(self, _warn=warnings.warn):
        if not self._closed:
            _warn(f"unclosed transport {self!r}", ResourceWarning, source=self)
            self.close()

    def get_pid(self):
        return self._pid

    def get_returncode(self):
        return self._returncode

    def get_pipe_transport(self, fd):
        proto = self._pipes.get(fd)
        return None if proto is None else proto.pipe

    def _check_proc(self):
        if self._proc is None:
            raise ProcessLookupError()

    def send_signal(self, signal):
        self._check_proc()
        self._proc.send_signal(signal)

    def terminate(self):
        self._check_proc()
        self._proc.terminate()

    def kill(self):
        self._check_proc()
        self._proc.kill()

    async def _connect_pipes(self, waiter):
        try:
            proc, loop = self._proc, self._loop
            wiring = ((0, proc.stdin, loop.connect_write_pipe,
                       WriteSubprocessPipeProto),
                      (1, proc.stdout, loop.connect_read_pipe,
                       ReadSubprocessPipeProto),
                      (2, proc.stderr, loop.connect_read_pipe,
                       ReadSubprocessPipeProto))
            for fd, stream, connect, factory in wiring:
                if stream is None:
                    continue
                _, pipe_proto = await connect(
                    lambda fd=fd, factory=factory: factory(self, fd), stream)
                self._pipes[fd] = pipe_proto
            assert self._pending_calls is not None
            loop.call_soon(self._protocol.connection_made, self)
            for callback, args in self._pending_calls:
                loop.call_soon(callback, *args)
            self._pending_calls = None
        except (SystemExit, KeyboardInterrupt):
            raise
        except BaseException as exc:
            # the protocol never saw connection_made(): drop its queued
            # calls and the pipes that were not connected, so that the
            # transport can still finish (and wake _wait()) once the
            # child is gone
            self._setup_failed = True
            self._pending_calls = None
            for fd in [fd for fd, p in self._pipes.items() if p is None]:
                del self._pipes[fd]
            if waiter is not None and not waiter.cancelled():
                waiter.set_exception(exc)
        else:
            if waiter is not None and not waiter.cancelled():
                waiter.set_result(None)

    def _call(self, cb, *data):
        if self._pending_calls is not None:
            self._pending_calls.append((cb, data))
        else:
            self._loop.call_soon(cb, *data)

    def _pipe_connection_lost(self, fd, exc):
        self._call(self._protocol.pipe_connection_lost, fd, exc)
        self._try_finish()

    def _pipe_data_received(self, fd, data):
        self._call(self._protocol.pipe_data_received, fd, data)

    def _process_exited(self, returncode):
        assert returncode is not None, returncode
        assert self._returncode is None, self._returncode
        if self._loop.get_debug():
            logger.info('%r exited with return code %r', self, returncode)
        self._returncode = returncode
        if self._proc.returncode is None:
            # keep Popen consistent (we reaped the child ourselves)
            self._proc.returncode = returncode
        self._call(self._protocol.process_exited)
        self._try_finish()
        # wait() returns once the child is gone, even if a pipe stays open
        # (e.g. inherited by a grandchild)
        self._wake_exit_waiters()

    async def _wait(self):
        """Wait until the process exited; return its exit status."""
        if self._returncode is not None:
            return self._returncode
        waiter = self._loop.create_future()
        self._exit_waiters.append(waiter)
        return await waiter

    def _try_finish(self):
        assert not self._finished
        if self._returncode is None:
            return
        if all(p is not None and p.disconnected
               for p in self._pipes.values()):
            self._finished = True
            self._call(self._call_connection_lost, None)

    def _wake_exit_waiters(self):
        waiters, self._exit_waiters = self._exit_waiters, []
        for waiter in waiters or ():
            if not waiter.cancelled():
                waiter.set_result(self._returncode)

    _setup_failed = False

    def _call_connection_lost(self, exc):
        try:
            if not self._setup_failed:
                self._protocol.connection_lost(exc)
        finally:
            self._wake_exit_waiters()
            self._loop = None
            self._proc = None
            self._protocol = None


class WriteSubprocessPipeProto(protocols.BaseProtocol):
    """Protocol of the child's stdin pipe."""

    def __init__(self, proc, fd):
        self.proc = proc
        self.fd = fd
        self.pipe = None
        self.disconnected = False

    def connection_made(self, transport):
        self.pipe = transport

    def __repr__(self):
        return f'<{type(self).__name__} fd={self.fd} pipe={self.pipe!r}>'

    def connection_lost(self, exc):
        self.disconnected = True
        self.proc._pipe_connection_lost(self.fd, exc)
        self.proc = None

    def pause_writing(self):
        self.proc._protocol.pause_writing()

    def resume_writing(self):
        self.proc._protocol.resume_writing()


class ReadSubprocessPipeProto(WriteSubprocessPipeProto, protocols.Protocol):
    """Protocol of the child's stdout/stderr pipes."""

    def data_received(self, data):
        self.proc._pipe_data_received(self.fd, data)
