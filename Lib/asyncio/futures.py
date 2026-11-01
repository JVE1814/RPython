"""A Future class similar to the one in PEP 3148."""

__all__ = (
    'Future', 'wrap_future', 'isfuture',
    'future_add_to_awaited_by', 'future_discard_from_awaited_by',
)

import contextvars
import logging
import sys
from types import GenericAlias

try:
    import concurrent.futures as _cf
except ImportError:  # pragma: no cover - minimal installations
    class _cf:
        class Future:
            pass

        class CancelledError(Exception):
            pass

        class InvalidStateError(Exception):
            pass

from . import base_futures
from . import events
from . import exceptions
from . import format_helpers


isfuture = base_futures.isfuture


_PENDING = base_futures._PENDING
_CANCELLED = base_futures._CANCELLED
_FINISHED = base_futures._FINISHED


STACK_DEBUG = logging.DEBUG - 1  # heavy-duty debugging


class Future:
    """A Future: the eventual result of an asynchronous operation.

    Unlike concurrent.futures.Future it is not thread-safe, result() and
    exception() never block, and done callbacks are scheduled with the
    event loop's call_soon() instead of being called immediately.
    """

    # Class attributes double as defaults for subclasses that skip __init__.
    _state = _PENDING
    _result = None
    _exception = None
    _loop = None
    _source_traceback = None
    _cancel_message = None
    # A saved CancelledError, re-raised by result()/exception().
    _cancelled_exc = None

    # True while a coroutine yields this future to its Task (await); also
    # marks Future-compatible classes (see isfuture()).
    _asyncio_future_blocking = False

    # Futures/tasks awaiting this one (for call graph introspection).
    __asyncio_awaited_by = None

    __log_traceback = False

    def __init__(self, *, loop=None):
        """Initialize the future.

        If *loop* is None, the current event loop (get_event_loop()) is used.
        """
        if self._loop is not None:
            raise RuntimeError(f"{self.__class__.__name__} object "
                               f"is already initialized")

        if loop is None:
            self._loop = events.get_event_loop()
        else:
            self._loop = loop
        self._callbacks = []
        if self._loop.get_debug():
            self._source_traceback = format_helpers.extract_stack(
                sys._getframe(1))

    def __repr__(self):
        return base_futures._future_repr(self)

    def __del__(self):
        if not self.__log_traceback:
            # the exception was retrieved, or there is none
            return
        exc = self._exception
        context = {
            'message':
                f'{self.__class__.__name__} exception was never retrieved',
            'exception': exc,
            'future': self,
        }
        if self._source_traceback:
            context['source_traceback'] = self._source_traceback
        self._loop.call_exception_handler(context)

    __class_getitem__ = classmethod(GenericAlias)

    @property
    def _log_traceback(self):
        return self.__log_traceback

    @_log_traceback.setter
    def _log_traceback(self, val):
        if val:
            raise ValueError('_log_traceback can only be set to False')
        self.__log_traceback = False

    @property
    def _asyncio_awaited_by(self):
        if self.__asyncio_awaited_by is None:
            return None
        return frozenset(self.__asyncio_awaited_by)

    def get_loop(self):
        """Return the event loop the Future is bound to."""
        loop = self._loop
        if loop is None:
            raise RuntimeError("Future object is not initialized.")
        return loop

    def _make_cancelled_error(self):
        """Return the CancelledError to raise for a cancelled future.

        The saved exception is handed out only once, to avoid reference
        cycles through its traceback.
        """
        if self._cancelled_exc is not None:
            exc = self._cancelled_exc
            self._cancelled_exc = None
            return exc

        if self._cancel_message is None:
            exc = exceptions.CancelledError()
        else:
            exc = exceptions.CancelledError(self._cancel_message)
        return exc

    def cancel(self, msg=None):
        """Cancel the future and schedule its callbacks.

        Returns False if the future is already done or cancelled.
        """
        self.__log_traceback = False
        if self._state != _PENDING:
            return False
        self._state = _CANCELLED
        self._cancel_message = msg
        self.__schedule_callbacks()
        return True

    def __schedule_callbacks(self):
        callbacks = self._callbacks[:]
        if not callbacks:
            return

        self._callbacks[:] = []
        for callback, ctx in callbacks:
            self._loop.call_soon(callback, self, context=ctx)

    def cancelled(self):
        """Return True if the future was cancelled."""
        return self._state == _CANCELLED

    def done(self):
        """Return True if the future is done (result, exception or cancel)."""
        return self._state != _PENDING

    def result(self):
        """Return the result, or raise the exception / CancelledError.

        InvalidStateError is raised if the future is not done yet.
        """
        if self._state == _CANCELLED:
            raise self._make_cancelled_error()
        if self._state != _FINISHED:
            raise exceptions.InvalidStateError('Result is not ready.')
        self.__log_traceback = False
        if self._exception is not None:
            raise self._exception.with_traceback(self._exception_tb)
        return self._result

    def exception(self):
        """Return the exception set on the future (None if a result was set).

        CancelledError / InvalidStateError as for result().
        """
        if self._state == _CANCELLED:
            raise self._make_cancelled_error()
        if self._state != _FINISHED:
            raise exceptions.InvalidStateError('Exception is not set.')
        self.__log_traceback = False
        return self._exception

    def add_done_callback(self, fn, *, context=None):
        """Call fn(future) when the future is done (scheduled via
        call_soon(); immediately scheduled if already done)."""
        if self._state != _PENDING:
            self._loop.call_soon(fn, self, context=context)
        else:
            if context is None:
                context = contextvars.copy_context()
            self._callbacks.append((fn, context))

    def remove_done_callback(self, fn):
        """Remove all registrations of fn; return how many were removed."""
        filtered_callbacks = [(f, ctx)
                              for (f, ctx) in self._callbacks
                              if f != fn]
        removed_count = len(self._callbacks) - len(filtered_callbacks)
        if removed_count:
            self._callbacks[:] = filtered_callbacks
        return removed_count

    def set_result(self, result):
        """Mark the future done and set its result."""
        if self._state != _PENDING:
            raise exceptions.InvalidStateError(f'{self._state}: {self!r}')
        self._result = result
        self._state = _FINISHED
        self.__schedule_callbacks()

    def set_exception(self, exception):
        """Mark the future done and set an exception."""
        if self._state != _PENDING:
            raise exceptions.InvalidStateError(f'{self._state}: {self!r}')
        if isinstance(exception, type):
            exception = exception()
        if not isinstance(exception, BaseException):
            raise TypeError("invalid exception object")
        if isinstance(exception, StopIteration):
            new_exc = RuntimeError("StopIteration interacts badly with "
                                   "generators and cannot be raised into a "
                                   "Future")
            new_exc.__cause__ = exception
            new_exc.__context__ = exception
            exception = new_exc
        self._exception = exception
        self._exception_tb = exception.__traceback__
        self._state = _FINISHED
        self.__schedule_callbacks()
        self.__log_traceback = True

    def __await__(self):
        if not self.done():
            self._asyncio_future_blocking = True
            yield self  # the Task wakes us up when we are done
        if not self.done():
            raise RuntimeError("await wasn't used with future")
        return self.result()

    __iter__ = __await__  # make compatible with 'yield from'


