"""Event loop and event loop policy."""

__all__ = (
    "_AbstractEventLoopPolicy",
    "AbstractEventLoop",
    "AbstractServer",
    "Handle",
    "TimerHandle",
    "get_event_loop_policy",
    "set_event_loop_policy",
    "get_event_loop",
    "set_event_loop",
    "new_event_loop",
    "_set_running_loop",
    "get_running_loop",
    "_get_running_loop",
)

import contextvars
import os
import signal
import socket
import subprocess
import sys
import threading
import warnings

from . import format_helpers


class Handle:
    """Object returned by callback registration methods."""

    __slots__ = ('_callback', '_args', '_cancelled', '_loop',
                 '_source_traceback', '_repr', '__weakref__',
                 '_context')

    def __init__(self, callback, args, loop, context=None):
        if context is None:
            context = contextvars.copy_context()
        self._context = context
        self._loop = loop
        self._callback = callback
        self._args = args
        self._cancelled = False
        self._repr = None
        if self._loop.get_debug():
            self._source_traceback = format_helpers.extract_stack(
                sys._getframe(1))
        else:
            self._source_traceback = None

    def _repr_info(self):
        info = [self.__class__.__name__]
        if self._cancelled:
            info.append('cancelled')
        if self._callback is not None:
            info.append(format_helpers._format_callback_source(
                self._callback, self._args,
                debug=self._loop.get_debug()))
        if self._source_traceback:
            frame = self._source_traceback[-1]
            info.append(f'created at {frame[0]}:{frame[1]}')
        return info

    def __repr__(self):
        if self._repr is not None:
            return self._repr
        info = self._repr_info()
        return '<{}>'.format(' '.join(info))

    def get_context(self):
        return self._context

    def cancel(self):
        if not self._cancelled:
            self._cancelled = True
            if self._loop.get_debug():
                # remember what was cancelled for later reprs
                self._repr = repr(self)
            self._callback = None
            self._args = None

    def cancelled(self):
        return self._cancelled

    def _run(self):
        try:
            self._context.run(self._callback, *self._args)
        except (SystemExit, KeyboardInterrupt):
            raise
        except BaseException as exc:
            cb = format_helpers._format_callback_source(
                self._callback, self._args,
                debug=self._loop.get_debug())
            msg = f'Exception in callback {cb}'
            context = {
                'message': msg,
                'exception': exc,
                'handle': self,
            }
            if self._source_traceback:
                context['source_traceback'] = self._source_traceback
            self._loop.call_exception_handler(context)
        self = None  # break reference cycles through the exception


class TimerHandle(Handle):
    """Object returned by timed callback registration methods."""

    __slots__ = ['_scheduled', '_when']

    def __init__(self, when, callback, args, loop, context=None):
        super().__init__(callback, args, loop, context)
        if self._source_traceback:
            del self._source_traceback[-1]
        self._when = when
        self._scheduled = False

    def _repr_info(self):
        info = super()._repr_info()
        pos = 2 if self._cancelled else 1
        info.insert(pos, f'when={self._when}')
        return info

    def __hash__(self):
        return hash(self._when)

    def __lt__(self, other):
        if isinstance(other, TimerHandle):
            return self._when < other._when
        return NotImplemented

    def __le__(self, other):
        if isinstance(other, TimerHandle):
            return self._when < other._when or self.__eq__(other)
        return NotImplemented

    def __gt__(self, other):
        if isinstance(other, TimerHandle):
            return self._when > other._when
        return NotImplemented

    def __ge__(self, other):
        if isinstance(other, TimerHandle):
            return self._when > other._when or self.__eq__(other)
        return NotImplemented

    def __eq__(self, other):
        if isinstance(other, TimerHandle):
            return (self._when == other._when and
                    self._callback == other._callback and
                    self._args == other._args and
                    self._cancelled == other._cancelled)
        return NotImplemented

    def cancel(self):
        if not self._cancelled:
            self._loop._timer_handle_cancelled(self)
        super().cancel()

    def when(self):
        """Return the scheduled time (in loop.time() units) as a float."""
        return self._when


