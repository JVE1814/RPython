//! `_asyncio`: the accelerator module used by `asyncio.futures`,
//! `asyncio.tasks` and `asyncio.events`.
//!
//! It provides its own Future and Task types, the running-loop slot and
//! the task registry (current task per loop, scheduled and eager tasks),
//! independent of the pure-Python implementations in Lib/asyncio, so that
//! both variants can be used (and tested) side by side.
//!
//! The types are currently defined by Python source compiled when the
//! module is first imported (like `_types`); the observable contract is
//! that of CPython's C accelerator (non-deletable `_asyncio_future_blocking`
//! / `_log_traceback`, `_callbacks` returning a fresh list, uninitialized
//! objects raising "... object is not initialized.", ...).

use crate::errors::*;
use crate::object::*;
use crate::types::dict;

const SRC: &str = r#"
import sys as _sys
import os as _os
import contextvars as _contextvars
import itertools as _itertools
import weakref as _weakref
import _thread
from types import GenericAlias as _GenericAlias

from asyncio import base_futures as _base_futures
from asyncio import exceptions as _exceptions
from asyncio import format_helpers as _format_helpers

_PENDING = 'PENDING'
_CANCELLED = 'CANCELLED'
_FINISHED = 'FINISHED'

_UNSET = object()


# ---------------------------------------------------------------------------
# running loop

class _LoopSlot(_thread._local):
    loop = None
    pid = None

_slot = _LoopSlot()


def _get_running_loop():
    """Return the running event loop or None (meant for loops and policies)."""
    loop = _slot.loop
    if loop is not None and _slot.pid == _os.getpid():
        return loop
    return None


def _set_running_loop(loop, /):
    """Set the running event loop (meant for event loop implementations)."""
    _slot.loop = loop
    _slot.pid = _os.getpid()


def get_running_loop():
    """Return the running event loop.  Raise a RuntimeError if there is none."""
    loop = _get_running_loop()
    if loop is None:
        raise RuntimeError('no running event loop')
    return loop


def get_event_loop():
    """Return the running loop, or the current loop of the event loop policy."""
    loop = _get_running_loop()
    if loop is not None:
        return loop
    from asyncio import events
    return events._get_event_loop_policy().get_event_loop()


def _events():
    from asyncio import events
    return events


def _loop_of(fut):
    try:
        getter = fut.get_loop
    except AttributeError:
        return fut._loop
    return getter()


# ---------------------------------------------------------------------------
# Future

