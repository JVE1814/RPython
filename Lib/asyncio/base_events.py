"""Event loop machinery independent of the I/O multiplexing mechanism.

BaseEventLoop implements scheduling (ready queue + timer heap), the run
loop, executors, name resolution, connection/server/datagram setup on top
of the transport factories (_make_*_transport) that concrete loops supply,
sendfile and error reporting.
"""

import collections
import collections.abc

import errno
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
except ImportError:  # no TLS support compiled in
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


# Rebuild the timer heap when more than this fraction of at least this
# many timers is cancelled.
_MIN_SCHEDULED_TIMER_HANDLES = 100
_MIN_CANCELLED_TIMER_HANDLES_FRACTION = 0.5

_HAS_IPv6 = hasattr(socket, 'AF_INET6')

# select() timeouts are capped (some platforms reject huge values).
MAXIMUM_SELECT_TIMEOUT = 24 * 3600


def _format_handle(handle):
    owner = getattr(handle._callback, '__self__', None)
    if isinstance(owner, tasks.Task):
        return repr(owner)
    return str(handle)


def _format_pipe(fd):
    names = {subprocess.PIPE: '<pipe>', subprocess.STDOUT: '<stdout>'}
    return names.get(fd, repr(fd))


def _set_reuseport(sock):
    if not hasattr(socket, 'SO_REUSEPORT'):
        raise ValueError('reuse_port not supported by socket module')
    try:
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEPORT, 1)
    except OSError:
        raise ValueError('reuse_port not supported by socket module, '
                         'SO_REUSEPORT defined but not implemented.')


def _ipaddr_info(host, port, family, type, proto, flowinfo=0, scopeid=0):
    """Resolve numeric (host, port) without getaddrinfo(); None if host is
    not a numeric address or the parameters need a real lookup."""
    if not hasattr(socket, 'inet_pton') or host is None:
        return None
    if proto not in (0, socket.IPPROTO_TCP, socket.IPPROTO_UDP):
        return None
    implied = {socket.SOCK_STREAM: socket.IPPROTO_TCP,
               socket.SOCK_DGRAM: socket.IPPROTO_UDP}
    if type not in implied:
        return None
    proto = implied[type]
    if port is None or port in (b'', ''):
        port = 0
    else:
        try:
            port = int(port)
        except (TypeError, ValueError):
            return None   # a service name: let getaddrinfo() handle it
    if isinstance(host, bytes):
        host = host.decode('idna')
    if '%' in host:
        return None   # scoped IPv6 addresses need getaddrinfo()
    if family == socket.AF_UNSPEC:
        candidates = [socket.AF_INET] + ([socket.AF_INET6] if _HAS_IPv6 else [])
    else:
        candidates = [family]
    for af in candidates:
        try:
            socket.inet_pton(af, host)
        except OSError:
            continue
        if _HAS_IPv6 and af == socket.AF_INET6:
            return af, type, proto, '', (host, port, flowinfo, scopeid)
        return af, type, proto, '', (host, port)
    return None


def _interleave_addrinfos(addrinfos, first_address_family_count=1):
    """Reorder addrinfos alternating between address families (RFC 8305),
    keeping *first_address_family_count* entries of the first family in
    front."""
    by_family = {}
    for info in addrinfos:
        by_family.setdefault(info[0], []).append(info)
    groups = list(by_family.values())
    if not groups:
        return []
    head = groups[0][:first_address_family_count - 1]
    groups[0] = groups[0][first_address_family_count - 1:]
    ordered = list(head)
    for row in itertools.zip_longest(*groups):
        ordered.extend(item for item in row if item is not None)
    return ordered


def _run_until_complete_cb(fut):
    if not fut.cancelled():
        exc = fut.exception()
        if isinstance(exc, (SystemExit, KeyboardInterrupt)):
            # run_forever() is already unwinding
            return
    futures._get_loop(fut).stop()


def _thread_pool_class():
    # concurrent.futures is only needed once an executor is used
    import concurrent.futures
    return concurrent.futures.ThreadPoolExecutor


def _set_nodelay(sock):
    if not hasattr(socket, 'TCP_NODELAY'):
        return
    if (sock.family in (socket.AF_INET, socket.AF_INET6) and
            sock.type == socket.SOCK_STREAM and
            sock.proto == socket.IPPROTO_TCP):
        sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)


def _check_ssl_socket(sock):
    if ssl is not None and isinstance(sock, ssl.SSLSocket):
        raise TypeError("Socket cannot be of type SSLSocket")


class _SendfileFallbackProtocol(protocols.Protocol):
    """Temporarily replaces a transport's protocol while loop.sendfile()
    pushes a file through transport.write()."""

    def __init__(self, transp):
        if not isinstance(transp, transports._FlowControlMixin):
            raise TypeError("transport should be _FlowControlMixin instance")
        self._transport = transp
        self._proto = transp.get_protocol()
        self._should_resume_reading = transp.is_reading()
        self._should_resume_writing = transp._protocol_paused
        transp.pause_reading()
        transp.set_protocol(self)
        self._write_ready_fut = (transp._loop.create_future()
                                 if self._should_resume_writing else None)

    async def drain(self):
        if self._transport.is_closing():
            raise ConnectionError("Connection closed by peer")
        if self._write_ready_fut is not None:
            await self._write_ready_fut

    def connection_made(self, transport):
        raise RuntimeError("Invalid state: "
                           "connection should have been established already.")

    def connection_lost(self, exc):
        if self._write_ready_fut is not None:
            self._write_ready_fut.set_exception(
                exc if exc is not None
                else ConnectionError("Connection is closed by peer"))
        self._proto.connection_lost(exc)

    def pause_writing(self):
        if self._write_ready_fut is None:
            self._write_ready_fut = self._transport._loop.create_future()

    def resume_writing(self):
        if self._write_ready_fut is not None:
            self._write_ready_fut.set_result(False)
            self._write_ready_fut = None

    def data_received(self, data):
        raise RuntimeError("Invalid state: reading should be paused")

    def eof_received(self):
        raise RuntimeError("Invalid state: reading should be paused")

    async def restore(self):
        self._transport.set_protocol(self._proto)
        if self._should_resume_reading:
            self._transport.resume_reading()
        if self._write_ready_fut is not None:
            self._write_ready_fut.cancel()
        if self._should_resume_writing:
            self._proto.resume_writing()


