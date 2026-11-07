"""The part of an asyncio event loop that does not depend on how I/O
readiness is detected.

BaseEventLoop owns the ready queue and the timer heap, drives one
iteration at a time (_run_once), runs callbacks in executors, resolves
names, and builds connections, servers, datagram endpoints, pipes and
subprocesses on top of the transport factories (the _make_*_transport
methods) that a concrete loop such as the selector loop provides.  Server
is the object that create_server() hands back.
"""

import collections
import collections.abc
import contextvars
import errno
import functools
import heapq
import itertools
import os
import socket
import stat
import subprocess
import sys
import threading
import time
import traceback
import warnings
import weakref

try:
    import ssl
except ImportError:  # interpreter built without TLS
    ssl = None

from . import constants
from . import coroutines
from . import events
from . import exceptions
from . import futures
from . import protocols
from . import sslproto
from . import staggered
from . import tasks
from . import timeouts
from . import transports
from . import trsock
from .log import logger


__all__ = 'BaseEventLoop', 'Server',


# Timer heap housekeeping: once the heap holds more than this many entries
# and more than this share of them are cancelled, the heap is rebuilt
# without the cancelled ones instead of letting them drift to the top.
_MIN_SCHEDULED_TIMER_HANDLES = 100
_MIN_CANCELLED_TIMER_HANDLES_FRACTION = 0.5

_HAS_IPv6 = hasattr(socket, 'AF_INET6')

# Upper bound for one wait in the selector: some platforms reject very
# long timeouts, and waking up once a day costs nothing.
MAXIMUM_SELECT_TIMEOUT = 24 * 3600


# ---------------------------------------------------------------------------
# Small module-level helpers.  Several of them are used by the concrete
# loops (selector_events, proactor_events) or by the test-suite, so their
# names and signatures are part of the package's internal contract.

def _ipaddr_info(host, port, family, type, proto, flowinfo=0, scopeid=0):
    """Build a getaddrinfo()-style 5-tuple for a numeric address.

    Returns None whenever the real resolver is needed: a host name, a
    service name for the port, a scoped IPv6 address, an unknown socket
    type, or a platform without inet_pton().
    """
    if host is None or not hasattr(socket, 'inet_pton'):
        return None
    implied_proto = {
        socket.SOCK_STREAM: socket.IPPROTO_TCP,
        socket.SOCK_DGRAM: socket.IPPROTO_UDP,
    }
    if type not in implied_proto:
        return None
    proto = implied_proto[type]

    if port in (None, b'', ''):
        port = 0
    else:
        try:
            port = int(port)
        except (TypeError, ValueError):
            return None     # e.g. 'http': only getaddrinfo() knows it

    if isinstance(host, bytes):
        host = host.decode('idna')
    if '%' in host:
        return None         # an interface suffix needs the resolver

    if family != socket.AF_UNSPEC:
        candidates = (family,)
    elif _HAS_IPv6:
        candidates = (socket.AF_INET, socket.AF_INET6)
    else:
        candidates = (socket.AF_INET,)

    for af in candidates:
        try:
            socket.inet_pton(af, host)
        except OSError:
            continue
        if _HAS_IPv6 and af == socket.AF_INET6:
            sockaddr = (host, port, flowinfo, scopeid)
        else:
            sockaddr = (host, port)
        return af, type, proto, '', sockaddr
    return None


def _interleave_addrinfos(addrinfos, first_address_family_count=1):
    """Reorder *addrinfos* so that address families alternate.

    The first family met keeps its order and leads with
    *first_address_family_count* entries; after that one entry of each
    family is taken in turn (RFC 8305 section 4).
    """
    by_family = {}
    for info in addrinfos:
        by_family.setdefault(info[0], collections.deque()).append(info)
    queues = list(by_family.values())
    if not queues:
        return []
    result = []
    lead = queues[0]
    for _ in range(first_address_family_count - 1):
        if not lead:
            break
        result.append(lead.popleft())
    while any(queues):
        for queue in queues:
            if queue:
                result.append(queue.popleft())
    return result


def _set_nodelay(sock):
    """Disable Nagle's algorithm on TCP sockets (no-op for anything else)."""
    if not hasattr(socket, 'TCP_NODELAY'):
        return
    if (sock.family in {socket.AF_INET, socket.AF_INET6}
            and sock.type == socket.SOCK_STREAM
            and sock.proto == socket.IPPROTO_TCP):
        sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)


def _check_ssl_socket(sock):
    """Reject already-wrapped sockets: asyncio does TLS itself."""
    if ssl is not None and isinstance(sock, ssl.SSLSocket):
        raise TypeError("Socket cannot be of type SSLSocket")


def _set_reuseport(sock):
    option = getattr(socket, 'SO_REUSEPORT', None)
    if option is None:
        raise ValueError('reuse_port not supported by socket module')
    try:
        sock.setsockopt(socket.SOL_SOCKET, option, 1)
    except OSError:
        raise ValueError('reuse_port not supported by socket module, '
                         'SO_REUSEPORT defined but not implemented.')


def _close_quietly(sock):
    try:
        sock.close()
    except OSError:
        pass


def _stop_when_done(fut):
    """Done callback used by run_until_complete()."""
    if not fut.cancelled():
        # SystemExit and KeyboardInterrupt already leave run_forever() on
        # their own; a pending stop() would then leak into the next run.
        if isinstance(fut.exception(), (SystemExit, KeyboardInterrupt)):
            return
    futures._get_loop(fut).stop()


def _describe_handle(handle):
    """Text for the slow-callback warning: the task, if a task step runs."""
    owner = getattr(handle._callback, '__self__', None)
    if isinstance(owner, tasks.Task):
        return repr(owner)
    return str(handle)


def _pipe_label(fd):
    if fd == subprocess.PIPE:
        return '<pipe>'
    if fd == subprocess.STDOUT:
        return '<stdout>'
    return repr(fd)


def _thread_pool_type():
    # concurrent.futures is imported only once an executor is needed, so
    # that importing asyncio stays cheap.
    import concurrent.futures
    return concurrent.futures.ThreadPoolExecutor


def _connect_failure(errors, all_errors):
    """The one exception that reports the per-address failures of
    create_connection() (returned, not raised, so that this frame does not
    end up in its traceback holding on to the failures)."""
    if not errors:
        return TimeoutError("create_connection failed")
    if all_errors:
        return ExceptionGroup("create_connection failed", errors)
    if all(str(e) == str(errors[0]) for e in errors):
        return errors[0]
    return OSError('Multiple exceptions: {}'.format(
        ', '.join(str(e) for e in errors)))


def _flatten(groups):
    return [item for group in groups for item in group]


# ---------------------------------------------------------------------------
# sendfile() emulation over a transport