class Future:
    """This class is *almost* compatible with concurrent.futures.Future.

    Differences:

    - result() and exception() do not take a timeout argument and
      raise an exception when the future isn't done yet.

    - Callbacks registered with add_done_callback() are always called
      via the event loop's call_soon_threadsafe().

    - This class is not compatible with the wait() and as_completed()
      methods in the concurrent.futures package.
    """

    __slots__ = ('_fut_loop', '_fut_state', '_fut_result', '_fut_exc',
                 '_fut_exc_tb', '_fut_callbacks', '_fut_blocking',
                 '_fut_log_tb', '_fut_cancel_msg', '_fut_cancelled_exc',
                 '_fut_source_tb', '_fut_awaited_by', '__weakref__',
                 '__dict__')

    def __init__(self, *, loop=None):
        if loop is None:
            loop = get_event_loop()
        self._fut_loop = loop
        self._fut_state = _PENDING
        self._fut_result = None
        self._fut_exc = None
        self._fut_exc_tb = None
        self._fut_callbacks = []
        self._fut_blocking = False
        self._fut_log_tb = False
        self._fut_cancel_msg = None
        self._fut_cancelled_exc = None
        self._fut_awaited_by = None
        self._fut_source_tb = None
        if loop.get_debug():
            self._fut_source_tb = _format_helpers.extract_stack(
                _sys._getframe(1))

    __class_getitem__ = classmethod(_GenericAlias)

    def _check_init(self):
        try:
            loop = self._fut_loop
        except AttributeError:
            loop = None
        if loop is None:
            raise RuntimeError('Future object is not initialized.')
        return loop

    def __repr__(self):
        return _base_futures._future_repr(self)

    def __del__(self):
        try:
            log = self._fut_log_tb
        except AttributeError:
            return
        if not log:
            return
        context = {
            'message': f'{self.__class__.__name__} exception was never '
                       f'retrieved',
            'exception': self._fut_exc,
            'future': self,
        }
        if self._fut_source_tb:
            context['source_traceback'] = self._fut_source_tb
        self._fut_loop.call_exception_handler(context)

    # -- attributes with C semantics

    @property
    def _loop(self):
        return getattr(self, '_fut_loop', None)

    @property
    def _state(self):
        if getattr(self, '_fut_loop', None) is None:
            return None
        return self._fut_state

    @property
    def _result(self):
        return getattr(self, '_fut_result', None)

    @property
    def _exception(self):
        return getattr(self, '_fut_exc', None)

    @property
    def _source_traceback(self):
        return getattr(self, '_fut_source_tb', None)

    @property
    def _cancel_message(self):
        return getattr(self, '_fut_cancel_msg', None)

    @_cancel_message.setter
    def _cancel_message(self, value):
        self._fut_cancel_msg = value

    @property
    def _asyncio_future_blocking(self):
        self._check_init()
        return self._fut_blocking

    @_asyncio_future_blocking.setter
    def _asyncio_future_blocking(self, value):
        self._check_init()
        self._fut_blocking = bool(value)

    @property
    def _log_traceback(self):
        self._check_init()
        return self._fut_log_tb

    @_log_traceback.setter
    def _log_traceback(self, value):
        self._check_init()
        if value:
            raise ValueError('_log_traceback can only be set to False')
        self._fut_log_tb = False

    @property
    def _callbacks(self):
        self._check_init()
        if not self._fut_callbacks:
            return None
        return list(self._fut_callbacks)

    @property
    def _asyncio_awaited_by(self):
        awaited_by = getattr(self, '_fut_awaited_by', None)
        if not awaited_by:
            return None
        return frozenset(awaited_by)

    # -- public API

    def get_loop(self):
        """Return the event loop the Future is bound to."""
        return self._check_init()

    def done(self):
        """Return True if the future is done (finished or cancelled)."""
        self._check_init()
        return self._fut_state != _PENDING

    def cancelled(self):
        """Return True if the future was cancelled."""
        self._check_init()
        return self._fut_state == _CANCELLED

    def _make_cancelled_error(self):
        """Create the CancelledError to raise if the Future is cancelled."""
        exc = self._fut_cancelled_exc
        if exc is not None:
            self._fut_cancelled_exc = None
            return exc
        if self._fut_cancel_msg is None:
            return _exceptions.CancelledError()
        return _exceptions.CancelledError(self._fut_cancel_msg)

    def result(self):
        """Return the result this future represents."""
        self._check_init()
        state = self._fut_state
        if state == _CANCELLED:
            raise self._make_cancelled_error()
        if state != _FINISHED:
            raise _exceptions.InvalidStateError('Result is not ready.')
        self._fut_log_tb = False
        if self._fut_exc is not None:
            raise self._fut_exc.with_traceback(self._fut_exc_tb)
        return self._fut_result

    def exception(self):
        """Return the exception that was set on this future."""
        self._check_init()
        state = self._fut_state
        if state == _CANCELLED:
            raise self._make_cancelled_error()
        if state != _FINISHED:
            raise _exceptions.InvalidStateError('Exception is not set.')
        self._fut_log_tb = False
        return self._fut_exc

    def add_done_callback(self, fn, /, *, context=None):
        """Add a callback to be run when the future becomes done."""
        loop = self._check_init()
        if self._fut_state != _PENDING:
            loop.call_soon(fn, self, context=context)
            return
        if context is None:
            context = _contextvars.copy_context()
        self._fut_callbacks.append((fn, context))

    def remove_done_callback(self, fn, /):
        """Remove all instances of a callback from the "call when done" list."""
        self._check_init()
        callbacks = self._fut_callbacks
        kept = [entry for entry in callbacks if entry[0] != fn]
        removed = len(callbacks) - len(kept)
        if removed:
            callbacks[:] = kept
        return removed

    def _schedule_callbacks(self):
        callbacks = self._fut_callbacks
        if not callbacks:
            return
        pending = callbacks[:]
        callbacks.clear()
        loop = self._fut_loop
        for fn, context in pending:
            loop.call_soon(fn, self, context=context)

    def cancel(self, msg=None):
        """Cancel the future and schedule callbacks."""
        self._check_init()
        self._fut_log_tb = False
        if self._fut_state != _PENDING:
            return False
        self._fut_state = _CANCELLED
        self._fut_cancel_msg = msg
        self._schedule_callbacks()
        return True

    def set_result(self, result, /):
        """Mark the future done and set its result."""
        self._check_init()
        if self._fut_state != _PENDING:
            raise _exceptions.InvalidStateError(
                f'{self._fut_state}: {self!r}')
        self._fut_result = result
        self._fut_state = _FINISHED
        self._schedule_callbacks()

    def set_exception(self, exception, /):
        """Mark the future done and set an exception."""
        self._check_init()
        if self._fut_state != _PENDING:
            raise _exceptions.InvalidStateError(
                f'{self._fut_state}: {self!r}')
        if isinstance(exception, type):
            exception = exception()
        if not isinstance(exception, BaseException):
            raise TypeError('invalid exception object')
        if isinstance(exception, StopIteration):
            new_exc = RuntimeError('StopIteration interacts badly with '
                                   'generators and cannot be raised into a '
                                   'Future')
            new_exc.__cause__ = exception
            new_exc.__context__ = exception
            exception = new_exc
        self._fut_exc = exception
        self._fut_exc_tb = exception.__traceback__
        self._fut_state = _FINISHED
        self._schedule_callbacks()
        self._fut_log_tb = True

    def __await__(self):
        self._check_init()
        if self._fut_state == _PENDING:
            self._fut_blocking = True
            yield self
        if self._fut_state == _PENDING:
            raise RuntimeError("await wasn't used with future")
        return self.result()

    __iter__ = __await__