class Server(events.AbstractServer):
    """Listening sockets accepting connections for a protocol factory."""

    def __init__(self, loop, sockets, protocol_factory, ssl_context, backlog,
                 ssl_handshake_timeout, ssl_shutdown_timeout=None):
        self._loop = loop
        self._sockets = sockets
        self._clients = weakref.WeakSet()   # live client transports
        self._waiters = []                  # wait_closed() futures
        self._protocol_factory = protocol_factory
        self._backlog = backlog
        self._ssl_context = ssl_context
        self._ssl_handshake_timeout = ssl_handshake_timeout
        self._ssl_shutdown_timeout = ssl_shutdown_timeout
        self._serving = False
        self._serving_forever_fut = None

    def __repr__(self):
        return f'<{type(self).__name__} sockets={self.sockets!r}>'

    def _attach(self, transport):
        assert self._sockets is not None
        self._clients.add(transport)

    def _detach(self, transport):
        self._clients.discard(transport)
        if not self._clients and self._sockets is None:
            self._wakeup()

    def _wakeup(self):
        pending, self._waiters = self._waiters, None
        for fut in pending or ():
            if not fut.done():
                fut.set_result(None)

    def _start_serving(self):
        if self._serving:
            return
        self._serving = True
        for sock in self._sockets:
            sock.listen(self._backlog)
            self._loop._start_serving(
                self._protocol_factory, sock, self._ssl_context, self,
                self._backlog, self._ssl_handshake_timeout,
                self._ssl_shutdown_timeout)

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
        listening, self._sockets = self._sockets, None
        if listening is None:
            return
        for sock in listening:
            self._loop._stop_serving(sock)
        self._serving = False
        forever = self._serving_forever_fut
        if forever is not None and not forever.done():
            forever.cancel()
            self._serving_forever_fut = None
        if not self._clients:
            self._wakeup()

    def close_clients(self):
        for transport in list(self._clients):
            transport.close()

    def abort_clients(self):
        for transport in list(self._clients):
            transport.abort()

    async def start_serving(self):
        self._start_serving()
        # let the add_reader() calls take effect
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
            try:
                self.close()
                # being cancelled means shutting down: drop the clients too
                self.close_clients()
                await self.wait_closed()
            finally:
                raise
        finally:
            self._serving_forever_fut = None

    async def wait_closed(self):
        """Wait until the server is closed and all clients disconnected."""
        if self._waiters is None:
            return
        fut = self._loop.create_future()
        self._waiters.append(fut)
        await fut