class AbstractServer:
    """Abstract server returned by create_server()."""

    def close(self):
        """Stop serving.  Already accepted connections stay open."""
        raise NotImplementedError

    def close_clients(self):
        """Close all active connections."""
        raise NotImplementedError

    def abort_clients(self):
        """Close all active connections immediately."""
        raise NotImplementedError

    def get_loop(self):
        """Get the event loop the Server object is attached to."""
        raise NotImplementedError

    def is_serving(self):
        """Return True if the server is accepting connections."""
        raise NotImplementedError

    async def start_serving(self):
        """Start accepting connections (idempotent)."""
        raise NotImplementedError

    async def serve_forever(self):
        """Accept connections until cancelled."""
        raise NotImplementedError

    async def wait_closed(self):
        """Wait until the server and its connections are closed."""
        raise NotImplementedError

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc):
        self.close()
        await self.wait_closed()


class AbstractEventLoop:
    """Abstract event loop."""

    # running and stopping

    def run_forever(self):
        raise NotImplementedError

    def run_until_complete(self, future):
        raise NotImplementedError

    def stop(self):
        raise NotImplementedError

    def is_running(self):
        raise NotImplementedError

    def is_closed(self):
        raise NotImplementedError

    def close(self):
        raise NotImplementedError

    async def shutdown_asyncgens(self):
        raise NotImplementedError

    async def shutdown_default_executor(self):
        raise NotImplementedError

    # scheduling

    def _timer_handle_cancelled(self, handle):
        raise NotImplementedError

    def call_soon(self, callback, *args, context=None):
        return self.call_later(0, callback, *args, context=context)

    def call_later(self, delay, callback, *args, context=None):
        raise NotImplementedError

    def call_at(self, when, callback, *args, context=None):
        raise NotImplementedError

    def time(self):
        raise NotImplementedError

    def create_future(self):
        raise NotImplementedError

    def create_task(self, coro, **kwargs):
        raise NotImplementedError

    # threads

    def call_soon_threadsafe(self, callback, *args, context=None):
        raise NotImplementedError

    def run_in_executor(self, executor, func, *args):
        raise NotImplementedError

    def set_default_executor(self, executor):
        raise NotImplementedError

    # network I/O

    async def getaddrinfo(self, host, port, *,
                          family=0, type=0, proto=0, flags=0):
        raise NotImplementedError

    async def getnameinfo(self, sockaddr, flags=0):
        raise NotImplementedError

    async def create_connection(
            self, protocol_factory, host=None, port=None,
            *, ssl=None, family=0, proto=0,
            flags=0, sock=None, local_addr=None,
            server_hostname=None,
            ssl_handshake_timeout=None,
            ssl_shutdown_timeout=None,
            happy_eyeballs_delay=None, interleave=None):
        raise NotImplementedError

    async def create_server(
            self, protocol_factory, host=None, port=None,
            *, family=socket.AF_UNSPEC,
            flags=socket.AI_PASSIVE, sock=None, backlog=100,
            ssl=None, reuse_address=None, reuse_port=None,
            keep_alive=None,
            ssl_handshake_timeout=None,
            ssl_shutdown_timeout=None,
            start_serving=True):
        """Create a TCP server and return a Server object."""
        raise NotImplementedError

    async def sendfile(self, transport, file, offset=0, count=None,
                       *, fallback=True):
        raise NotImplementedError

    async def start_tls(self, transport, protocol, sslcontext, *,
                        server_side=False,
                        server_hostname=None,
                        ssl_handshake_timeout=None,
                        ssl_shutdown_timeout=None):
        raise NotImplementedError

    async def create_unix_connection(
            self, protocol_factory, path=None, *,
            ssl=None, sock=None,
            server_hostname=None,
            ssl_handshake_timeout=None,
            ssl_shutdown_timeout=None):
        raise NotImplementedError

    async def create_unix_server(
            self, protocol_factory, path=None, *,
            sock=None, backlog=100, ssl=None,
            ssl_handshake_timeout=None,
            ssl_shutdown_timeout=None,
            start_serving=True):
        raise NotImplementedError

    async def connect_accepted_socket(
            self, protocol_factory, sock,
            *, ssl=None,
            ssl_handshake_timeout=None,
            ssl_shutdown_timeout=None):
        raise NotImplementedError

    async def create_datagram_endpoint(self, protocol_factory,
                                       local_addr=None, remote_addr=None, *,
                                       family=0, proto=0, flags=0,
                                       reuse_address=None, reuse_port=None,
                                       allow_broadcast=None, sock=None):
        raise NotImplementedError

    # pipes and subprocesses

    async def connect_read_pipe(self, protocol_factory, pipe):
        raise NotImplementedError

    async def connect_write_pipe(self, protocol_factory, pipe):
        raise NotImplementedError

    async def subprocess_shell(self, protocol_factory, cmd, *,
                               stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE,
                               **kwargs):
        raise NotImplementedError

    async def subprocess_exec(self, protocol_factory, *args,
                              stdin=subprocess.PIPE,
                              stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE,
                              **kwargs):
        raise NotImplementedError

    # ready-based callbacks

    def add_reader(self, fd, callback, *args):
        raise NotImplementedError

    def remove_reader(self, fd):
        raise NotImplementedError

    def add_writer(self, fd, callback, *args):
        raise NotImplementedError

    def remove_writer(self, fd):
        raise NotImplementedError

    # completion-based socket operations

    async def sock_recv(self, sock, nbytes):
        raise NotImplementedError

    async def sock_recv_into(self, sock, buf):
        raise NotImplementedError

    async def sock_recvfrom(self, sock, bufsize):
        raise NotImplementedError

    async def sock_recvfrom_into(self, sock, buf, nbytes=0):
        raise NotImplementedError

    async def sock_sendall(self, sock, data):
        raise NotImplementedError

    async def sock_sendto(self, sock, data, address):
        raise NotImplementedError

    async def sock_connect(self, sock, address):
        raise NotImplementedError

    async def sock_accept(self, sock):
        raise NotImplementedError

    async def sock_sendfile(self, sock, file, offset=0, count=None,
                            *, fallback=None):
        raise NotImplementedError

    # signals

    def add_signal_handler(self, sig, callback, *args):
        raise NotImplementedError

    def remove_signal_handler(self, sig):
        raise NotImplementedError

    # task factory

    def set_task_factory(self, factory):
        raise NotImplementedError

    def get_task_factory(self):
        raise NotImplementedError

    # error handling

    def get_exception_handler(self):
        raise NotImplementedError

    def set_exception_handler(self, handler):
        raise NotImplementedError

    def default_exception_handler(self, context):
        raise NotImplementedError

    def call_exception_handler(self, context):
        raise NotImplementedError

    # debug flag

    def get_debug(self):
        raise NotImplementedError

    def set_debug(self, enabled):
        raise NotImplementedError