def future_add_to_awaited_by(fut, waiter, /):
    """Record that *waiter* is awaiting *fut*."""
    if isinstance(fut, Future) and isinstance(waiter, Future):
        if fut._fut_awaited_by is None:
            fut._fut_awaited_by = set()
        fut._fut_awaited_by.add(waiter)


def future_discard_from_awaited_by(fut, waiter, /):
    """Forget that *waiter* is awaiting *fut*."""
    if isinstance(fut, Future) and isinstance(waiter, Future):
        if fut._fut_awaited_by is not None:
            fut._fut_awaited_by.discard(waiter)


# ---------------------------------------------------------------------------
# task registry

_scheduled_tasks = _weakref.WeakSet()
_eager_tasks = set()
_current_tasks = {}


def _register_task(task):
    """Register a new task in asyncio as executed by loop."""
    _scheduled_tasks.add(task)


def _register_eager_task(task):
    """Register a new task in asyncio as executed by loop."""
    _eager_tasks.add(task)


def _unregister_task(task):
    """Unregister a task."""
    _scheduled_tasks.discard(task)


def _unregister_eager_task(task):
    """Unregister a task."""
    _eager_tasks.discard(task)


def _enter_task(loop, task):
    """Enter into task execution or resume suspended task."""
    current = _current_tasks.get(loop)
    if current is not None:
        raise RuntimeError(f'Cannot enter into task {task!r} while another '
                           f'task {current!r} is being executed.')
    _current_tasks[loop] = task


def _leave_task(loop, task):
    """Leave task execution or suspend a task."""
    current = _current_tasks.get(loop)
    if current is not task:
        raise RuntimeError(f'Leaving task {task!r} does not match the '
                           f'current task {current!r}.')
    del _current_tasks[loop]


def _swap_current_task(loop, task):
    """Temporarily swap in the supplied task and return the original one
    (or None)."""
    previous = _current_tasks.get(loop)
    if task is None:
        _current_tasks.pop(loop, None)
    else:
        _current_tasks[loop] = task
    return previous


def current_task(loop=None):
    """Return a currently executed task."""
    if loop is None:
        loop = get_running_loop()
    return _current_tasks.get(loop)


def all_tasks(loop=None):
    """Return a set of all tasks for the loop."""
    if loop is None:
        loop = get_running_loop()
    for attempt in range(1000):
        try:
            candidates = list(_scheduled_tasks)
            break
        except RuntimeError:
            if attempt == 999:
                raise
    candidates.extend(_eager_tasks)
    return {task for task in candidates
            if _loop_of(task) is loop and not task.done()}