class BaseEventLoop(events.AbstractEventLoop):

    def __init__(self):
        self._timer_cancelled_count = 0
        self._closed = False
        self._stopping = False
        self._ready = collections.deque()
        self._scheduled = []
        self._default_executor = None
        self._internal_fds = 0
        self._thread_id = None        # thread running the loop, if any
        self._clock_resolution = time.get_clock_info('monotonic').resolution
        self._exception_handler = None
        self.set_debug(coroutines._is_debug_mode())
        self._old_agen_hooks = None
        # debug mode reports callbacks running longer than this (seconds)
        self.slow_callback_duration = 0.1
        self._current_handle = None
        self._task_factory = None
        self._coroutine_origin_tracking_enabled = False
        self._coroutine_origin_tracking_saved_depth = None
        self._asyncgens = weakref.WeakSet()
        self._asyncgens_shutdown_called = False
        self._executor_shutdown_called = False

    def __repr__(self):
        return (f'<{type(self).__name__} running={self.is_running()} '
                f'closed={self.is_closed()} debug={self.get_debug()}>')

    def __del__(self, _warn=warnings.warn):
        if not self.is_closed():
            _warn(f"unclosed event loop {self!r}", ResourceWarning, source=self)
            if not self.is_running():
                self.close()

    # ------------------------------------------------------------ factories

    def create_future(self):
        """A new Future bound to this loop."""
        return futures.Future(loop=self)

    def create_task(self, coro, **kwargs):
        """Schedule a coroutine (through the task factory if one is set)."""
        self._check_closed()
        if self._task_factory is not None:
            return self._task_factory(self, coro, **kwargs)
        task = tasks.Task(coro, loop=self, **kwargs)
        if task._source_traceback:
            task._source_traceback.pop()
        try:
            return task
        finally:
            del task

    def set_task_factory(self, factory):
        if factory is not None and not callable(factory):
            raise TypeError('task factory must be a callable or None')
        self._task_factory = factory

    def get_task_factory(self):
        return self._task_factory

    def _make_socket_transport(self, sock, protocol, waiter=None, *,
                               extra=None, server=None):
        raise NotImplementedError

    def _make_ssl_transport(
            self, rawsock, protocol, sslcontext, waiter=None,
            *, server_side=False, server_hostname=None,
            extra=None, server=None,
            ssl_handshake_timeout=None,
            ssl_shutdown_timeout=None,
            call_connection_made=True):
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
        raise NotImplementedError

    def _process_events(self, event_list):
        raise NotImplementedError

    # ------------------------------------------------------------ state checks

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
        # only meaningful while the loop runs (debug mode)
        if self._thread_id is not None and \
                threading.get_ident() != self._thread_id:
            raise RuntimeError(
                "Non-thread-safe operation invoked on an event loop other "
                "than the current one")

    def _check_callback(self, callback, method):
        if coroutines.iscoroutine(callback) or \
                coroutines._iscoroutinefunction(callback):
            raise TypeError(f"coroutines cannot be used with {method}()")
        if not callable(callback):
            raise TypeError(f'a callable object was expected by {method}(), '
                            f'got {callback!r}')

    def is_running(self):
        return self._thread_id is not None

    def is_closed(self):
        return self._closed

    # ------------------------------------------------------------ async gens

    def _asyncgen_finalizer_hook(self, agen):
        self._asyncgens.discard(agen)
        if not self.is_closed():
            self.call_soon_threadsafe(self.create_task, agen.aclose())

    def _asyncgen_firstiter_hook(self, agen):
        if self._asyncgens_shutdown_called:
            warnings.warn(
                f"asynchronous generator {agen!r} was scheduled after "
                f"loop.shutdown_asyncgens() call",
                ResourceWarning, source=self)
        self._asyncgens.add(agen)

    async def shutdown_asyncgens(self):
        """Close every async generator still alive in this loop."""
        self._asyncgens_shutdown_called = True
        if not len(self._asyncgens):
            return
        agens = list(self._asyncgens)
        self._asyncgens.clear()
        outcomes = await tasks.gather(*(ag.aclose() for ag in agens),
                                      return_exceptions=True)
        for agen, outcome in zip(agens, outcomes):
            if isinstance(outcome, Exception):
                self.call_exception_handler({
                    'message': f'an error occurred during closing of '
                               f'asynchronous generator {agen!r}',
                    'exception': outcome,
                    'asyncgen': agen,
                })

    # ------------------------------------------------------------ executors

    async def shutdown_default_executor(self, timeout=None):
        """Shut the default executor down without blocking the loop."""
        self._executor_shutdown_called = True
        if self._default_executor is None:
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
            self._default_executor.shutdown(wait=False)
        else:
            joiner.join()

    def _do_shutdown(self, future):
        try:
            self._default_executor.shutdown(wait=True)
            if not self.is_closed():
                self.call_soon_threadsafe(
                    futures._set_result_unless_cancelled, future, None)
        except Exception as exc:
            if not self.is_closed() and not future.cancelled():
                self.call_soon_threadsafe(future.set_exception, exc)

    def run_in_executor(self, executor, func, *args):
        """Run func(*args) in *executor* (None: the default thread pool)."""
        self._check_closed()
        if self._debug:
            self._check_callback(func, 'run_in_executor')
        if executor is None:
            executor = self._default_executor
            self._check_default_executor()
            if executor is None:
                executor = _thread_pool_class()(
                    thread_name_prefix='asyncio')
                self._default_executor = executor
        return futures.wrap_future(executor.submit(func, *args), loop=self)

    def set_default_executor(self, executor):
        if not isinstance(executor, _thread_pool_class()):
            raise TypeError('executor must be ThreadPoolExecutor instance')
        self._default_executor = executor

    # ------------------------------------------------------------ running

    def _run_forever_setup(self):
        self._check_closed()
        self._check_running()
        self._set_coroutine_origin_tracking(self._debug)
        self._old_agen_hooks = sys.get_asyncgen_hooks()
        self._thread_id = threading.get_ident()
        sys.set_asyncgen_hooks(firstiter=self._asyncgen_firstiter_hook,
                               finalizer=self._asyncgen_finalizer_hook)
        events._set_running_loop(self)

    def _run_forever_cleanup(self):
        self._stopping = False
        self._thread_id = None
        events._set_running_loop(None)
        self._set_coroutine_origin_tracking(False)
        if self._old_agen_hooks is not None:
            sys.set_asyncgen_hooks(*self._old_agen_hooks)
            self._old_agen_hooks = None

    def run_forever(self):
        """Run until stop() is called."""
        self._run_forever_setup()
        try:
            while True:
                self._run_once()
                if self._stopping:
                    break
        finally:
            self._run_forever_cleanup()

    def run_until_complete(self, future):
        """Run until *future* (or awaitable) is done; return its result."""
        self._check_closed()
        self._check_running()
        wrapped = not futures.isfuture(future)
        future = tasks.ensure_future(future, loop=self)
        if wrapped:
            # the caller cannot see this task: an exception that escapes is
            # reported through the raise below, not by __del__
            future._log_destroy_pending = False
        future.add_done_callback(_run_until_complete_cb)
        try:
            self.run_forever()
        except:
            if wrapped and future.done() and not future.cancelled():
                future.exception()   # retrieved: don't log it again
            raise
        finally:
            future.remove_done_callback(_run_until_complete_cb)
        if not future.done():
            raise RuntimeError('Event loop stopped before Future completed.')
        return future.result()

    def stop(self):
        """Stop after the callbacks of the current iteration have run."""
        self._stopping = True

    def close(self):
        """Close the loop (it must not be running)."""
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

    # ------------------------------------------------------------ scheduling

    def time(self):
        """The loop's clock (monotonic seconds)."""
        return time.monotonic()

    def call_later(self, delay, callback, *args, context=None):
        """Call callback(*args) after *delay* seconds; returns a TimerHandle."""
        if delay is None:
            raise TypeError('delay must not be None')
        timer = self.call_at(self.time() + delay, callback, *args,
                             context=context)
        if timer._source_traceback:
            timer._source_traceback.pop()
        return timer

    def call_at(self, when, callback, *args, context=None):
        """Call callback(*args) at loop time *when*."""
        if when is None:
            raise TypeError("when cannot be None")
        self._check_closed()
        if self._debug:
            self._check_thread()
            self._check_callback(callback, 'call_at')
        timer = events.TimerHandle(when, callback, args, self, context)
        if timer._source_traceback:
            timer._source_traceback.pop()
        heapq.heappush(self._scheduled, timer)
        timer._scheduled = True
        return timer

    def call_soon(self, callback, *args, context=None):
        """Call callback(*args) on the next loop iteration (FIFO)."""
        self._check_closed()
        if self._debug:
            self._check_thread()
            self._check_callback(callback, 'call_soon')
        handle = self._call_soon(callback, args, context)
        if handle._source_traceback:
            handle._source_traceback.pop()
        return handle

    def _call_soon(self, callback, args, context):
        handle = events.Handle(callback, args, self, context)
        if handle._source_traceback:
            handle._source_traceback.pop()
        self._ready.append(handle)
        return handle

    def call_soon_threadsafe(self, callback, *args, context=None):
        """call_soon() usable from other threads (wakes the loop up)."""
        self._check_closed()
        if self._debug:
            self._check_callback(callback, 'call_soon_threadsafe')
        handle = self._call_soon(callback, args, context)
        if handle._source_traceback:
            handle._source_traceback.pop()
        self._write_to_self()
        return handle

    def _add_callback(self, handle):
        """Queue an existing Handle (no-op if it was cancelled)."""
        assert isinstance(handle, events.Handle), 'A Handle is required here'
        if not handle._cancelled:
            self._ready.append(handle)

    def _add_callback_signalsafe(self, handle):
        self._add_callback(handle)
        self._write_to_self()

    def _timer_handle_cancelled(self, handle):
        if handle._scheduled:
            self._timer_cancelled_count += 1

    def _run_once(self):
        """One iteration: poll for I/O, then run all callbacks due now."""
        heap = self._scheduled
        if (len(heap) > _MIN_SCHEDULED_TIMER_HANDLES and
                self._timer_cancelled_count / len(heap) >
                _MIN_CANCELLED_TIMER_HANDLES_FRACTION):
            survivors = []
            for timer in heap:
                if timer._cancelled:
                    timer._scheduled = False
                else:
                    survivors.append(timer)
            heapq.heapify(survivors)
            heap[:] = survivors
            self._timer_cancelled_count = 0
        else:
            while heap and heap[0]._cancelled:
                self._timer_cancelled_count -= 1
                heapq.heappop(heap)._scheduled = False

        if self._ready or self._stopping:
            timeout = 0
        elif heap:
            timeout = min(max(0, heap[0]._when - self.time()),
                          MAXIMUM_SELECT_TIMEOUT)
        else:
            timeout = None

        ready_io = self._selector.select(timeout)
        self._process_events(ready_io)
        ready_io = None

        # Timers due within the clock resolution count as due now.
        horizon = self.time() + self._clock_resolution
        while heap and heap[0]._when < horizon:
            timer = heapq.heappop(heap)
            timer._scheduled = False
            self._ready.append(timer)

        # Only what is queued now runs in this iteration; callbacks queued
        # by these callbacks wait for the next one.
        for _ in range(len(self._ready)):
            handle = self._ready.popleft()
            if handle._cancelled:
                continue
            if not self._debug:
                handle._run()
                continue
            self._current_handle = handle
            try:
                started = self.time()
                handle._run()
                took = self.time() - started
                if took >= self.slow_callback_duration:
                    logger.warning('Executing %s took %.3f seconds',
                                   _format_handle(handle), took)
            finally:
                self._current_handle = None
        handle = None

    def _set_coroutine_origin_tracking(self, enabled):
        if bool(enabled) == bool(self._coroutine_origin_tracking_enabled):
            return
        if enabled:
            self._coroutine_origin_tracking_saved_depth = \
                sys.get_coroutine_origin_tracking_depth()
            sys.set_coroutine_origin_tracking_depth(constants.DEBUG_STACK_DEPTH)
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

    # ------------------------------------------------------------ resolution

    def _getaddrinfo_debug(self, host, port, family, type, proto, flags):
        what = [f"{host}:{port!r}"]
        for label, value in (('family', family), ('type', type),
                             ('proto', proto), ('flags', flags)):
            if value:
                what.append(f'{label}={value!r}')
        description = ', '.join(what)
        logger.debug('Get address info %s', description)
        started = self.time()
        found = socket.getaddrinfo(host, port, family, type, proto, flags)
        took = self.time() - started
        message = f'Getting address info {description} took {took * 1e3:.3f}ms: {found!r}'
        if took >= self.slow_callback_duration:
            logger.info(message)
        else:
            logger.debug(message)
        return found

    async def getaddrinfo(self, host, port, *,
                          family=0, type=0, proto=0, flags=0):
        resolve = self._getaddrinfo_debug if self._debug else socket.getaddrinfo
        return await self.run_in_executor(None, resolve, host, port,
                                          family, type, proto, flags)

    async def getnameinfo(self, sockaddr, flags=0):
        return await self.run_in_executor(None, socket.getnameinfo,
                                          sockaddr, flags)

    async def _ensure_resolved(self, address, *,
                               family=0, type=socket.SOCK_STREAM,
                               proto=0, flags=0, loop):
        host, port = address[:2]
        numeric = _ipaddr_info(host, port, family, type, proto, *address[2:])
        if numeric is not None:
            return [numeric]
        return await loop.getaddrinfo(host, port, family=family, type=type,
                                      proto=proto, flags=flags)

    # ------------------------------------------------------------ sendfile

    async def sock_sendfile(self, sock, file, offset=0, count=None,
                            *, fallback=True):
        """Send *file* over the non-blocking socket *sock*."""
        if sock.gettimeout() != 0:
            raise ValueError("the socket must be non-blocking")
        _check_ssl_socket(sock)
        self._check_sendfile_params(sock, file, offset, count)
        try:
            return await self._sock_sendfile_native(sock, file, offset, count)
        except exceptions.SendfileNotAvailableError:
            if not fallback:
                raise
        return await self._sock_sendfile_fallback(sock, file, offset, count)

    async def _sock_sendfile_native(self, sock, file, offset, count):
        raise exceptions.SendfileNotAvailableError(
            f"syscall sendfile is not available for socket {sock!r} "
            f"and file {file!r} combination")

    async def _sock_sendfile_fallback(self, sock, file, offset, count):
        file.seek(offset)
        chunk = (min(count, constants.SENDFILE_FALLBACK_READBUFFER_SIZE)
                 if count else constants.SENDFILE_FALLBACK_READBUFFER_SIZE)
        buf = bytearray(chunk)
        sent = 0
        try:
            while True:
                want = min(count - sent, chunk) if count else chunk
                if want <= 0:
                    break
                view = memoryview(buf)[:want]
                n = await self.run_in_executor(None, file.readinto, view)
                if not n:
                    break
                await self.sock_sendall(sock, view[:n])
                sent += n
            return sent
        finally:
            if hasattr(file, "seek"):
                file.seek(offset + sent)

    def _check_sendfile_params(self, sock, file, offset, count):
        if 'b' not in getattr(file, 'mode', 'b'):
            raise ValueError("file should be opened in binary mode")
        if not sock.type == socket.SOCK_STREAM:
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
        """Send *file* through *transport*; return the bytes sent."""
        if transport.is_closing():
            raise RuntimeError("Transport is closing")
        mode = getattr(transport, '_sendfile_compatible',
                       constants._SendfileMode.UNSUPPORTED)
        if mode is constants._SendfileMode.UNSUPPORTED:
            raise RuntimeError(
                f"sendfile is not supported for transport {transport!r}")
        if mode is constants._SendfileMode.TRY_NATIVE:
            try:
                return await self._sendfile_native(transport, file,
                                                   offset, count)
            except exceptions.SendfileNotAvailableError:
                if not fallback:
                    raise
        if not fallback:
            raise RuntimeError(
                f"fallback is disabled and native sendfile is not "
                f"supported for transport {transport!r}")
        return await self._sendfile_fallback(transport, file, offset, count)

    async def _sendfile_native(self, transp, file, offset, count):
        raise exceptions.SendfileNotAvailableError(
            "sendfile syscall is not supported")

    async def _sendfile_fallback(self, transp, file, offset, count):
        file.seek(offset)
        chunk = min(count, 16384) if count else 16384
        buf = bytearray(chunk)
        sent = 0
        proto = _SendfileFallbackProtocol(transp)
        try:
            while True:
                want = min(count - sent, chunk) if count else chunk
                if want <= 0:
                    return sent
                view = memoryview(buf)[:want]
                n = await self.run_in_executor(None, file.readinto, view)
                if not n:
                    return sent
                await proto.drain()
                transp.write(view[:n])
                sent += n
        finally:
            if hasattr(file, "seek"):
                file.seek(offset + sent)
            await proto.restore()

    # ------------------------------------------------------------ TLS

    async def start_tls(self, transport, protocol, sslcontext, *,
                        server_side=False,
                        server_hostname=None,
                        ssl_handshake_timeout=None,
                        ssl_shutdown_timeout=None):
        """Upgrade an existing connection to TLS."""
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
        ssl_protocol = sslproto.SSLProtocol(
            self, protocol, sslcontext, waiter, server_side, server_hostname,
            ssl_handshake_timeout=ssl_handshake_timeout,
            ssl_shutdown_timeout=ssl_shutdown_timeout,
            call_connection_made=False)
        transport.pause_reading()
        transport.set_protocol(ssl_protocol)
        resume = self.call_soon(ssl_protocol.connection_made, transport)
        again = self.call_soon(transport.resume_reading)
        try:
            await waiter
        except BaseException:
            transport.close()
            resume.cancel()
            again.cancel()
            raise
        return ssl_protocol._app_transport

    # ------------------------------------------------------------ connections

    async def _connect_sock(self, exceptions, addr_info, local_addr_infos=None):
        """Create a socket for *addr_info* and connect it (errors are
        appended to *exceptions*)."""
        my_exceptions = []
        exceptions.append(my_exceptions)
        family, type_, proto, _, address = addr_info
        sock = None
        try:
            sock = socket.socket(family=family, type=type_, proto=proto)
            sock.setblocking(False)
            if local_addr_infos is not None:
                for lfamily, _, _, _, laddr in local_addr_infos:
                    if lfamily != family:
                        continue
                    try:
                        sock.bind(laddr)
                        break
                    except OSError as exc:
                        my_exceptions.append(OSError(
                            exc.errno,
                            f'error while attempting to bind on address '
                            f'{laddr!r}: {str(exc.strerror).lower()}'))
                else:
                    if my_exceptions:
                        raise my_exceptions.pop()
                    raise OSError(f"no matching local address with "
                                  f"family={family} found")
            await self.sock_connect(sock, address)
            return sock
        except OSError as exc:
            my_exceptions.append(exc)
            if sock is not None:
                sock.close()
            raise
        except:
            if sock is not None:
                try:
                    sock.close()
                except OSError:
                    pass    # keep the original error
            raise
        finally:
            exceptions = my_exceptions = None

    async def create_connection(
            self, protocol_factory, host=None, port=None,
            *, ssl=None, family=0,
            proto=0, flags=0, sock=None,
            local_addr=None, server_hostname=None,
            ssl_handshake_timeout=None,
            ssl_shutdown_timeout=None,
            happy_eyeballs_delay=None, interleave=None,
            all_errors=False):
        """Open a stream connection; returns (transport, protocol)."""
        if server_hostname is not None and not ssl:
            raise ValueError('server_hostname is only meaningful with ssl')
        if server_hostname is None and ssl:
            if not host:
                raise ValueError('You must set server_hostname '
                                 'when using ssl without a host')
            server_hostname = host
        if ssl_handshake_timeout is not None and not ssl:
            raise ValueError(
                'ssl_handshake_timeout is only meaningful with ssl')
        if ssl_shutdown_timeout is not None and not ssl:
            raise ValueError(
                'ssl_shutdown_timeout is only meaningful with ssl')
        if sock is not None:
            _check_ssl_socket(sock)
        if happy_eyeballs_delay is not None and interleave is None:
            # RFC 8305 recommends interleaving when racing
            interleave = 1

        if host is not None or port is not None:
            if sock is not None:
                raise ValueError(
                    'host/port and sock can not be specified at the same time')
            infos = await self._ensure_resolved(
                (host, port), family=family,
                type=socket.SOCK_STREAM, proto=proto, flags=flags, loop=self)
            if not infos:
                raise OSError('getaddrinfo() returned empty list')
            laddr_infos = None
            if local_addr is not None:
                laddr_infos = await self._ensure_resolved(
                    local_addr, family=family,
                    type=socket.SOCK_STREAM, proto=proto,
                    flags=flags, loop=self)
                if not laddr_infos:
                    raise OSError('getaddrinfo() returned empty list')
            if interleave:
                infos = _interleave_addrinfos(infos, interleave)

            errors = []
            if happy_eyeballs_delay is None:
                for info in infos:
                    try:
                        sock = await self._connect_sock(errors, info,
                                                        laddr_infos)
                        break
                    except OSError:
                        continue
            else:
                sock = (await staggered.staggered_race(
                    (lambda info=info: self._connect_sock(errors, info,
                                                          laddr_infos)
                     for info in infos),
                    happy_eyeballs_delay, loop=self))[0]

            if sock is None:
                flat = [exc for group in errors for exc in group]
                try:
                    if all_errors:
                        raise ExceptionGroup("create_connection failed", flat)
                    if not flat:
                        raise TimeoutError("create_connection failed")
                    if len(flat) == 1:
                        raise flat[0]
                    first = str(flat[0])
                    if all(str(exc) == first for exc in flat):
                        raise flat[0]
                    raise OSError('Multiple exceptions: {}'.format(
                        ', '.join(str(exc) for exc in flat)))
                finally:
                    flat = errors = None
        else:
            if sock is None:
                raise ValueError(
                    'host and port was not specified and no sock specified')
            if sock.type != socket.SOCK_STREAM:
                raise ValueError(
                    f'A Stream Socket was expected, got {sock!r}')

        try:
            transport, protocol = await self._create_connection_transport(
                sock, protocol_factory, ssl, server_hostname,
                ssl_handshake_timeout=ssl_handshake_timeout,
                ssl_shutdown_timeout=ssl_shutdown_timeout)
        except:
            # no transport owns the socket: do not leak it
            sock.close()
            raise
        if self._debug:
            logger.debug("%r connected to %s:%r: (%r, %r)",
                         transport.get_extra_info('socket'), host, port,
                         transport, protocol)
        return transport, protocol

    async def _create_connection_transport(
            self, sock, protocol_factory, ssl,
            server_hostname, server_side=False,
            ssl_handshake_timeout=None,
            ssl_shutdown_timeout=None):
        sock.setblocking(False)
        protocol = protocol_factory()
        waiter = self.create_future()
        if ssl:
            context = None if isinstance(ssl, bool) else ssl
            if context is None:
                context = sslproto._create_transport_context(server_side,
                                                             server_hostname)
            transport = self._make_ssl_transport(
                sock, protocol, context, waiter,
                server_side=server_side, server_hostname=server_hostname,
                ssl_handshake_timeout=ssl_handshake_timeout,
                ssl_shutdown_timeout=ssl_shutdown_timeout)
        else:
            transport = self._make_socket_transport(sock, protocol, waiter)
        try:
            await waiter
        except:
            transport.close()
            raise
        return transport, protocol

    async def create_datagram_endpoint(self, protocol_factory,
                                       local_addr=None, remote_addr=None, *,
                                       family=0, proto=0, flags=0,
                                       reuse_port=None,
                                       allow_broadcast=None, sock=None):
        """Create a datagram endpoint; returns (transport, protocol)."""
        if sock is not None:
            if sock.type == socket.SOCK_STREAM:
                raise ValueError(
                    f'A datagram socket was expected, got {sock!r}')
            given = dict(local_addr=local_addr, remote_addr=remote_addr,
                         family=family, proto=proto, flags=flags,
                         reuse_port=reuse_port,
                         allow_broadcast=allow_broadcast)
            if local_addr or remote_addr or family or proto or flags or \
                    reuse_port or allow_broadcast:
                problems = ', '.join(f'{k}={v}' for k, v in given.items() if v)
                raise ValueError(
                    f'socket modifier keyword arguments can not be used '
                    f'when sock is specified. ({problems})')
            sock.setblocking(False)
            remote = None
        else:
            if not (local_addr or remote_addr):
                if family == 0:
                    raise ValueError('unexpected address family')
                pairs = [((family, proto), (None, None))]
            elif hasattr(socket, 'AF_UNIX') and family == socket.AF_UNIX:
                for addr in (local_addr, remote_addr):
                    if addr is not None and not isinstance(addr, str):
                        raise TypeError('string is expected')
                if local_addr and local_addr[0] not in (0, '\x00'):
                    try:
                        if stat.S_ISSOCK(os.stat(local_addr).st_mode):
                            os.remove(local_addr)
                    except FileNotFoundError:
                        pass
                    except OSError as err:
                        logger.error('Unable to check or remove stale UNIX '
                                     'socket %r: %r', local_addr, err)
                pairs = [((family, proto), (local_addr, remote_addr))]
            else:
                # one (family, proto) entry per family, with local/remote
                found = collections.OrderedDict()
                for slot, addr in ((0, local_addr), (1, remote_addr)):
                    if addr is None:
                        continue
                    if not (isinstance(addr, tuple) and len(addr) == 2):
                        raise TypeError('2-tuple is expected')
                    infos = await self._ensure_resolved(
                        addr, family=family, type=socket.SOCK_DGRAM,
                        proto=proto, flags=flags, loop=self)
                    if not infos:
                        raise OSError('getaddrinfo() returned empty list')
                    for fam, _, pro, _, address in infos:
                        key = (fam, pro)
                        found.setdefault(key, [None, None])[slot] = address
                pairs = [(key, tuple(addrs)) for key, addrs in found.items()
                         if not ((local_addr and addrs[0] is None) or
                                 (remote_addr and addrs[1] is None))]
                if not pairs:
                    raise ValueError('can not get address information')

            failures = []
            for (fam, pro), (local_address, remote_address) in pairs:
                sock = None
                remote = None
                try:
                    sock = socket.socket(family=fam, type=socket.SOCK_DGRAM,
                                         proto=pro)
                    if reuse_port:
                        _set_reuseport(sock)
                    if allow_broadcast:
                        sock.setsockopt(socket.SOL_SOCKET,
                                        socket.SO_BROADCAST, 1)
                    sock.setblocking(False)
                    if local_addr:
                        sock.bind(local_address)
                    if remote_addr:
                        if not allow_broadcast:
                            await self.sock_connect(sock, remote_address)
                        remote = remote_address
                except OSError as exc:
                    if sock is not None:
                        sock.close()
                    failures.append(exc)
                except:
                    if sock is not None:
                        sock.close()
                    raise
                else:
                    break
            else:
                raise failures[0]

        protocol = protocol_factory()
        waiter = self.create_future()
        transport = self._make_datagram_transport(sock, protocol, remote,
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
        except:
            transport.close()
            raise
        return transport, protocol

    async def _create_server_getaddrinfo(self, host, port, family, flags):
        infos = await self._ensure_resolved((host, port), family=family,
                                            type=socket.SOCK_STREAM,
                                            flags=flags, loop=self)
        if not infos:
            raise OSError(f'getaddrinfo({host!r}) returned empty list')
        return infos

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
        """Listen for TCP connections; returns a Server."""
        if isinstance(ssl, bool):
            raise TypeError('ssl argument must be an SSLContext or None')
        if ssl_handshake_timeout is not None and ssl is None:
            raise ValueError(
                'ssl_handshake_timeout is only meaningful with ssl')
        if ssl_shutdown_timeout is not None and ssl is None:
            raise ValueError(
                'ssl_shutdown_timeout is only meaningful with ssl')
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
            elif isinstance(host, str) or \
                    not isinstance(host, collections.abc.Iterable):
                hosts = [host]
            else:
                hosts = host
            lookups = [self._create_server_getaddrinfo(h, port, family, flags)
                       for h in hosts]
            infos = set(itertools.chain.from_iterable(
                await tasks.gather(*lookups)))

            sockets = []
            completed = False
            try:
                for fam, socktype, pro, _, address in infos:
                    try:
                        listener = socket.socket(fam, socktype, pro)
                    except socket.error:
                        if self._debug:
                            logger.warning('create_server() failed to create '
                                           'socket.socket(%r, %r, %r)',
                                           fam, socktype, pro, exc_info=True)
                        continue
                    sockets.append(listener)
                    if reuse_address:
                        listener.setsockopt(socket.SOL_SOCKET,
                                            socket.SO_REUSEADDR, True)
                    if reuse_port and fam in (socket.AF_INET, socket.AF_INET6):
                        _set_reuseport(listener)
                    if keep_alive:
                        listener.setsockopt(socket.SOL_SOCKET,
                                            socket.SO_KEEPALIVE, True)
                    # IPv6 sockets must not also grab the IPv4 port
                    if (_HAS_IPv6 and fam == socket.AF_INET6 and
                            hasattr(socket, 'IPPROTO_IPV6')):
                        listener.setsockopt(socket.IPPROTO_IPV6,
                                            socket.IPV6_V6ONLY, True)
                    try:
                        listener.bind(address)
                    except OSError as err:
                        raise OSError(err.errno,
                                      f'error while attempting to bind on '
                                      f'address {address!r}: '
                                      f'{str(err.strerror).lower()}') from None
                completed = True
            finally:
                if not completed:
                    for listener in sockets:
                        listener.close()
        else:
            if sock is None:
                raise ValueError('Neither host/port nor sock were specified')
            if sock.type != socket.SOCK_STREAM:
                raise ValueError(f'A Stream Socket was expected, got {sock!r}')
            sockets = [sock]

        for listener in sockets:
            listener.setblocking(False)

        server = Server(self, sockets, protocol_factory, ssl, backlog,
                        ssl_handshake_timeout, ssl_shutdown_timeout)
        if start_serving:
            server._start_serving()
            # let the readers get registered before returning
            await tasks.sleep(0)
        if self._debug:
            logger.info("%r is serving", server)
        return server

    async def connect_accepted_socket(
            self, protocol_factory, sock,
            *, ssl=None,
            ssl_handshake_timeout=None,
            ssl_shutdown_timeout=None):
        """Wrap an already accepted connection; returns
        (transport, protocol)."""
        if sock.type != socket.SOCK_STREAM:
            raise ValueError(f'A Stream Socket was expected, got {sock!r}')
        if ssl_handshake_timeout is not None and not ssl:
            raise ValueError(
                'ssl_handshake_timeout is only meaningful with ssl')
        if ssl_shutdown_timeout is not None and not ssl:
            raise ValueError(
                'ssl_shutdown_timeout is only meaningful with ssl')
        _check_ssl_socket(sock)
        transport, protocol = await self._create_connection_transport(
            sock, protocol_factory, ssl, '', server_side=True,
            ssl_handshake_timeout=ssl_handshake_timeout,
            ssl_shutdown_timeout=ssl_shutdown_timeout)
        if self._debug:
            logger.debug("%r handled: (%r, %r)",
                         transport.get_extra_info('socket'),
                         transport, protocol)
        return transport, protocol

    # ------------------------------------------------------------ pipes

    async def connect_read_pipe(self, protocol_factory, pipe):
        protocol = protocol_factory()
        waiter = self.create_future()
        transport = self._make_read_pipe_transport(pipe, protocol, waiter)
        try:
            await waiter
        except:
            transport.close()
            raise
        if self._debug:
            logger.debug('Read pipe %r connected: (%r, %r)',
                         pipe.fileno(), transport, protocol)
        return transport, protocol

    async def connect_write_pipe(self, protocol_factory, pipe):
        protocol = protocol_factory()
        waiter = self.create_future()
        transport = self._make_write_pipe_transport(pipe, protocol, waiter)
        try:
            await waiter
        except:
            transport.close()
            raise
        if self._debug:
            logger.debug('Write pipe %r connected: (%r, %r)',
                         pipe.fileno(), transport, protocol)
        return transport, protocol

    # ------------------------------------------------------------ processes

    def _log_subprocess(self, msg, stdin, stdout, stderr):
        parts = [msg]
        if stdin is not None:
            parts.append(f'stdin={_format_pipe(stdin)}')
        if stdout is not None and stderr == subprocess.STDOUT:
            parts.append(f'stdout=stderr={_format_pipe(stdout)}')
        else:
            if stdout is not None:
                parts.append(f'stdout={_format_pipe(stdout)}')
            if stderr is not None:
                parts.append(f'stderr={_format_pipe(stderr)}')
        logger.debug(' '.join(parts))

    def _check_subprocess_options(self, universal_newlines, shell, bufsize,
                                  encoding, errors, text):
        if universal_newlines:
            raise ValueError("universal_newlines must be False")
        if not shell:
            raise ValueError("shell must be True")
        if bufsize != 0:
            raise ValueError("bufsize must be 0")
        if text:
            raise ValueError("text must be False")
        if encoding is not None:
            raise ValueError("encoding must be None")
        if errors is not None:
            raise ValueError("errors must be None")

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
        self._check_subprocess_options(universal_newlines, shell, bufsize,
                                       encoding, errors, text)
        protocol = protocol_factory()
        debug_log = None
        if self._debug:
            debug_log = 'run shell command %r' % cmd
            self._log_subprocess(debug_log, stdin, stdout, stderr)
        transport = await self._make_subprocess_transport(
            protocol, cmd, True, stdin, stdout, stderr, bufsize, **kwargs)
        if self._debug and debug_log is not None:
            logger.info('%s: %r', debug_log, transport)
        return transport, protocol

    async def subprocess_exec(self, protocol_factory, program, *args,
                              stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, universal_newlines=False,
                              shell=False, bufsize=0,
                              encoding=None, errors=None, text=None,
                              **kwargs):
        if universal_newlines:
            raise ValueError("universal_newlines must be False")
        if shell:
            raise ValueError("shell must be False")
        if bufsize != 0:
            raise ValueError("bufsize must be 0")
        if text:
            raise ValueError("text must be False")
        if encoding is not None:
            raise ValueError("encoding must be None")
        if errors is not None:
            raise ValueError("errors must be None")
        popen_args = (program,) + args
        protocol = protocol_factory()
        debug_log = None
        if self._debug:
            debug_log = f'execute program {program!r}'
            self._log_subprocess(debug_log, stdin, stdout, stderr)
        transport = await self._make_subprocess_transport(
            protocol, popen_args, False, stdin, stdout, stderr,
            bufsize, **kwargs)
        if self._debug and debug_log is not None:
            logger.info('%s: %r', debug_log, transport)
        return transport, protocol

    # ------------------------------------------------------------ errors

    def get_exception_handler(self):
        return self._exception_handler

    def set_exception_handler(self, handler):
        """Install handler(loop, context) for errors (None: default)."""
        if handler is not None and not callable(handler):
            raise TypeError(f'A callable object or None is expected, '
                            f'got {handler!r}')
        self._exception_handler = handler

    def default_exception_handler(self, context):
        """Log *context* (message, exception and other keys) at ERROR."""
        message = context.get('message') or 'Unhandled exception in event loop'
        exc = context.get('exception')
        exc_info = (type(exc), exc, exc.__traceback__) \
            if exc is not None else False

        current = self._current_handle
        if ('source_traceback' not in context and current is not None
                and current._source_traceback):
            context['handle_traceback'] = current._source_traceback

        lines = [message]
        for key in sorted(context):
            if key in ('message', 'exception'):
                continue
            value = context[key]
            if key in ('source_traceback', 'handle_traceback'):
                what = 'Object' if key == 'source_traceback' else 'Handle'
                stack = ''.join(traceback.format_list(value)).rstrip()
                value = f'{what} created at (most recent call last):\n{stack}'
            else:
                value = repr(value)
            lines.append(f'{key}: {value}')
        logger.error('\n'.join(lines), exc_info=exc_info)

    def call_exception_handler(self, context):
        """Report an error through the installed (or default) handler."""
        if self._exception_handler is None:
            try:
                self.default_exception_handler(context)
            except (SystemExit, KeyboardInterrupt):
                raise
            except BaseException:
                logger.error('Exception in default exception handler',
                             exc_info=True)
            return
        try:
            # run the handler in the context of the failing object
            owner = (context.get("task") or context.get("future")
                     or context.get("handle"))
            ctx = owner.get_context() if hasattr(owner, "get_context") \
                else None
            if ctx is not None and hasattr(ctx, "run"):
                ctx.run(self._exception_handler, self, context)
            else:
                self._exception_handler(self, context)
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
