"""Event loop interface, callback handles and the (deprecated) policies."""

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


# ---------------------------------------------------------------- handles

class Handle:
    """A scheduled callback; returned by loop.call_soon() and friends."""

    __slots__ = ('_callback', '_args', '_cancelled', '_loop',
                 '_source_traceback', '_repr', '__weakref__', '_context')

    def __init__(self, callback, args, loop, context=None):
        self._loop = loop
        self._callback = callback
        self._args = args
        self._cancelled = False
        self._repr = None
        self._context = (contextvars.copy_context() if context is None
                         else context)
        self._source_traceback = (
            format_helpers.extract_stack(sys._getframe(1))
            if loop.get_debug() else None)

    def _repr_info(self):
        info = [type(self).__name__]
        if self._cancelled:
            info.append('cancelled')
        if self._callback is not None:
            info.append(format_helpers._format_callback_source(
                self._callback, self._args, debug=self._loop.get_debug()))
        if self._source_traceback:
            last = self._source_traceback[-1]
            info.append(f'created at {last[0]}:{last[1]}')
        return info

    def __repr__(self):
        if self._repr is None:
            return '<' + ' '.join(self._repr_info()) + '>'
        return self._repr

    def get_context(self):
        return self._context

    def cancelled(self):
        return self._cancelled

    def cancel(self):
        if self._cancelled:
            return
        self._cancelled = True
        if self._loop.get_debug():
            # freeze the description before the callback is dropped
            self._repr = repr(self)
        self._callback = self._args = None

    def _run(self):
        try:
            self._context.run(self._callback, *self._args)
        except (SystemExit, KeyboardInterrupt):
            raise
        except BaseException as exc:
            where = format_helpers._format_callback_source(
                self._callback, self._args, debug=self._loop.get_debug())
            report = {'message': f'Exception in callback {where}',
                      'exception': exc, 'handle': self}
            if self._source_traceback:
                report['source_traceback'] = self._source_traceback
            self._loop.call_exception_handler(report)
            del report, exc
        self = None


class _ThreadSafeHandle(Handle):
    """Handle returned by call_soon_threadsafe(): cancel() and cancelled()
    from another thread wait while the callback is running, so that a
    callback that has started is never reported as cancelled."""

    __slots__ = ('_lock',)

    def __init__(self, callback, args, loop, context=None):
        super().__init__(callback, args, loop, context)
        self._lock = threading.RLock()

    def cancelled(self):
        with self._lock:
            return self._cancelled

    def cancel(self):
        with self._lock:
            if not self._cancelled:
                super().cancel()

    def _run(self):
        # the callback may cancel its own handle (same thread: re-entrant)
        with self._lock:
            super()._run()


class TimerHandle(Handle):
    """A callback scheduled for a point in (loop) time."""

    __slots__ = ['_scheduled', '_when']

    def __init__(self, when, callback, args, loop, context=None):
        super().__init__(callback, args, loop, context)
        if self._source_traceback:
            # drop the frame of call_at()/call_later() itself
            self._source_traceback.pop()
        self._when = when
        self._scheduled = False

    def _repr_info(self):
        info = super()._repr_info()
        info.insert(2 if self._cancelled else 1, f'when={self._when}')
        return info

    def when(self):
        """The scheduled time, in loop.time() units."""
        return self._when

    def __hash__(self):
        return hash(self._when)

    def _same(self, other):
        return (self._when, self._callback, self._args, self._cancelled) == \
            (other._when, other._callback, other._args, other._cancelled)

    def __eq__(self, other):
        if not isinstance(other, TimerHandle):
            return NotImplemented
        return self._same(other)

    def __lt__(self, other):
        if not isinstance(other, TimerHandle):
            return NotImplemented
        return self._when < other._when

    def __gt__(self, other):
        if not isinstance(other, TimerHandle):
            return NotImplemented
        return self._when > other._when

    def __le__(self, other):
        if not isinstance(other, TimerHandle):
            return NotImplemented
        return self._when < other._when or self._same(other)

    def __ge__(self, other):
        if not isinstance(other, TimerHandle):
            return NotImplemented
        return self._when > other._when or self._same(other)

    def cancel(self):
        if not self._cancelled:
            self._loop._timer_handle_cancelled(self)
        super().cancel()


# ---------------------------------------------------------------- interfaces

def _unimplemented(name):
    def method(self, *args, **kwargs):
        raise NotImplementedError
    method.__name__ = name
    return method


def _unimplemented_async(name):
    async def method(self, *args, **kwargs):
        raise NotImplementedError
    method.__name__ = name
    return method


class AbstractServer:
    """Interface of the server objects returned by create_server()."""

    close = _unimplemented('close')
    close_clients = _unimplemented('close_clients')
    abort_clients = _unimplemented('abort_clients')
    get_loop = _unimplemented('get_loop')
    is_serving = _unimplemented('is_serving')
    start_serving = _unimplemented_async('start_serving')
    serve_forever = _unimplemented_async('serve_forever')
    wait_closed = _unimplemented_async('wait_closed')

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc_info):
        self.close()
        await self.wait_closed()


_LOOP_SYNC_METHODS = (
    'run_forever', 'run_until_complete', 'stop', 'is_running', 'is_closed',
    'close', '_timer_handle_cancelled', 'call_later', 'call_at', 'time',
    'create_future', 'create_task', 'call_soon_threadsafe',
    'run_in_executor', 'set_default_executor', 'add_reader',
    'remove_reader', 'add_writer', 'remove_writer', 'add_signal_handler',
    'remove_signal_handler', 'set_task_factory', 'get_task_factory',
    'get_exception_handler', 'set_exception_handler',
    'default_exception_handler', 'call_exception_handler', 'get_debug',
    'set_debug',
)