class _AbstractEventLoopPolicy:
    """Abstract policy for accessing the event loop."""

    def get_event_loop(self):
        """Return the event loop of the current context."""
        raise NotImplementedError

    def set_event_loop(self, loop):
        """Set the event loop of the current context."""
        raise NotImplementedError

    def new_event_loop(self):
        """Create and return a new event loop object."""
        raise NotImplementedError


class _BaseDefaultEventLoopPolicy(_AbstractEventLoopPolicy):
    """Default policy: one event loop per thread, set explicitly with
    set_event_loop(); get_event_loop() never creates a loop."""

    _loop_factory = None

    class _Local(threading.local):
        _loop = None

    def __init__(self):
        self._local = self._Local()

    def get_event_loop(self):
        if self._local._loop is None:
            raise RuntimeError('There is no current event loop in thread %r.'
                               % threading.current_thread().name)
        return self._local._loop

    def set_event_loop(self, loop):
        if loop is not None and not isinstance(loop, AbstractEventLoop):
            raise TypeError(f"loop must be an instance of AbstractEventLoop "
                            f"or None, not '{type(loop).__name__}'")
        self._local._loop = loop

    def new_event_loop(self):
        return self._loop_factory()


# The global policy, created lazily.
_event_loop_policy = None

# Protects the lazy creation of the policy.
_lock = threading.Lock()