# Kept for the tests (the C Future replaces Future below).
_PyFuture = Future


def _get_loop(fut):
    # Future-compatible objects may lack get_loop()
    try:
        get_loop = fut.get_loop
    except AttributeError:
        pass
    else:
        return get_loop()
    return fut._loop


def _set_result_unless_cancelled(fut, result):
    """Helper setting the result only if the future was not cancelled."""
    if fut.cancelled():
        return
    fut.set_result(result)


def _convert_future_exc(exc):
    exc_class = type(exc)
    if exc_class is _cf.CancelledError:
        return exceptions.CancelledError(*exc.args).with_traceback(exc.__traceback__)
    elif exc_class is _cf.InvalidStateError:
        return exceptions.InvalidStateError(*exc.args).with_traceback(exc.__traceback__)
    else:
        return exc


def _set_concurrent_future_state(concurrent, source):
    """Copy the state of an asyncio future into a concurrent.futures one."""
    assert source.done()
    if source.cancelled():
        concurrent.cancel()
    if not concurrent.set_running_or_notify_cancel():
        return
    exception = source.exception()
    if exception is not None:
        concurrent.set_exception(_convert_future_exc(exception))
    else:
        result = source.result()
        concurrent.set_result(result)


def _copy_future_state(source, dest):
    """Copy the state of a done future into a pending asyncio future."""
    assert source.done()
    if dest.cancelled():
        return
    assert not dest.done()
    if source.cancelled():
        dest.cancel()
    else:
        exception = source.exception()
        if exception is not None:
            dest.set_exception(_convert_future_exc(exception))
        else:
            result = source.result()
            dest.set_result(result)


