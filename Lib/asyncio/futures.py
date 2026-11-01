"""asyncio.Future and helpers bridging to concurrent.futures."""

__all__ = (
    'Future', 'wrap_future', 'isfuture',
    'future_add_to_awaited_by', 'future_discard_from_awaited_by',
)

import contextvars
import logging
import sys
from types import GenericAlias

from . import base_futures
from . import events
from . import exceptions
from . import format_helpers

isfuture = base_futures.isfuture

_PENDING = base_futures._PENDING
_CANCELLED = base_futures._CANCELLED
_FINISHED = base_futures._FINISHED

STACK_DEBUG = logging.DEBUG - 1


def _concurrent():
    # concurrent.futures is imported on demand: it is comparatively heavy
    # and only needed when bridging to threads.
    import concurrent.futures
    return concurrent.futures


class Future:
    """Placeholder for the result of an asynchronous operation.

    Done callbacks are scheduled through the event loop (never called
    synchronously); result()/exception() never block.  Not thread-safe.
    """

    # Class-level defaults keep half-initialized subclasses usable.
    _state = _PENDING
    _result = None
    _exception = None
    _exception_tb = None
    _loop = None
    _source_traceback = None
    _cancel_message = None
    _cancelled_exc = None
    # None marks "not Future-compatible"; False/True is the await protocol
    # flag (True while yielded to a Task).
    _asyncio_future_blocking = False
    _awaiters = None
    _unretrieved = False

    def __init__(self, *, loop=None):
        if self._loop is not None:
            raise RuntimeError(f"{type(self).__name__} object is already "
                               f"initialized")
        self._loop = events.get_event_loop() if loop is None else loop
        self._callbacks = []
        if self._loop.get_debug():
            self._source_traceback = format_helpers.extract_stack(
                sys._getframe(1))

    __class_getitem__ = classmethod(GenericAlias)

    def __repr__(self):
        return base_futures._future_repr(self)

    def __del__(self):
        if not self._unretrieved:
            return
        report = {
            'message': f'{type(self).__name__} exception was never retrieved',
            'exception': self._exception,
            'future': self,
        }
        if self._source_traceback:
            report['source_traceback'] = self._source_traceback
        self._loop.call_exception_handler(report)

    @property
    def _log_traceback(self):
        return self._unretrieved

    @_log_traceback.setter
    def _log_traceback(self, value):
        if value:
            raise ValueError('_log_traceback can only be set to False')
        self._unretrieved = False

    @property
    def _asyncio_awaited_by(self):
        return None if self._awaiters is None else frozenset(self._awaiters)

    def get_loop(self):
        """The event loop this future belongs to."""
        if self._loop is None:
            raise RuntimeError("Future object is not initialized.")
        return self._loop

    # -- state inspection

    def done(self):
        return self._state != _PENDING

    def cancelled(self):
        return self._state == _CANCELLED

    def _make_cancelled_error(self):
        # Hand out the stored CancelledError only once (reference cycles).
        saved, self._cancelled_exc = self._cancelled_exc, None
        if saved is not None:
            return saved
        if self._cancel_message is None:
            return exceptions.CancelledError()
        return exceptions.CancelledError(self._cancel_message)

    def _ensure_finished(self, what):
        if self._state == _CANCELLED:
            raise self._make_cancelled_error()
        if self._state != _FINISHED:
            raise exceptions.InvalidStateError(what)
        self._unretrieved = False

    def result(self):
        """The result; re-raises the exception or CancelledError."""
        self._ensure_finished('Result is not ready.')
        if self._exception is not None:
            raise self._exception.with_traceback(self._exception_tb)
        return self._result

    def exception(self):
        """The exception (None if a result was set)."""
        self._ensure_finished('Exception is not set.')
        return self._exception

    # -- callbacks

    def add_done_callback(self, fn, *, context=None):
        """Arrange for fn(self) to be scheduled once the future is done."""
        if self._state == _PENDING:
            self._callbacks.append(
                (fn, contextvars.copy_context() if context is None else context))
        else:
            self._loop.call_soon(fn, self, context=context)

    def remove_done_callback(self, fn):
        """Forget every registration of *fn*; return how many there were."""
        kept = [entry for entry in self._callbacks if entry[0] != fn]
        removed = len(self._callbacks) - len(kept)
        if removed:
            self._callbacks[:] = kept
        return removed

    def _schedule_done_callbacks(self):
        pending, self._callbacks[:] = self._callbacks[:], []
        for fn, ctx in pending:
            self._loop.call_soon(fn, self, context=ctx)

    # -- completion

    def _check_pending(self):
        if self._state != _PENDING:
            raise exceptions.InvalidStateError(f'{self._state}: {self!r}')

    def cancel(self, msg=None):
        """Cancel the future; False if it is already done."""
        self._unretrieved = False
        if self._state != _PENDING:
            return False
        self._state = _CANCELLED
        self._cancel_message = msg
        self._schedule_done_callbacks()
        return True

    def set_result(self, result):
        self._check_pending()
        self._result = result
        self._state = _FINISHED
        self._schedule_done_callbacks()

    def set_exception(self, exception):
        self._check_pending()
        if isinstance(exception, type):
            exception = exception()
        if not isinstance(exception, BaseException):
            raise TypeError("invalid exception object")
        if isinstance(exception, StopIteration):
            # a StopIteration would end the awaiting coroutine silently
            wrapped = RuntimeError("StopIteration interacts badly with "
                                   "generators and cannot be raised into a "
                                   "Future")
            wrapped.__cause__ = wrapped.__context__ = exception
            exception = wrapped
        self._exception = exception
        self._exception_tb = exception.__traceback__
        self._state = _FINISHED
        self._schedule_done_callbacks()
        self._unretrieved = True

    # -- await protocol

    def __await__(self):
        if self._state == _PENDING:
            self._asyncio_future_blocking = True
            yield self   # the driving Task resumes us when we are done
            if self._state == _PENDING:
                raise RuntimeError("await wasn't used with future")
        return self.result()

    __iter__ = __await__