class _RunningLoop(threading.local):
    loop_pid = (None, None)


_running_loop = _RunningLoop()


def get_running_loop():
    """Return the running event loop; raise RuntimeError if there is none."""
    loop = _get_running_loop()
    if loop is None:
        raise RuntimeError('no running event loop')
    return loop


def _get_running_loop():
    """Return the running event loop or None (fast, no exception)."""
    running_loop, pid = _running_loop.loop_pid
    if running_loop is not None and pid == os.getpid():
        return running_loop


def _set_running_loop(loop):
    """Set the running event loop (used by loop implementations only)."""
    _running_loop.loop_pid = (loop, os.getpid())


def _init_event_loop_policy():
    global _event_loop_policy
    with _lock:
        if _event_loop_policy is None:
            from . import _DefaultEventLoopPolicy
            _event_loop_policy = _DefaultEventLoopPolicy()


def _get_event_loop_policy():
    """Get the current event loop policy."""
    if _event_loop_policy is None:
        _init_event_loop_policy()
    return _event_loop_policy


def get_event_loop_policy():
    warnings._deprecated('asyncio.get_event_loop_policy', remove=(3, 16))
    return _get_event_loop_policy()


def _set_event_loop_policy(policy):
    """Set the current event loop policy (None restores the default)."""
    global _event_loop_policy
    if policy is not None and not isinstance(policy, _AbstractEventLoopPolicy):
        raise TypeError(f"policy must be an instance of AbstractEventLoopPolicy "
                        f"or None, not '{type(policy).__name__}'")
    _event_loop_policy = policy


def set_event_loop_policy(policy):
    warnings._deprecated('asyncio.set_event_loop_policy', remove=(3, 16))
    _set_event_loop_policy(policy)


def get_event_loop():
    """Return the running loop, or the policy's current loop."""
    current_loop = _get_running_loop()
    if current_loop is not None:
        return current_loop
    return _get_event_loop_policy().get_event_loop()


def set_event_loop(loop):
    """Equivalent to calling get_event_loop_policy().set_event_loop(loop)."""
    _get_event_loop_policy().set_event_loop(loop)


def new_event_loop():
    """Equivalent to calling get_event_loop_policy().new_event_loop()."""
    return _get_event_loop_policy().new_event_loop()


# Pure Python versions, kept for the tests.
_py__get_running_loop = _get_running_loop
_py__set_running_loop = _set_running_loop
_py_get_running_loop = get_running_loop
_py_get_event_loop = get_event_loop


try:
    # C versions from the accelerator module, when available.
    from _asyncio import (_get_running_loop, _set_running_loop,
                          get_running_loop, get_event_loop)
except ImportError:
    pass
else:
    _c__get_running_loop = _get_running_loop
    _c__set_running_loop = _set_running_loop
    _c_get_running_loop = get_running_loop
    _c_get_event_loop = get_event_loop


if hasattr(os, 'fork'):
    def on_fork():
        # the child must not see the parent's loop or wakeup fd
        if _event_loop_policy is not None:
            _event_loop_policy._local = _BaseDefaultEventLoopPolicy._Local()
        _set_running_loop(None)
        signal.set_wakeup_fd(-1)

    os.register_at_fork(after_in_child=on_fork)