def _chain_future(source, destination):
    """Propagate the outcome of *source* to *destination*, and the
    cancellation of *destination* back to *source*.  Either may be an
    asyncio or a concurrent.futures Future."""
    if not isfuture(source) and not isinstance(source, _cf.Future):
        raise TypeError('A future is required for source argument')
    if not isfuture(destination) and not isinstance(destination, _cf.Future):
        raise TypeError('A future is required for destination argument')
    source_loop = _get_loop(source) if isfuture(source) else None
    dest_loop = _get_loop(destination) if isfuture(destination) else None

    def _set_state(future, other):
        if isfuture(future):
            _copy_future_state(other, future)
        else:
            _set_concurrent_future_state(future, other)

    def _call_check_cancel(destination):
        if destination.cancelled():
            if source_loop is None or source_loop is dest_loop:
                source.cancel()
            else:
                source_loop.call_soon_threadsafe(source.cancel)

    def _call_set_state(source):
        if (destination.cancelled() and
                dest_loop is not None and dest_loop.is_closed()):
            return
        if dest_loop is None or dest_loop is source_loop:
            _set_state(destination, source)
        else:
            if dest_loop.is_closed():
                return
            dest_loop.call_soon_threadsafe(_set_state, destination, source)

    destination.add_done_callback(_call_check_cancel)
    source.add_done_callback(_call_set_state)


def wrap_future(future, *, loop=None):
    """Wrap a concurrent.futures.Future object in an asyncio Future."""
    if isfuture(future):
        return future
    assert isinstance(future, _cf.Future), \
        f'concurrent.futures.Future is expected, got {future!r}'
    if loop is None:
        loop = events.get_event_loop()
    new_future = loop.create_future()
    _chain_future(future, new_future)
    return new_future


def future_add_to_awaited_by(fut, waiter, /):
    """Record that *fut* is awaited on by *waiter* (native futures only)."""
    if isinstance(fut, _PyFuture) and isinstance(waiter, _PyFuture):
        if fut._Future__asyncio_awaited_by is None:
            fut._Future__asyncio_awaited_by = set()
        fut._Future__asyncio_awaited_by.add(waiter)


def future_discard_from_awaited_by(fut, waiter, /):
    """Forget that *fut* is awaited on by *waiter*."""
    if isinstance(fut, _PyFuture) and isinstance(waiter, _PyFuture):
        if fut._Future__asyncio_awaited_by is not None:
            fut._Future__asyncio_awaited_by.discard(waiter)


_py_future_add_to_awaited_by = future_add_to_awaited_by
_py_future_discard_from_awaited_by = future_discard_from_awaited_by

try:
    import _asyncio
except ImportError:
    pass
else:
    # _CFuture is needed for tests.
    Future = _CFuture = _asyncio.Future
    future_add_to_awaited_by = _asyncio.future_add_to_awaited_by
    future_discard_from_awaited_by = _asyncio.future_discard_from_awaited_by
    _c_future_add_to_awaited_by = future_add_to_awaited_by
    _c_future_discard_from_awaited_by = future_discard_from_awaited_by