# ---------------------------------------------------------------------------
# Task

_task_counter = _itertools.count(1)


class Task(Future):
    """A coroutine wrapped in a Future."""

    __slots__ = ('_task_coro', '_task_context', '_task_name',
                 '_task_fut_waiter', '_task_must_cancel',
                 '_task_num_cancels', '_task_log_destroy_pending')

    def __init__(self, coro, *, loop=None, name=None, context=None,
                 eager_start=False):
        Future.__init__(self, loop=loop)
        from asyncio import coroutines
        self._task_log_destroy_pending = True
        if not coroutines.iscoroutine(coro):
            self._task_log_destroy_pending = False
            raise TypeError(f'a coroutine was expected, got {coro!r}')
        if self._fut_source_tb:
            del self._fut_source_tb[-1]
        self._task_must_cancel = False
        self._task_num_cancels = 0
        self._task_fut_waiter = None
        self._task_coro = coro
        if context is None:
            context = _contextvars.copy_context()
        self._task_context = context
        if name is None:
            self._task_name = None
        elif isinstance(name, str):
            self._task_name = name
        else:
            self._task_name = str(name)

        if eager_start and self._fut_loop.is_running():
            self._task_eager_start()
        else:
            self._fut_loop.call_soon(self._task_step_cb,
                                     context=self._task_context)
            _register_task(self)

    def __del__(self):
        try:
            pending = (self._fut_state == _PENDING and
                       self._task_log_destroy_pending)
        except AttributeError:
            pending = False
        if pending:
            context = {'task': self,
                       'message': 'Task was destroyed but it is pending!'}
            if self._fut_source_tb:
                context['source_traceback'] = self._fut_source_tb
            self._fut_loop.call_exception_handler(context)
        Future.__del__(self)

    __class_getitem__ = classmethod(_GenericAlias)

    def __repr__(self):
        from asyncio import base_tasks
        return base_tasks._task_repr(self)

    # -- attributes

    @property
    def _log_destroy_pending(self):
        return getattr(self, '_task_log_destroy_pending', True)

    @_log_destroy_pending.setter
    def _log_destroy_pending(self, value):
        self._task_log_destroy_pending = bool(value)

    @property
    def _must_cancel(self):
        return getattr(self, '_task_must_cancel', False)

    @property
    def _coro(self):
        return getattr(self, '_task_coro', None)

    @property
    def _fut_waiter(self):
        return getattr(self, '_task_fut_waiter', None)

    @property
    def _num_cancels_requested(self):
        return getattr(self, '_task_num_cancels', 0)

    # -- public API

    def get_coro(self):
        return getattr(self, '_task_coro', None)

    def get_context(self):
        return getattr(self, '_task_context', None)

    def get_name(self):
        name = getattr(self, '_task_name', None)
        if name is None:
            name = self._task_name = f'Task-{next(_task_counter)}'
        return name

    def set_name(self, value, /):
        if not isinstance(value, str):
            value = str(value)
        self._task_name = value

    def set_result(self, result, /):
        raise RuntimeError('Task does not support set_result operation')

    def set_exception(self, exception, /):
        raise RuntimeError('Task does not support set_exception operation')

    def get_stack(self, *, limit=None):
        """Return the list of stack frames for this task's coroutine."""
        from asyncio import base_tasks
        return base_tasks._task_get_stack(self, limit)

    def print_stack(self, *, limit=None, file=None):
        """Print the stack or traceback for this task's coroutine."""
        from asyncio import base_tasks
        return base_tasks._task_print_stack(self, limit, file)

    def cancel(self, msg=None):
        """Request that this task cancel itself."""
        self._check_init()
        self._fut_log_tb = False
        if self._fut_state != _PENDING:
            return False
        self._task_num_cancels += 1
        waiter = self._task_fut_waiter
        if waiter is not None and waiter.cancel(msg=msg):
            return True
        self._task_must_cancel = True
        self._fut_cancel_msg = msg
        return True

    def cancelling(self):
        """Return the count of the task's cancellation requests."""
        return self._task_num_cancels

    def uncancel(self):
        """Decrement the task's count of cancellation requests."""
        if self._task_num_cancels > 0:
            self._task_num_cancels -= 1
            if self._task_num_cancels == 0:
                self._task_must_cancel = False
        return self._task_num_cancels

    # -- stepping

    def _task_eager_start(self):
        loop = self._fut_loop
        previous = _swap_current_task(loop, self)
        try:
            _register_eager_task(self)
            try:
                self._task_context.run(self._task_step_run_and_handle, None)
            finally:
                _unregister_eager_task(self)
        finally:
            try:
                displaced = _swap_current_task(loop, previous)
                assert displaced is self
            finally:
                if self._fut_state != _PENDING:
                    self._task_coro = None
                else:
                    _register_task(self)

    def _task_step_cb(self, exc=None):
        if self._fut_state != _PENDING:
            raise _exceptions.InvalidStateError(
                f'__step(): already done: {self!r}, {exc!r}')
        if self._task_must_cancel:
            if not isinstance(exc, _exceptions.CancelledError):
                exc = self._make_cancelled_error()
            self._task_must_cancel = False
        self._task_fut_waiter = None
        loop = self._fut_loop
        _enter_task(loop, self)
        try:
            self._task_step_run_and_handle(exc)
        finally:
            _leave_task(loop, self)

    def _task_schedule(self, exc=None):
        if exc is None:
            self._fut_loop.call_soon(self._task_step_cb,
                                     context=self._task_context)
        else:
            self._fut_loop.call_soon(self._task_step_cb, exc,
                                     context=self._task_context)

    def _task_step_run_and_handle(self, exc):
        coro = self._task_coro
        try:
            if exc is None:
                result = coro.send(None)
            else:
                result = coro.throw(exc)
        except StopIteration as stop:
            if self._task_must_cancel:
                self._task_must_cancel = False
                Future.cancel(self, msg=self._fut_cancel_msg)
            else:
                Future.set_result(self, stop.value)
        except _exceptions.CancelledError as cancelled:
            self._fut_cancelled_exc = cancelled
            Future.cancel(self)
        except (KeyboardInterrupt, SystemExit) as fatal:
            Future.set_exception(self, fatal)
            raise
        except BaseException as failure:
            Future.set_exception(self, failure)
        else:
            self._task_handle_yield(result)

    def _task_handle_yield(self, result):
        blocking = getattr(result, '_asyncio_future_blocking', None)
        if blocking is None:
            if result is None:
                self._task_schedule()
            else:
                import inspect
                if inspect.isgenerator(result):
                    self._task_schedule(RuntimeError(
                        f'yield was used instead of yield from for '
                        f'generator in task {self!r} with {result!r}'))
                else:
                    self._task_schedule(RuntimeError(
                        f'Task got bad yield: {result!r}'))
            return
        if _loop_of(result) is not self._fut_loop:
            self._task_schedule(RuntimeError(
                f'Task {self!r} got Future {result!r} attached to a '
                f'different loop'))
            return
        if not blocking:
            self._task_schedule(RuntimeError(
                f'yield was used instead of yield from in task {self!r} '
                f'with {result!r}'))
            return
        if result is self:
            self._task_schedule(RuntimeError(
                f'Task cannot await on itself: {self!r}'))
            return
        result._asyncio_future_blocking = False
        from asyncio import futures
        futures.future_add_to_awaited_by(result, self)
        result.add_done_callback(self._task_wakeup,
                                 context=self._task_context)
        self._task_fut_waiter = result
        if self._task_must_cancel:
            if result.cancel(msg=self._fut_cancel_msg):
                self._task_must_cancel = False

    def _task_wakeup(self, fut):
        from asyncio import futures
        futures.future_discard_from_awaited_by(fut, self)
        try:
            fut.result()
        except BaseException as exc:
            self._task_step_cb(exc)
        else:
            self._task_step_cb()


"#;

pub fn init_module() -> PyResult {
    let m = crate::types::module::new_module("_asyncio")?;
    let d = crate::types::module::module_dict(&m);
    dict::set_item_str(&d, "__builtins__", &crate::builtins::builtins_module())?;
    let code = crate::compile::compile_str(SRC, "<_asyncio>", crate::compile::Mode::Exec, 0, -1)?;
    crate::interp::eval_code(&code.code, &d, Some(&d))?;
    Ok(m)
}