class _SendfileFallbackProtocol(protocols.Protocol):
    """Stand-in protocol installed on a transport while sendfile() writes
    file chunks through it.

    It keeps the application's protocol aside (reading paused), tracks the
    transport's write flow control so the copy loop can wait for room, and
    puts everything back in restore().
    """

    def __init__(self, transp):
        if not isinstance(transp, transports._FlowControlMixin):
            raise TypeError("transport should be _FlowControlMixin instance")
        self._transport = transp
        self._proto = transp.get_protocol()
        self._resume_reading_after = transp.is_reading()
        self._writing_was_paused = transp._protocol_paused
        transp.pause_reading()
        transp.set_protocol(self)
        # set while the transport asks us to stop writing
        self._room = (transp._loop.create_future()
                      if self._writing_was_paused else None)

    async def drain(self):
        if self._transport.is_closing():
            raise ConnectionError("Connection closed by peer")
        if self._room is not None:
            await self._room

    def pause_writing(self):
        if self._room is None:
            self._room = self._transport._loop.create_future()

    def resume_writing(self):
        room, self._room = self._room, None
        if room is not None:
            room.set_result(False)

    def connection_made(self, transport):
        raise RuntimeError("Invalid state: "
                           "connection should have been established already.")

    def connection_lost(self, exc):
        if self._room is not None:
            if exc is None:
                exc = ConnectionError("Connection is closed by peer")
            self._room.set_exception(exc)
        self._proto.connection_lost(exc)

    def data_received(self, data):
        raise RuntimeError("Invalid state: reading should be paused")

    def eof_received(self):
        raise RuntimeError("Invalid state: reading should be paused")

    async def restore(self):
        transp = self._transport
        transp.set_protocol(self._proto)
        if self._resume_reading_after:
            transp.resume_reading()
        if self._room is not None:
            # The transport may still be paused: nobody waits for room any
            # more, so the future is simply dropped.
            self._room.cancel()
        if self._writing_was_paused:
            self._proto.resume_writing()


# ---------------------------------------------------------------------------
# Server

class Server(events.AbstractServer):
    """Listening sockets created by loop.create_server()."""

    def __init__(self, loop, sockets, protocol_factory, ssl_context, backlog,
                 ssl_handshake_timeout, ssl_shutdown_timeout=None):
        self._loop = loop
        self._sockets = sockets          # None once closed
        self._protocol_factory = protocol_factory
        self._ssl_context = ssl_context
        self._backlog = backlog
        self._ssl_handshake_timeout = ssl_handshake_timeout
        self._ssl_shutdown_timeout = ssl_shutdown_timeout
        self._clients = weakref.WeakSet()
        self._active_count = 0           # connections not yet detached
        self._waiters = []               # wait_closed() futures; None when done
        self._serving = False
        self._serving_forever_fut = None
        # Accepted connections run their protocol callbacks in a copy of
        # the context the server was created in.
        self._context = contextvars.copy_context()

    def __repr__(self):
        return f'<{self.__class__.__name__} sockets={self.sockets!r}>'

    # Accepted transports register themselves for the server's lifetime.
    def _attach(self, transport):
        self._clients.add(transport)
        self._active_count += 1

    def _detach(self, transport):
        self._clients.discard(transport)
        self._active_count -= 1
        if self._active_count == 0 and self._sockets is None:
            self._wakeup()

    def _wakeup(self):
        waiters, self._waiters = self._waiters, None
        for waiter in waiters or ():
            if not waiter.done():
                waiter.set_result(None)

    def _start_serving(self):
        if self._serving:
            return
        self._serving = True
        for sock in self._sockets:
            sock.listen(self._backlog)
            self._loop._start_serving(
                self._protocol_factory, sock, self._ssl_context, self,
                self._backlog, self._ssl_handshake_timeout,
                self._ssl_shutdown_timeout, context=self._context)

    def get_loop(self):
        return self._loop

    def is_serving(self):
        return self._serving

    @property
    def sockets(self):
        if self._sockets is None:
            return ()
        return tuple(trsock.TransportSocket(s) for s in self._sockets)

    def close(self):
        listening = self._sockets
        if listening is None:
            return
        self._sockets = None
        for sock in listening:
            self._loop._stop_serving(sock)
        self._serving = False

        forever = self._serving_forever_fut
        if forever is not None and not forever.done():
            forever.cancel()
            self._serving_forever_fut = None

        if self._active_count == 0:
            self._wakeup()

    def close_clients(self):
        for transport in list(self._clients):
            transport.close()

    def abort_clients(self):
        for transport in list(self._clients):
            transport.abort()

    async def start_serving(self):
        self._start_serving()
        # give the loop one turn so that accepting really starts
        await tasks.sleep(0)

    async def serve_forever(self):
        if self._serving_forever_fut is not None:
            raise RuntimeError(
                f'server {self!r} is already being awaited on serve_forever()')
        if self._sockets is None:
            raise RuntimeError(f'server {self!r} is closed')

        self._start_serving()
        self._serving_forever_fut = self._loop.create_future()
        try:
            await self._serving_forever_fut
        except exceptions.CancelledError:
            # Cancelling serve_forever() shuts the server down, including
            # connections that would otherwise keep wait_closed() waiting.
            try:
                self.close()
                self.close_clients()
                await self.wait_closed()
            finally:
                raise
        finally:
            self._serving_forever_fut = None

    async def wait_closed(self):
        """Wait until the server is closed and its connections are gone."""
        if self._waiters is None:
            return
        waiter = self._loop.create_future()
        self._waiters.append(waiter)
        await waiter


# ---------------------------------------------------------------------------
# The loop