_LOOP_ASYNC_METHODS = (
    'shutdown_asyncgens', 'shutdown_default_executor', 'getaddrinfo',
    'getnameinfo', 'create_connection', 'create_server', 'sendfile',
    'start_tls', 'create_unix_connection', 'create_unix_server',
    'connect_accepted_socket', 'create_datagram_endpoint',
    'connect_read_pipe', 'connect_write_pipe', 'subprocess_shell',
    'subprocess_exec', 'sock_recv', 'sock_recv_into', 'sock_recvfrom',
    'sock_recvfrom_into', 'sock_sendall', 'sock_sendto', 'sock_connect',
    'sock_accept', 'sock_sendfile',
)


class AbstractEventLoop:
    """Interface of event loops; every method raises NotImplementedError
    except call_soon(), which defaults to call_later(0, ...)."""

    def call_soon(self, callback, *args, context=None):
        return self.call_later(0, callback, *args, context=context)


for _name in _LOOP_SYNC_METHODS:
    setattr(AbstractEventLoop, _name, _unimplemented(_name))
for _name in _LOOP_ASYNC_METHODS:
    setattr(AbstractEventLoop, _name, _unimplemented_async(_name))
del _name


# ---------------------------------------------------------------- policies

class _AbstractEventLoopPolicy:
    """Interface of event loop policies (deprecated mechanism)."""

    def get_event_loop(self):
        raise NotImplementedError

    def set_event_loop(self, loop):
        raise NotImplementedError

    def new_event_loop(self):
        raise NotImplementedError


class _BaseDefaultEventLoopPolicy(_AbstractEventLoopPolicy):
    """Keeps one "current" loop per thread; never creates loops implicitly."""

    _loop_factory = None

    class _Local(threading.local):
        _loop = None

    def __init__(self):
        self._local = self._Local()

    def get_event_loop(self):
        loop = self._local._loop
        if loop is None:
            raise RuntimeError('There is no current event loop in thread %r.'
                               % threading.current_thread().name)
        return loop

    def set_event_loop(self, loop):
        if loop is not None and not isinstance(loop, AbstractEventLoop):
            raise TypeError(f"loop must be an instance of AbstractEventLoop "
                            f"or None, not '{type(loop).__name__}'")
        self._local._loop = loop

    def new_event_loop(self):
        return self._loop_factory()


_event_loop_policy = None
_policy_lock = threading.Lock()


class _RunningLoopSlot(threading.local):
    # (loop, pid): a loop inherited through fork() is not "running" in
    # the child.
    value = (None, None)


_running_slot = _RunningLoopSlot()


def _get_running_loop():
    """The running loop of this thread, or None."""
    loop, pid = _running_slot.value
    if loop is not None and pid == os.getpid():
        return loop
    return None


def _set_running_loop(loop):
    """Declare *loop* as running in this thread (for loop implementations)."""
    _running_slot.value = (loop, os.getpid())


def get_running_loop():
    """Return the running event loop; RuntimeError if there is none."""
    loop = _get_running_loop()
    if loop is None:
        raise RuntimeError('no running event loop')
    return loop


def _get_event_loop_policy():
    global _event_loop_policy
    if _event_loop_policy is None:
        with _policy_lock:
            if _event_loop_policy is None:
                from . import _DefaultEventLoopPolicy
                _event_loop_policy = _DefaultEventLoopPolicy()
    return _event_loop_policy


def _set_event_loop_policy(policy):
    global _event_loop_policy
    if policy is not None and not isinstance(policy, _AbstractEventLoopPolicy):
        raise TypeError(f"policy must be an instance of AbstractEventLoopPolicy "
                        f"or None, not '{type(policy).__name__}'")
    _event_loop_policy = policy


def get_event_loop_policy():
    """Deprecated: return the global event loop policy."""
    warnings._deprecated('asyncio.get_event_loop_policy', remove=(3, 16))
    return _get_event_loop_policy()


def set_event_loop_policy(policy):
    """Deprecated: replace the global event loop policy (None = default)."""
    warnings._deprecated('asyncio.set_event_loop_policy', remove=(3, 16))
    _set_event_loop_policy(policy)


def get_event_loop():
    """The running loop if any, else the current loop of the policy."""
    loop = _get_running_loop()
    if loop is not None:
        return loop
    return _get_event_loop_policy().get_event_loop()


def set_event_loop(loop):
    """Make *loop* the current loop of this thread."""
    _get_event_loop_policy().set_event_loop(loop)


def new_event_loop():
    """Create a new event loop (through the policy)."""
    return _get_event_loop_policy().new_event_loop()


_py__get_running_loop = _get_running_loop
_py__set_running_loop = _set_running_loop
_py_get_running_loop = get_running_loop
_py_get_event_loop = get_event_loop

try:
    import _asyncio as _accel
except ImportError:
    pass
else:
    _c__get_running_loop = _get_running_loop = _accel._get_running_loop
    _c__set_running_loop = _set_running_loop = _accel._set_running_loop
    _c_get_running_loop = get_running_loop = _accel.get_running_loop
    _c_get_event_loop = get_event_loop = _accel.get_event_loop
    del _accel


def _reset_after_fork():
    # The parent's loops are unusable in a forked child.
    if _event_loop_policy is not None:
        _event_loop_policy._local = _BaseDefaultEventLoopPolicy._Local()
    _set_running_loop(None)
    signal.set_wakeup_fd(-1)


if hasattr(os, 'register_at_fork'):
    os.register_at_fork(after_in_child=_reset_after_fork)