_PyFuture = Future


def _get_loop(fut):
    getter = getattr(fut, 'get_loop', None)
    if getter is not None:
        return getter()
    return fut._loop


def _set_result_unless_cancelled(fut, result):
    if not fut.cancelled():
        fut.set_result(result)


def _convert_future_exc(exc):
    cf = _concurrent()
    for foreign, native in ((cf.CancelledError, exceptions.CancelledError),
                            (cf.InvalidStateError, exceptions.InvalidStateError)):
        if type(exc) is foreign:
            return native(*exc.args).with_traceback(exc.__traceback__)
    return exc


def _set_concurrent_future_state(concurrent, source):
    """Copy a finished asyncio future into a concurrent.futures.Future."""
    assert source.done()
    if source.cancelled():
        concurrent.cancel()
    if not concurrent.set_running_or_notify_cancel():
        return
    exc = source.exception()
    if exc is None:
        concurrent.set_result(source.result())
    else:
        concurrent.set_exception(_convert_future_exc(exc))


def _copy_future_state(source, dest):
    """Copy a finished future into a pending asyncio future."""
    assert source.done()
    if dest.cancelled():
        return
    assert not dest.done()
    if source.cancelled():
        dest.cancel()
        return
    exc = source.exception()
    if exc is None:
        dest.set_result(source.result())
    else:
        dest.set_exception(_convert_future_exc(exc))


def _chain_future(source, destination):
    """Make *destination* follow *source* (and cancel *source* when
    *destination* is cancelled).  Each side may be an asyncio or a
    concurrent.futures future; cross-loop updates go through
    call_soon_threadsafe()."""
    cf = _concurrent()
    for fut, role in ((source, 'source'), (destination, 'destination')):
        if not isfuture(fut) and not isinstance(fut, cf.Future):
            raise TypeError(f'A future is required for {role} argument')
    src_loop = _get_loop(source) if isfuture(source) else None
    dst_loop = _get_loop(destination) if isfuture(destination) else None

    def copy_into(target, origin):
        if isfuture(target):
            _copy_future_state(origin, target)
        else:
            _set_concurrent_future_state(target, origin)

    # a callback may run in any thread: act directly when already running
    # in the loop that owns the other side
    def on_destination_done(dest):
        if not dest.cancelled():
            return
        if src_loop is None or src_loop is events._get_running_loop():
            source.cancel()
        else:
            src_loop.call_soon_threadsafe(source.cancel)

    def on_source_done(src):
        if dst_loop is None or dst_loop is events._get_running_loop():
            copy_into(destination, src)
        elif not dst_loop.is_closed():
            dst_loop.call_soon_threadsafe(copy_into, destination, src)

    destination.add_done_callback(on_destination_done)
    source.add_done_callback(on_source_done)


def wrap_future(future, *, loop=None):
    """Return an asyncio future mirroring a concurrent.futures.Future."""
    if isfuture(future):
        return future
    assert isinstance(future, _concurrent().Future), \
        f'concurrent.futures.Future is expected, got {future!r}'
    if loop is None:
        loop = events.get_event_loop()
    mirror = loop.create_future()
    _chain_future(future, mirror)
    return mirror


def future_add_to_awaited_by(fut, waiter, /):
    """Record that *waiter* awaits *fut* (native futures/tasks only)."""
    if isinstance(fut, _PyFuture) and isinstance(waiter, _PyFuture):
        if fut._awaiters is None:
            fut._awaiters = set()
        fut._awaiters.add(waiter)


def future_discard_from_awaited_by(fut, waiter, /):
    """Undo future_add_to_awaited_by()."""
    if isinstance(fut, _PyFuture) and isinstance(waiter, _PyFuture):
        if fut._awaiters is not None:
            fut._awaiters.discard(waiter)


_py_future_add_to_awaited_by = future_add_to_awaited_by
_py_future_discard_from_awaited_by = future_discard_from_awaited_by

try:
    import _asyncio
except ImportError:
    pass
else:
    Future = _CFuture = _asyncio.Future
    future_add_to_awaited_by = _c_future_add_to_awaited_by = \
        _asyncio.future_add_to_awaited_by
    future_discard_from_awaited_by = _c_future_discard_from_awaited_by = \
        _asyncio.future_discard_from_awaited_by