class BaseEventLoop(events.AbstractEventLoop):

    def __init__(self):
        self._closed = False
        self._stopping = False
        self._ready = collections.deque()    # Handles to run next iteration
        self._scheduled = []                  # heap of TimerHandles
        self._timer_cancelled_count = 0
        self._default_executor = None
        self._executor_shutdown_called = False
        self._internal_fds = 0
        self._thread_id = None                # set while running
        self._clock_resolution = time.get_clock_info('monotonic').resolution
        self._exception_handler = None
        self._task_factory = None
        self._current_handle = None
        self._coroutine_origin_tracking_enabled = False
        self._coroutine_origin_tracking_saved_depth = None
        self._old_agen_hooks = None
        # async generators first iterated while this loop ran; they are
        # finalized by shutdown_asyncgens()
        self._asyncgens = weakref.WeakSet()
        self._asyncgens_shutdown_called = False
        # callbacks slower than this are reported in debug mode
        self.slow_callback_duration = 0.1
        self.set_debug(coroutines._is_debug_mode())

    def __repr__(self):
        return (f'<{self.__class__.__name__} running={self.is_running()} '
                f'closed={self.is_closed()} debug={self.get_debug()}>')

    def __del__(self, _warn=warnings.warn):
        if not self.is_closed():
            _warn(f"unclosed event loop {self!r}", ResourceWarning,
                  source=self)
            if not self.is_running():
                self.close()

    # -- hooks for concrete loops -------------------------------------------

    def _make_socket_transport(self, sock, protocol, waiter=None, *,
                               extra=None, server=None):
        raise NotImplementedError

    def _make_ssl_transport(
            self, rawsock, protocol, sslcontext, waiter=None,
            *, server_side=False, server_hostname=None,
            extra=None, server=None,
            ssl_handshake_timeout=None,
            ssl_shutdown_timeout=None,
            call_connection_made=True, context=None):
        raise NotImplementedError

    def _make_datagram_transport(self, sock, protocol,
                                 address=None, waiter=None, extra=None):
        raise NotImplementedError

    def _make_read_pipe_transport(self, pipe, protocol, waiter=None,
                                  extra=None):
        raise NotImplementedError

    def _make_write_pipe_transport(self, pipe, protocol, waiter=None,
                                   extra=None):
        raise NotImplementedError

    async def _make_subprocess_transport(self, protocol, args, shell,
                                         stdin, stdout, stderr, bufsize,
                                         extra=None, **kwargs):
        raise NotImplementedError

    def _write_to_self(self):
        """Wake the loop up from another thread (or a signal handler)."""
        raise NotImplementedError

    def _process_events(self, event_list):
        """Turn what the selector reported into ready callbacks."""
        raise NotImplementedError

    # -- futures and tasks ----------------------------------------------------

    def create_future(self):
        return futures.Future(loop=self)

    def create_task(self, coro, **kwargs):
        """Wrap *coro* in a task (or whatever the task factory makes)."""
        self._check_closed()
        factory = self._task_factory
        if factory is not None:
            return factory(self, coro, **kwargs)
        task = tasks.Task(coro, loop=self, **kwargs)
        if task._source_traceback:
            # the frame of this method says nothing useful
            del task._source_traceback[-1]
        return task

    def set_task_factory(self, factory):
        if factory is not None and not callable(factory):
            raise TypeError('task factory must be a callable or None')
        self._task_factory = factory

    def get_task_factory(self):
        return self._task_factory

    # -- state checks ---------------------------------------------------------

    def _check_closed(self):
        if self._closed:
            raise RuntimeError('Event loop is closed')

    def _check_default_executor(self):
        if self._executor_shutdown_called:
            raise RuntimeError('Executor shutdown has been called')

    def _check_running(self):
        if self.is_running():
            raise RuntimeError('This event loop is already running')
        if events._get_running_loop() is not None:
            raise RuntimeError(
                'Cannot run the event loop while another loop is running')

    def _check_thread(self):
        """In debug mode, refuse non-thread-safe calls from other threads."""
        owner = self._thread_id
        if owner is not None and owner != threading.get_ident():
            raise RuntimeError(
                "Non-thread-safe operation invoked on an event loop other "
                "than the current one")

    def _check_callback(self, callback, method):
        if (coroutines.iscoroutine(callback)
                or coroutines._iscoroutinefunction(callback)):
            raise TypeError(f"coroutines cannot be used with {method}()")
        if not callable(callback):
            raise TypeError(f'a callable object was expected by {method}(), '
                            f'got {callback!r}')

    def is_running(self):
        return self._thread_id is not None

    def is_closed(self):
        return self._closed

    # -- async generator bookkeeping -------------------------------------------

    def _asyncgen_firstiter_hook(self, agen):
        if self._asyncgens_shutdown_called:
            warnings.warn(
                f"asynchronous generator {agen!r} was scheduled after "
                f"loop.shutdown_asyncgens() call",
                ResourceWarning, source=self)
        self._asyncgens.add(agen)

    def _asyncgen_finalizer_hook(self, agen):
        # May be called from any thread (by the garbage collector).
        self._asyncgens.discard(agen)
        if not self.is_closed():
            self.call_soon_threadsafe(self.create_task, agen.aclose())

    async def shutdown_asyncgens(self):
        """Close every async generator still suspended in this loop."""
        self._asyncgens_shutdown_called = True
        pending = list(self._asyncgens)
        if not pending:
            return
        self._asyncgens.clear()
        outcomes = await tasks.gather(*(agen.aclose() for agen in pending),
                                      return_exceptions=True)
        for agen, outcome in zip(pending, outcomes):
            if isinstance(outcome, Exception):
                self.call_exception_handler({
                    'message': f'an error occurred during closing of '
                               f'asynchronous generator {agen!r}',
                    'exception': outcome,
                    'asyncgen': agen,
                })

    # -- executors ------------------------------------------------------------

    async def shutdown_default_executor(self, timeout=None):
        """Shut the default executor down, waiting up to *timeout* seconds
        for its threads (None: no limit)."""
        self._executor_shutdown_called = True
        executor = self._default_executor
        if executor is None:
            return
        done = self.create_future()
        joiner = threading.Thread(target=self._do_shutdown, args=(done,))
        joiner.start()
        try:
            async with timeouts.timeout(timeout):
                await done
        except TimeoutError:
            warnings.warn("The executor did not finishing joining "
                          f"its threads within {timeout} seconds.",
                          RuntimeWarning, stacklevel=2)
            executor.shutdown(wait=False)
        else:
            joiner.join()

    def _do_shutdown(self, done):
        # runs in a helper thread
        try:
            self._default_executor.shutdown(wait=True)
        except Exception as exc:
            if not self.is_closed() and not done.cancelled():
                self.call_soon_threadsafe(done.set_exception, exc)
        else:
            if not self.is_closed():
                self.call_soon_threadsafe(
                    futures._set_result_unless_cancelled, done, None)

    def run_in_executor(self, executor, func, *args):
        self._check_closed()
        if self._debug:
            self._check_callback(func, 'run_in_executor')
        if executor is None:
            self._check_default_executor()
            executor = self._default_executor
            if executor is None:
                executor = _thread_pool_type()(thread_name_prefix='asyncio')
                self._default_executor = executor
        return futures.wrap_future(executor.submit(func, *args), loop=self)

    def set_default_executor(self, executor):
        if not isinstance(executor, _thread_pool_type()):
            raise TypeError('executor must be ThreadPoolExecutor instance')
        self._default_executor = executor

    # -- running and stopping ---------------------------------------------------

    def _run_forever_setup(self):
        """Everything run_forever() does before its first iteration; usable
        by code that drives _run_once() itself."""
        self._check_closed()
        self._check_running()
        self._set_coroutine_origin_tracking(self._debug)
        self._old_agen_hooks = sys.get_asyncgen_hooks()
        self._thread_id = threading.get_ident()
        sys.set_asyncgen_hooks(firstiter=self._asyncgen_firstiter_hook,
                               finalizer=self._asyncgen_finalizer_hook)
        events._set_running_loop(self)

    def _run_forever_cleanup(self):
        """Undo _run_forever_setup()."""
        self._stopping = False
        self._thread_id = None
        events._set_running_loop(None)
        self._set_coroutine_origin_tracking(False)
        hooks, self._old_agen_hooks = self._old_agen_hooks, None
        if hooks is not None:
            sys.set_asyncgen_hooks(*hooks)

    def run_forever(self):
        """Run iterations until stop() is called."""
        self._run_forever_setup()
        try:
            while True:
                self._run_once()
                if self._stopping:
                    break
        finally:
            self._run_forever_cleanup()

    def run_until_complete(self, future):
        """Run until *future* (a future, task or coroutine) is done and
        return its result or raise its exception."""
        self._check_closed()
        self._check_running()

        wrapped_here = not futures.isfuture(future)
        future = tasks.ensure_future(future, loop=self)
        if wrapped_here:
            # The task never escapes this call: if the loop is interrupted
            # it would otherwise complain about being destroyed pending.
            future._log_destroy_pending = False

        future.add_done_callback(_stop_when_done)
        try:
            self.run_forever()
        except BaseException:
            if wrapped_here and future.done() and not future.cancelled():
                # The exception escapes through run_forever() already; mark
                # it as retrieved so the task does not log it again.
                future.exception()
            raise
        finally:
            future.remove_done_callback(_stop_when_done)
        if not future.done():
            raise RuntimeError('Event loop stopped before Future completed.')
        return future.result()

    def stop(self):
        """Stop after the callbacks already queued have run."""
        self._stopping = True

    def close(self):
        """Close the loop: drop pending callbacks, shut the default executor
        down without waiting.  Closing twice is harmless."""
        if self.is_running():
            raise RuntimeError("Cannot close a running event loop")
        if self._closed:
            return
        if self._debug:
            logger.debug("Close %r", self)
        self._closed = True
        self._ready.clear()
        self._scheduled.clear()
        self._executor_shutdown_called = True
        executor, self._default_executor = self._default_executor, None
        if executor is not None:
            executor.shutdown(wait=False)

    # -- scheduling -------------------------------------------------------------

    def time(self):
        return time.monotonic()

    def call_later(self, delay, callback, *args, context=None):
        """Run callback(*args) *delay* seconds from now."""
        if delay is None:
            raise TypeError('delay must not be None')
        # Goes through call_at() on purpose: subclasses hook into it.
        timer = self.call_at(self.time() + delay, callback, *args,
                             context=context)
        if timer._source_traceback:
            del timer._source_traceback[-1]
        return timer

    def call_at(self, when, callback, *args, context=None):
        """Run callback(*args) at loop time *when*."""
        if when is None:
            raise TypeError("when cannot be None")
        self._check_closed()
        if self._debug:
            self._check_thread()
            self._check_callback(callback, 'call_at')
        timer = events.TimerHandle(when, callback, args, self, context)
        if timer._source_traceback:
            del timer._source_traceback[-1]
        heapq.heappush(self._scheduled, timer)
        timer._scheduled = True
        return timer

    def call_soon(self, callback, *args, context=None):
        """Run callback(*args) on the next iteration, in FIFO order."""
        self._check_closed()
        if self._debug:
            self._check_thread()
            self._check_callback(callback, 'call_soon')
        handle = self._call_soon(callback, args, context)
        if handle._source_traceback:
            del handle._source_traceback[-1]
        return handle

    def _call_soon(self, callback, args, context):
        handle = events.Handle(callback, args, self, context)
        if handle._source_traceback:
            del handle._source_traceback[-1]
        self._ready.append(handle)
        return handle

    def call_soon_threadsafe(self, callback, *args, context=None):
        """call_soon() that may be used from any thread."""
        self._check_closed()
        if self._debug:
            self._check_callback(callback, 'call_soon_threadsafe')
        handle = events._ThreadSafeHandle(callback, args, self, context)
        if handle._source_traceback:
            # this method and _ThreadSafeHandle.__init__ itself
            del handle._source_traceback[-2:]
        self._ready.append(handle)
        self._write_to_self()
        return handle

    def _add_callback(self, handle):
        """Queue an existing Handle (used for I/O callbacks)."""
        if not handle._cancelled:
            self._ready.append(handle)

    def _add_callback_signalsafe(self, handle):
        """_add_callback() for signal handlers: also wakes the selector."""
        self._add_callback(handle)
        self._write_to_self()

    def _timer_handle_cancelled(self, handle):
        """Called by TimerHandle.cancel()."""
        if handle._scheduled:
            self._timer_cancelled_count += 1

    def _purge_cancelled_timers(self):
        heap = self._scheduled
        if (len(heap) > _MIN_SCHEDULED_TIMER_HANDLES
                and self._timer_cancelled_count / len(heap)
                > _MIN_CANCELLED_TIMER_HANDLES_FRACTION):
            live = []
            for timer in heap:
                if timer._cancelled:
                    timer._scheduled = False
                else:
                    live.append(timer)
            heapq.heapify(live)
            self._scheduled = live
            self._timer_cancelled_count = 0
            return
        # Few cancelled timers: only drop those that reached the top.
        while heap and heap[0]._cancelled:
            self._timer_cancelled_count -= 1
            heapq.heappop(heap)._scheduled = False

    def _run_once(self):
        """One loop iteration: wait for I/O (no longer than the nearest
        timer allows), move due timers to the ready queue, then run every
        callback that was ready when the iteration started."""
        self._purge_cancelled_timers()

        if self._ready or self._stopping:
            timeout = 0
        elif self._scheduled:
            delay = self._scheduled[0]._when - self.time()
            timeout = min(max(delay, 0), MAXIMUM_SELECT_TIMEOUT)
        else:
            timeout = None

        self._process_events(self._selector.select(timeout))

        # Timers due within the clock's resolution count as due now.
        horizon = self.time() + self._clock_resolution
        heap = self._scheduled
        ready = self._ready
        while heap and heap[0]._when < horizon:
            timer = heapq.heappop(heap)
            timer._scheduled = False
            ready.append(timer)

        # Callbacks queued by the callbacks below wait for the next round.
        if self._debug:
            self._run_ready_debug(len(ready))
        else:
            for _ in range(len(ready)):
                handle = ready.popleft()
                if not handle._cancelled:
                    handle._run()
        handle = None   # do not keep the last callback alive

    def _run_ready_debug(self, count):
        ready = self._ready
        for _ in range(count):
            handle = ready.popleft()
            if handle._cancelled:
                continue
            self._current_handle = handle
            try:
                started = self.time()
                handle._run()
                elapsed = self.time() - started
                if elapsed >= self.slow_callback_duration:
                    logger.warning('Executing %s took %.3f seconds',
                                   _describe_handle(handle), elapsed)
            finally:
                self._current_handle = None

    # -- debug mode -------------------------------------------------------------

    def _set_coroutine_origin_tracking(self, enabled):
        enabled = bool(enabled)
        if enabled == self._coroutine_origin_tracking_enabled:
            return
        if enabled:
            self._coroutine_origin_tracking_saved_depth = (
                sys.get_coroutine_origin_tracking_depth())
            sys.set_coroutine_origin_tracking_depth(
                constants.DEBUG_STACK_DEPTH)
        else:
            sys.set_coroutine_origin_tracking_depth(
                self._coroutine_origin_tracking_saved_depth)
        self._coroutine_origin_tracking_enabled = enabled

    def get_debug(self):
        return self._debug

    def set_debug(self, enabled):
        self._debug = enabled
        if self.is_running():
            self.call_soon_threadsafe(self._set_coroutine_origin_tracking,
                                      enabled)

    # -- name resolution --------------------------------------------------------

    def _getaddrinfo_debug(self, host, port, family, type, proto, flags):
        details = [f"{host}:{port!r}"]
        for label, value in (('family', family), ('type', type),
                             ('proto', proto), ('flags', flags)):
            if value:
                details.append(f'{label}={value!r}')
        what = ', '.join(details)
        logger.debug('Get address info %s', what)

        started = self.time()
        infos = socket.getaddrinfo(host, port, family, type, proto, flags)
        elapsed = self.time() - started

        report = (f'Getting address info {what} took '
                  f'{elapsed * 1e3:.3f}ms: {infos!r}')
        if elapsed >= self.slow_callback_duration:
            logger.info(report)
        else:
            logger.debug(report)
        return infos

    async def getaddrinfo(self, host, port, *,
                          family=0, type=0, proto=0, flags=0):
        resolver = (self._getaddrinfo_debug if self._debug
                    else socket.getaddrinfo)
        return await self.run_in_executor(None, resolver, host, port,
                                          family, type, proto, flags)

    async def getnameinfo(self, sockaddr, flags=0):
        return await self.run_in_executor(None, socket.getnameinfo,
                                          sockaddr, flags)

    async def _ensure_resolved(self, address, *,
                               family=0, type=socket.SOCK_STREAM,
                               proto=0, flags=0, loop):
        """Resolve *address*, skipping the executor for numeric hosts."""
        host, port = address[:2]
        numeric = _ipaddr_info(host, port, family, type, proto, *address[2:])
        if numeric is not None:
            return [numeric]
        return await loop.getaddrinfo(host, port, family=family, type=type,
                                      proto=proto, flags=flags)

    # -- sendfile ---------------------------------------------------------------

    async def sock_sendfile(self, sock, file, offset=0, count=None,
                            *, fallback=True):
        """Send *file* over the connected stream socket *sock*."""
        if self._debug and sock.gettimeout() != 0:
            raise ValueError("the socket must be non-blocking")
        _check_ssl_socket(sock)
        self._check_sendfile_params(sock, file, offset, count)
        try:
            return await self._sock_sendfile_native(sock, file,
                                                    offset, count)
        except exceptions.SendfileNotAvailableError:
            if not fallback:
                raise
        return await self._sock_sendfile_fallback(sock, file, offset, count)

    async def _sock_sendfile_native(self, sock, file, offset, count):
        # Concrete loops override this when the OS offers a sendfile call.
        raise exceptions.SendfileNotAvailableError(
            f"syscall sendfile is not available for socket {sock!r} "
            f"and file {file!r} combination")

    @staticmethod
    def _sendfile_chunk_size(count):
        size = constants.SENDFILE_FALLBACK_READBUFFER_SIZE
        return min(count, size) if count else size

    async def _copy_file_chunks(self, file, offset, count, chunk_size, send):
        """Read *file* from *offset* (at most *count* bytes when given) in
        executor threads and pass each chunk to ``await send(view)``.
        Returns the number of bytes sent; the file position is left right
        after them.  *offset* is absolute: the current position of the file
        does not matter."""
        file.seek(offset)
        buf = memoryview(bytearray(chunk_size))
        sent = 0
        try:
            while True:
                want = chunk_size
                if count:
                    want = min(count - sent, chunk_size)
                    if want <= 0:
                        break
                got = await self.run_in_executor(None, file.readinto,
                                                 buf[:want])
                if not got:
                    break
                await send(buf[:got])
                sent += got
            return sent
        finally:
            if hasattr(file, 'seek'):
                file.seek(offset + sent)

    async def _sock_sendfile_fallback(self, sock, file, offset, count):
        async def send(chunk):
            await self.sock_sendall(sock, chunk)
        return await self._copy_file_chunks(
            file, offset, count, self._sendfile_chunk_size(count), send)

    def _check_sendfile_params(self, sock, file, offset, count):
        if 'b' not in getattr(file, 'mode', 'b'):
            raise ValueError("file should be opened in binary mode")
        if sock.type != socket.SOCK_STREAM:
            raise ValueError("only SOCK_STREAM type sockets are supported")
        if count is not None:
            if not isinstance(count, int):
                raise TypeError(
                    f"count must be a positive integer (got {count!r})")
            if count <= 0:
                raise ValueError(
                    f"count must be a positive integer (got {count!r})")
        if not isinstance(offset, int):
            raise TypeError(
                f"offset must be a non-negative integer (got {offset!r})")
        if offset < 0:
            raise ValueError(
                f"offset must be a non-negative integer (got {offset!r})")

    async def sendfile(self, transport, file, offset=0, count=None,
                       *, fallback=True):
        """Send *file* through *transport*, natively when the transport
        supports it, else by copying chunks (unless *fallback* is false).
        Returns the number of bytes sent."""
        if transport.is_closing():
            raise RuntimeError("Transport is closing")
        Mode = constants._SendfileMode
        mode = getattr(transport, '_sendfile_compatible', Mode.UNSUPPORTED)
        if mode is Mode.UNSUPPORTED:
            raise RuntimeError(
                f"sendfile is not supported for transport {transport!r}")
        if mode is Mode.TRY_NATIVE:
            try:
                return await self._sendfile_native(transport, file,
                                                   offset, count)
            except exceptions.SendfileNotAvailableError:
                if not fallback:
                    raise
        elif not fallback:
            raise RuntimeError(
                f"fallback is disabled and native sendfile is not "
                f"supported for transport {transport!r}")
        return await self._sendfile_fallback(transport, file, offset, count)

    async def _sendfile_native(self, transp, file, offset, count):
        raise exceptions.SendfileNotAvailableError(
            "sendfile syscall is not supported")

    async def _sendfile_fallback(self, transp, file, offset, count):
        gate = _SendfileFallbackProtocol(transp)

        async def send(chunk):
            await gate.drain()
            transp.write(chunk)

        try:
            return await self._copy_file_chunks(
                file, offset, count, min(count, 16384) if count else 16384,
                send)
        finally:
            await gate.restore()

    # -- TLS upgrade ------------------------------------------------------------

    async def start_tls(self, transport, protocol, sslcontext, *,
                        server_side=False,
                        server_hostname=None,
                        ssl_handshake_timeout=None,
                        ssl_shutdown_timeout=None,
                        _buffered=None):
        """Wrap an established connection in TLS and return the new
        transport once the handshake is done.

        *_buffered* (private) holds bytes the caller has already read off
        the connection; they are the start of the peer's TLS stream (for
        instance its ClientHello) and are fed to the TLS layer first.
        """
        if ssl is None:
            raise RuntimeError('Python ssl module is not available')
        if not isinstance(sslcontext, ssl.SSLContext):
            raise TypeError(
                f'sslcontext is expected to be an instance of ssl.SSLContext, '
                f'got {sslcontext!r}')
        if not getattr(transport, '_start_tls_compatible', False):
            raise TypeError(
                f'transport {transport!r} is not supported by start_tls()')

        waiter = self.create_future()
        tls = sslproto.SSLProtocol(
            self, protocol, sslcontext, waiter, server_side, server_hostname,
            ssl_handshake_timeout=ssl_handshake_timeout,
            ssl_shutdown_timeout=ssl_shutdown_timeout,
            call_connection_made=False)
        if _buffered:
            tls._incoming.write(_buffered)

        # Switch protocols with reading paused so that no byte reaches the
        # application protocol in clear text.
        transport.pause_reading()
        transport.set_protocol(tls)
        pending = (self.call_soon(tls.connection_made, transport),
                   self.call_soon(transport.resume_reading))
        try:
            await waiter
        except BaseException:
            transport.close()
            for handle in pending:
                handle.cancel()
            # The traceback keeps this frame alive: drop what would tie the
            # exception back to itself through the TLS objects.
            waiter = tls = pending = None
            raise
        return tls._app_transport

    # -- outgoing connections ---------------------------------------------------

    async def _connect_sock(self, exceptions, addr_info, local_addr_infos=None):
        """Create a socket for *addr_info*, bind it to a matching local
        address if any were given, and connect it.  Failures are appended
        to a fresh list that is itself appended to *exceptions*."""
        failures = []
        exceptions.append(failures)
        family, type_, proto, _, address = addr_info
        sock = None
        try:
            sock = socket.socket(family=family, type=type_, proto=proto)
            sock.setblocking(False)
            if local_addr_infos is not None:
                self._bind_local(sock, family, local_addr_infos, failures)
            await self.sock_connect(sock, address)
            return sock
        except OSError as exc:
            failures.append(exc)
            if sock is not None:
                _close_quietly(sock)
            raise
        except BaseException:
            if sock is not None:
                _close_quietly(sock)
            raise
        finally:
            # break the cycle exception -> traceback -> frame -> lists
            exceptions = failures = None

    @staticmethod
    def _bind_local(sock, family, local_addr_infos, failures):
        for lfamily, _, _, _, laddr in local_addr_infos:
            if lfamily != family:
                continue
            try:
                sock.bind(laddr)
                return
            except OSError as exc:
                failures.append(OSError(
                    exc.errno,
                    f'error while attempting to bind on address '
                    f'{laddr!r}: {str(exc).lower()}'))
        if failures:
            raise failures.pop()
        raise OSError(f"no matching local address with {family=} found")

    async def create_connection(
            self, protocol_factory, host=None, port=None,
            *, ssl=None, family=0,
            proto=0, flags=0, sock=None,
            local_addr=None, server_hostname=None,
            ssl_handshake_timeout=None,
            ssl_shutdown_timeout=None,
            happy_eyeballs_delay=None, interleave=None,
            all_errors=False):
        """Open a stream connection to (host, port), or wrap the connected
        socket *sock*, and return (transport, protocol)."""
        if server_hostname is not None and not ssl:
            raise ValueError('server_hostname is only meaningful with ssl')
        if server_hostname is None and ssl:
            # The host name is what the certificate is checked against.
            if not host:
                raise ValueError('You must set server_hostname '
                                 'when using ssl without a host')
            server_hostname = host
        self._require_ssl_for_timeouts(bool(ssl), ssl_handshake_timeout,
                                       ssl_shutdown_timeout)
        if sock is not None:
            _check_ssl_socket(sock)
        if happy_eyeballs_delay is not None and interleave is None:
            interleave = 1      # RFC 8305 recommends alternating families

        if host is not None or port is not None:
            if sock is not None:
                raise ValueError(
                    'host/port and sock can not be specified at the same time')
            sock = await self._dial(
                host, port, family, proto, flags, local_addr,
                happy_eyeballs_delay, interleave, all_errors)
        else:
            if sock is None:
                raise ValueError(
                    'host and port was not specified and no sock specified')
            if sock.type != socket.SOCK_STREAM:
                # Datagram sockets would "connect" fine and then fail in
                # confusing ways (see create_datagram_endpoint()).
                raise ValueError(f'A Stream Socket was expected, got {sock!r}')

        transport, protocol = await self._create_connection_transport(
            sock, protocol_factory, ssl, server_hostname,
            ssl_handshake_timeout=ssl_handshake_timeout,
            ssl_shutdown_timeout=ssl_shutdown_timeout)
        if self._debug:
            raw = transport.get_extra_info('socket')
            logger.debug("%r connected to %s:%r: (%r, %r)",
                         raw, host, port, transport, protocol)
        return transport, protocol

    async def _resolve_nonempty(self, address, **kwargs):
        infos = await self._ensure_resolved(address, loop=self, **kwargs)
        if not infos:
            raise OSError('getaddrinfo() returned empty list')
        return infos

    async def _dial(self, host, port, family, proto, flags, local_addr,
                    happy_eyeballs_delay, interleave, all_errors):
        """Resolve (host, port) and return the first socket that connects."""
        lookup = dict(family=family, type=socket.SOCK_STREAM, proto=proto,
                      flags=flags)
        targets = await self._resolve_nonempty((host, port), **lookup)
        local_infos = None
        if local_addr is not None:
            local_infos = await self._resolve_nonempty(local_addr, **lookup)
        if interleave:
            targets = _interleave_addrinfos(targets, interleave)

        errors = []
        attempts = None
        sock = None
        try:
            if happy_eyeballs_delay is None:
                for target in targets:
                    try:
                        sock = await self._connect_sock(errors, target,
                                                        local_infos)
                    except OSError:
                        continue
                    break
            else:
                attempts = [functools.partial(self._connect_sock, errors,
                                              target, local_infos)
                            for target in targets]
                outcome = await staggered.staggered_race(
                    attempts, happy_eyeballs_delay, loop=self)
                sock = outcome[0]
            if sock is not None:
                return sock
            failure = _connect_failure(_flatten(errors), all_errors)
            try:
                raise failure
            finally:
                failure = None
        finally:
            # The frames of the failed attempts stay reachable from the
            # traceback of what is raised, and they reach these lists (the
            # attempts hold them): empty them so no container keeps the
            # exceptions, and with them that traceback, alive.
            for group in errors:
                group.clear()
            errors.clear()
            errors = attempts = outcome = None

    @staticmethod
    def _require_ssl_for_timeouts(have_ssl, handshake, shutdown):
        if handshake is not None and not have_ssl:
            raise ValueError(
                'ssl_handshake_timeout is only meaningful with ssl')
        if shutdown is not None and not have_ssl:
            raise ValueError(
                'ssl_shutdown_timeout is only meaningful with ssl')

    async def _create_connection_transport(
            self, sock, protocol_factory, ssl,
            server_hostname, server_side=False,
            ssl_handshake_timeout=None,
            ssl_shutdown_timeout=None):
        """Build the (possibly TLS) transport for a connected socket and
        wait until it is ready.  The socket is closed if that fails before
        a transport owns it."""
        try:
            sock.setblocking(False)
            protocol = protocol_factory()
            waiter = self.create_future()
            if ssl:
                context = None if isinstance(ssl, bool) else ssl
                if context is None:
                    context = sslproto._create_transport_context(
                        server_side, server_hostname)
                transport = self._make_ssl_transport(
                    sock, protocol, context, waiter,
                    server_side=server_side, server_hostname=server_hostname,
                    ssl_handshake_timeout=ssl_handshake_timeout,
                    ssl_shutdown_timeout=ssl_shutdown_timeout,
                    context=contextvars.copy_context())
            else:
                transport = self._make_socket_transport(sock, protocol, waiter)
        except BaseException:
            sock.close()
            raise

        try:
            await waiter
        except BaseException:
            transport.close()
            # see start_tls(): keep the traceback from pinning the transport
            waiter = transport = protocol = None
            raise
        return transport, protocol

    # -- datagram endpoints -----------------------------------------------------

    async def _datagram_address_pairs(self, local_addr, remote_addr,
                                      family, proto, flags):
        """Plan the sockets create_datagram_endpoint() may try: a list of
        ((family, proto), (local_address, remote_address))."""
        if not (local_addr or remote_addr):
            if family == 0:
                raise ValueError('unexpected address family')
            return [((family, proto), (None, None))]

        if hasattr(socket, 'AF_UNIX') and family == socket.AF_UNIX:
            for addr in (local_addr, remote_addr):
                if addr is not None and not isinstance(addr, str):
                    raise TypeError('string is expected')
            if local_addr and local_addr[0] not in (0, '\x00'):
                _remove_stale_unix_socket(local_addr)
            return [((family, proto), (local_addr, remote_addr))]

        # Resolve both ends and pair the results by (family, protocol).
        pairs = {}
        for slot, addr in enumerate((local_addr, remote_addr)):
            if addr is None:
                continue
            if not (isinstance(addr, tuple) and len(addr) == 2):
                raise TypeError('2-tuple is expected')
            infos = await self._resolve_nonempty(
                addr, family=family, type=socket.SOCK_DGRAM,
                proto=proto, flags=flags)
            for fam, _, pro, _, sockaddr in infos:
                pairs.setdefault((fam, pro), [None, None])[slot] = sockaddr

        usable = [(key, tuple(ends)) for key, ends in pairs.items()
                  if (not local_addr or ends[0] is not None)
                  and (not remote_addr or ends[1] is not None)]
        if not usable:
            raise ValueError('can not get address information')
        return usable

    async def _open_datagram_socket(self, family, proto, local, remote,
                                    reuse_port, allow_broadcast):
        """Create, configure, bind and connect one datagram socket.
        Returns (sock, remote address or None)."""
        sock = socket.socket(family=family, type=socket.SOCK_DGRAM,
                             proto=proto)
        try:
            if reuse_port:
                _set_reuseport(sock)
            if allow_broadcast:
                sock.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
            sock.setblocking(False)
            if local is not None:
                sock.bind(local)
            if remote is not None and not allow_broadcast:
                await self.sock_connect(sock, remote)
        except BaseException:
            sock.close()
            raise
        return sock, remote

    async def create_datagram_endpoint(self, protocol_factory,
                                       local_addr=None, remote_addr=None, *,
                                       family=0, proto=0, flags=0,
                                       reuse_port=None,
                                       allow_broadcast=None, sock=None):
        """Create a datagram connection and return (transport, protocol)."""
        if sock is not None:
            if sock.type != socket.SOCK_DGRAM:
                raise ValueError(
                    f'A datagram socket was expected, got {sock!r}')
            modifiers = dict(
                local_addr=local_addr, remote_addr=remote_addr,
                family=family, proto=proto, flags=flags,
                reuse_port=reuse_port, allow_broadcast=allow_broadcast)
            given = ', '.join(f'{name}={value}'
                              for name, value in modifiers.items() if value)
            if given:
                raise ValueError(
                    f'socket modifier keyword arguments can not be used '
                    f'when sock is specified. ({given})')
            sock.setblocking(False)
            r_addr = None
        else:
            plan = await self._datagram_address_pairs(
                local_addr, remote_addr, family, proto, flags)
            errors = []
            for (fam, pro), (local, remote) in plan:
                try:
                    sock, r_addr = await self._open_datagram_socket(
                        fam, pro, local if local_addr else None,
                        remote if remote_addr else None,
                        reuse_port, allow_broadcast)
                except OSError as exc:
                    errors.append(exc)
                else:
                    break
            else:
                try:
                    raise errors[0]
                finally:
                    errors = None

        protocol = protocol_factory()
        waiter = self.create_future()
        transport = self._make_datagram_transport(sock, protocol, r_addr,
                                                  waiter)
        if self._debug:
            if local_addr:
                logger.info("Datagram endpoint local_addr=%r remote_addr=%r "
                            "created: (%r, %r)",
                            local_addr, remote_addr, transport, protocol)
            else:
                logger.debug("Datagram endpoint remote_addr=%r created: "
                             "(%r, %r)", remote_addr, transport, protocol)
        try:
            await waiter
        except BaseException:
            transport.close()
            raise
        return transport, protocol

    # -- servers ----------------------------------------------------------------

    async def _create_server_getaddrinfo(self, host, port, family, flags):
        infos = await self._ensure_resolved((host, port), family=family,
                                            type=socket.SOCK_STREAM,
                                            flags=flags, loop=self)
        if not infos:
            raise OSError(f'getaddrinfo({host!r}) returned empty list')
        return infos

    def _bind_listening_socket(self, info, reuse_address, reuse_port,
                               keep_alive):
        """Create and bind one listening socket for a getaddrinfo() entry.
        Returns None when the entry has to be skipped."""
        af, socktype, proto, _, sockaddr = info
        try:
            sock = socket.socket(af, socktype, proto)
        except OSError:
            if self._debug:
                logger.warning('create_server() failed to create '
                               'socket.socket(%r, %r, %r)',
                               af, socktype, proto, exc_info=True)
            return None
        try:
            if reuse_address:
                sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, True)
            if reuse_port:
                _set_reuseport(sock)
            if keep_alive:
                sock.setsockopt(socket.SOL_SOCKET, socket.SO_KEEPALIVE, True)
            # One socket per family: keep IPv6 sockets from also taking the
            # IPv4 port (which a separate IPv4 socket binds).
            if (_HAS_IPv6 and af == socket.AF_INET6
                    and hasattr(socket, 'IPPROTO_IPV6')):
                sock.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, True)
            try:
                sock.bind(sockaddr)
            except OSError as err:
                text = (f'error while attempting to bind on address '
                        f'{sockaddr!r}: {str(err).lower()}')
                if err.errno == errno.EADDRNOTAVAIL:
                    # usually an address family the host has disabled
                    sock.close()
                    if self._debug:
                        logger.warning(text)
                    return None
                raise OSError(err.errno, text) from None
        except BaseException:
            sock.close()
            raise
        return sock

    async def create_server(
            self, protocol_factory, host=None, port=None,
            *,
            family=socket.AF_UNSPEC,
            flags=socket.AI_PASSIVE,
            sock=None,
            backlog=100,
            ssl=None,
            reuse_address=None,
            reuse_port=None,
            keep_alive=None,
            ssl_handshake_timeout=None,
            ssl_shutdown_timeout=None,
            start_serving=True):
        """Create a TCP server listening on *host* (a name, a sequence of
        names, or None for all interfaces) and *port*, or on *sock*."""
        if isinstance(ssl, bool):
            raise TypeError('ssl argument must be an SSLContext or None')
        self._require_ssl_for_timeouts(ssl is not None, ssl_handshake_timeout,
                                       ssl_shutdown_timeout)
        if sock is not None:
            _check_ssl_socket(sock)

        if host is not None or port is not None:
            if sock is not None:
                raise ValueError(
                    'host/port and sock can not be specified at the same time')
            if reuse_address is None:
                reuse_address = os.name == "posix" and sys.platform != "cygwin"

            if host == '':
                hosts = [None]
            elif (isinstance(host, str)
                    or not isinstance(host, collections.abc.Iterable)):
                hosts = [host]
            else:
                hosts = host
            resolved = await tasks.gather(*[
                self._create_server_getaddrinfo(h, port, family, flags)
                for h in hosts])
            infos = set(itertools.chain.from_iterable(resolved))

            sockets = []
            try:
                for info in infos:
                    listener = self._bind_listening_socket(
                        info, reuse_address, reuse_port, keep_alive)
                    if listener is not None:
                        sockets.append(listener)
                if not sockets:
                    raise OSError('could not bind on any address out of %r'
                                  % ([info[4] for info in infos],))
            except BaseException:
                for listener in sockets:
                    listener.close()
                raise
        else:
            if sock is None:
                raise ValueError('Neither host/port nor sock were specified')
            if sock.type != socket.SOCK_STREAM:
                raise ValueError(f'A Stream Socket was expected, got {sock!r}')
            sockets = [sock]

        for listener in sockets:
            listener.setblocking(False)

        server = Server(self, sockets, protocol_factory,
                        ssl, backlog, ssl_handshake_timeout,
                        ssl_shutdown_timeout)
        if start_serving:
            server._start_serving()
            # let the listening sockets be registered before returning
            await tasks.sleep(0)

        if self._debug:
            logger.info("%r is serving", server)
        return server

    async def connect_accepted_socket(
            self, protocol_factory, sock,
            *, ssl=None,
            ssl_handshake_timeout=None,
            ssl_shutdown_timeout=None):
        """Wrap a socket accepted outside asyncio; returns
        (transport, protocol)."""
        if sock.type != socket.SOCK_STREAM:
            raise ValueError(f'A Stream Socket was expected, got {sock!r}')
        self._require_ssl_for_timeouts(bool(ssl), ssl_handshake_timeout,
                                       ssl_shutdown_timeout)
        _check_ssl_socket(sock)

        transport, protocol = await self._create_connection_transport(
            sock, protocol_factory, ssl, '', server_side=True,
            ssl_handshake_timeout=ssl_handshake_timeout,
            ssl_shutdown_timeout=ssl_shutdown_timeout)
        if self._debug:
            raw = transport.get_extra_info('socket')
            logger.debug("%r handled: (%r, %r)", raw, transport, protocol)
        return transport, protocol

    # -- pipes ------------------------------------------------------------------

    async def _connect_pipe(self, make_transport, kind, protocol_factory,
                            pipe):
        protocol = protocol_factory()
        waiter = self.create_future()
        transport = make_transport(pipe, protocol, waiter)
        try:
            await waiter
        except BaseException:
            transport.close()
            raise
        if self._debug:
            logger.debug('%s pipe %r connected: (%r, %r)',
                         kind, pipe.fileno(), transport, protocol)
        return transport, protocol

    async def connect_read_pipe(self, protocol_factory, pipe):
        return await self._connect_pipe(self._make_read_pipe_transport,
                                        'Read', protocol_factory, pipe)

    async def connect_write_pipe(self, protocol_factory, pipe):
        return await self._connect_pipe(self._make_write_pipe_transport,
                                        'Write', protocol_factory, pipe)

    # -- subprocesses -----------------------------------------------------------

    def _log_subprocess(self, msg, stdin, stdout, stderr):
        parts = [msg]
        if stdin is not None:
            parts.append(f'stdin={_pipe_label(stdin)}')
        if stdout is not None and stderr == subprocess.STDOUT:
            parts.append(f'stdout=stderr={_pipe_label(stdout)}')
        else:
            if stdout is not None:
                parts.append(f'stdout={_pipe_label(stdout)}')
            if stderr is not None:
                parts.append(f'stderr={_pipe_label(stderr)}')
        logger.debug(' '.join(parts))

    @staticmethod
    def _check_subprocess_options(shell_expected, shell, universal_newlines,
                                  bufsize, encoding, errors, text):
        # The transports speak bytes and do their own buffering.
        if universal_newlines:
            raise ValueError("universal_newlines must be False")
        if bool(shell) != shell_expected:
            raise ValueError(f"shell must be {shell_expected}")
        if bufsize != 0:
            raise ValueError("bufsize must be 0")
        if text:
            raise ValueError("text must be False")
        if encoding is not None:
            raise ValueError("encoding must be None")
        if errors is not None:
            raise ValueError("errors must be None")

    async def _spawn(self, protocol_factory, args, shell, description,
                     stdin, stdout, stderr, bufsize, kwargs):
        protocol = protocol_factory()
        if self._debug:
            self._log_subprocess(description, stdin, stdout, stderr)
        transport = await self._make_subprocess_transport(
            protocol, args, shell, stdin, stdout, stderr, bufsize, **kwargs)
        if self._debug:
            logger.info('%s: %r', description, transport)
        return transport, protocol

    async def subprocess_shell(self, protocol_factory, cmd, *,
                               stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE,
                               universal_newlines=False,
                               shell=True, bufsize=0,
                               encoding=None, errors=None, text=None,
                               **kwargs):
        if not isinstance(cmd, (bytes, str)):
            raise ValueError("cmd must be a string")
        self._check_subprocess_options(True, shell, universal_newlines,
                                       bufsize, encoding, errors, text)
        return await self._spawn(protocol_factory, cmd, True,
                                 f'run shell command {cmd!r}',
                                 stdin, stdout, stderr, bufsize, kwargs)

    async def subprocess_exec(self, protocol_factory, program, *args,
                              stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, universal_newlines=False,
                              shell=False, bufsize=0,
                              encoding=None, errors=None, text=None,
                              **kwargs):
        self._check_subprocess_options(False, shell, universal_newlines,
                                       bufsize, encoding, errors, text)
        return await self._spawn(protocol_factory, (program, *args), False,
                                 f'execute program {program!r}',
                                 stdin, stdout, stderr, bufsize, kwargs)

    # -- error reporting --------------------------------------------------------

    def get_exception_handler(self):
        return self._exception_handler

    def set_exception_handler(self, handler):
        """Install handler(loop, context) for unhandled errors; None
        restores default_exception_handler()."""
        if handler is not None and not callable(handler):
            raise TypeError(f'A callable object or None is expected, '
                            f'got {handler!r}')
        self._exception_handler = handler

    def default_exception_handler(self, context):
        """Log *context* at ERROR level.

        The 'message' key gives the first line, 'exception' the exc_info;
        every other key is listed as "key: repr(value)", except the
        creation tracebacks, which are formatted.  When the failure has no
        'source_traceback' and a callback is running in debug mode, that
        callback's creation traceback is added.
        """
        message = context.get('message') or 'Unhandled exception in event loop'
        exc = context.get('exception')
        exc_info = ((type(exc), exc, exc.__traceback__) if exc is not None
                    else False)

        current = self._current_handle
        if ('source_traceback' not in context and current is not None
                and current._source_traceback):
            context['handle_traceback'] = current._source_traceback

        headings = {
            'source_traceback': 'Object created at (most recent call last):\n',
            'handle_traceback': 'Handle created at (most recent call last):\n',
        }
        lines = [message]
        for key in sorted(context):
            if key in ('message', 'exception'):
                continue
            value = context[key]
            if key in headings:
                text = headings[key] + ''.join(
                    traceback.format_list(value)).rstrip()
            else:
                text = repr(value)
            lines.append(f'{key}: {text}')
        logger.error('\n'.join(lines), exc_info=exc_info)

    def call_exception_handler(self, context):
        """Report an error that nobody else can handle.

        *context* is a dict with at least 'message'; optional keys include
        'exception', 'future', 'task', 'handle', 'protocol', 'transport',
        'socket' and 'asyncgen'.  The installed handler (or the default
        one) is called; errors raised by handlers are logged, never
        propagated.
        """
        handler = self._exception_handler
        if handler is None:
            try:
                self.default_exception_handler(context)
            except (SystemExit, KeyboardInterrupt):
                raise
            except BaseException:
                # A broken default handler (in a subclass) must not take
                # the loop down; log through the module logger instead.
                logger.error('Exception in default exception handler',
                             exc_info=True)
            return

        try:
            # The handler sees the context variables of the object that
            # failed, as the failing code did.
            owner = (context.get("task") or context.get("future")
                     or context.get("handle"))
            get_context = getattr(owner, "get_context", None)
            owner_ctx = get_context() if get_context is not None else None
            if owner_ctx is not None:
                owner_ctx.run(handler, self, context)
            else:
                handler(self, context)
        except (SystemExit, KeyboardInterrupt):
            raise
        except BaseException as exc:
            try:
                self.default_exception_handler({
                    'message': 'Unhandled error in exception handler',
                    'exception': exc,
                    'context': context,
                })
            except (SystemExit, KeyboardInterrupt):
                raise
            except BaseException:
                logger.error('Exception in default exception handler '
                             'while handling an unexpected error '
                             'in custom exception handler',
                             exc_info=True)


def _remove_stale_unix_socket(path):
    """Remove a socket file left behind at *path* by an earlier process."""
    try:
        if stat.S_ISSOCK(os.stat(path).st_mode):
            os.remove(path)
    except FileNotFoundError:
        pass
    except OSError as err:
        logger.error('Unable to check or remove stale UNIX socket %r: %r',
                     path, err)
