"""Support for tasks, coroutines and the scheduler."""

__all__ = (
    'Task', 'create_task',
    'FIRST_COMPLETED', 'FIRST_EXCEPTION', 'ALL_COMPLETED',
    'wait', 'wait_for', 'as_completed', 'sleep',
    'gather', 'shield', 'ensure_future', 'run_coroutine_threadsafe',
    'current_task', 'all_tasks',
    'create_eager_task_factory', 'eager_task_factory',
    '_register_task', '_unregister_task', '_enter_task', '_leave_task',
)

import contextvars
import functools
import inspect
import itertools
import math
import types
import weakref
from types import GenericAlias

from . import base_tasks
from . import coroutines
from . import events
from . import exceptions
from . import futures
from . import queues
from . import timeouts

# Unique, increasing default task names ("Task-1", "Task-2", ...).
_task_name_counter = itertools.count(1).__next__


def current_task(loop=None):
    """Return the currently running task of *loop* (or None)."""
    if loop is None:
        loop = events.get_running_loop()
    return _current_tasks.get(loop)


def all_tasks(loop=None):
    """Return the set of not yet finished tasks of *loop*."""
    if loop is None:
        loop = events.get_running_loop()
    # The WeakSet may change size while we copy it (GC, other threads):
    # retry a bounded number of times.
    scheduled_tasks = None
    i = 0
    while True:
        try:
            scheduled_tasks = list(_scheduled_tasks)
        except RuntimeError:
            i += 1
            if i >= 1000:
                raise
        else:
            break
    eager_tasks = list(_eager_tasks)
    return {t for t in itertools.chain(scheduled_tasks, eager_tasks)
            if futures._get_loop(t) is loop and not t.done()}


class Task(futures._PyFuture):  # Inherit Python Task implementation
                                # from a Python Future implementation.
    """A coroutine wrapped in a Future."""

    # Log "Task was destroyed but it is pending!" from __del__ unless
    # disabled (e.g. for tasks created implicitly by gather()).
    _log_destroy_pending = True

    def __init__(self, coro, *, loop=None, name=None, context=None,
                 eager_start=False):
        super().__init__(loop=loop)
        if self._source_traceback:
            del self._source_traceback[-1]
        if not coroutines.iscoroutine(coro):
            # attributes needed by __del__ exist already; do not log
            self._log_destroy_pending = False
            raise TypeError(f"a coroutine was expected, got {coro!r}")

        if name is None:
            self._name = f'Task-{_task_name_counter()}'
        else:
            self._name = str(name)

        self._num_cancels_requested = 0
        self._must_cancel = False
        self._fut_waiter = None
        self._coro = coro
        if context is None:
            self._context = contextvars.copy_context()
        else:
            self._context = context

        if eager_start and self._loop.is_running():
            self.__eager_start()
        else:
            self._loop.call_soon(self.__step, context=self._context)
            _py_register_task(self)

    def __del__(self):
        if self._state == futures._PENDING and self._log_destroy_pending:
            context = {
                'task': self,
                'message': 'Task was destroyed but it is pending!',
            }
            if self._source_traceback:
                context['source_traceback'] = self._source_traceback
            self._loop.call_exception_handler(context)
        super().__del__()

    __class_getitem__ = classmethod(GenericAlias)

    def __repr__(self):
        return base_tasks._task_repr(self)

    def get_coro(self):
        return self._coro

    def get_context(self):
        return self._context

    def get_name(self):
        return self._name

    def set_name(self, value):
        self._name = str(value)

    def set_result(self, result):
        raise RuntimeError('Task does not support set_result operation')

    def set_exception(self, exception):
        raise RuntimeError('Task does not support set_exception operation')

    def get_stack(self, *, limit=None):
        """Return the list of stack frames of this task's coroutine
        (or of the traceback of its exception)."""
        return base_tasks._task_get_stack(self, limit)

    def print_stack(self, *, limit=None, file=None):
        """Print the stack or traceback for this task's coroutine."""
        return base_tasks._task_print_stack(self, limit, file)

    def cancel(self, msg=None):
        """Request that this task cancel itself.

        CancelledError is thrown into the coroutine at its next suspension
        point (the coroutine may suppress it).  Returns False if the task
        is already done.
        """
        self._log_traceback = False
        if self.done():
            return False
        self._num_cancels_requested += 1
        if self._fut_waiter is not None:
            if self._fut_waiter.cancel(msg=msg):
                # Keep _fut_waiter: a waited-on Task may ignore the
                # cancellation, so it may need cancelling again.
                return True
        # __step is already scheduled: it will throw CancelledError
        self._must_cancel = True
        self._cancel_message = msg
        return True

    def cancelling(self):
        """Return the number of pending cancellation requests."""
        return self._num_cancels_requested

    def uncancel(self):
        """Decrement the number of pending cancellation requests."""
        if self._num_cancels_requested > 0:
            self._num_cancels_requested -= 1
            if self._num_cancels_requested == 0:
                self._must_cancel = False
        return self._num_cancels_requested

    def __eager_start(self):
        prev_task = _py_swap_current_task(self._loop, self)
        try:
            _py_register_eager_task(self)
            try:
                self._context.run(self.__step_run_and_handle_result, None)
            finally:
                _py_unregister_eager_task(self)
        finally:
            try:
                curtask = _py_swap_current_task(self._loop, prev_task)
                assert curtask is self
            finally:
                if self.done():
                    self._coro = None
                    self = None  # break reference cycles
                else:
                    _py_register_task(self)

    def __step(self, exc=None):
        if self.done():
            raise exceptions.InvalidStateError(
                f'__step(): already done: {self!r}, {exc!r}')
        if self._must_cancel:
            if not isinstance(exc, exceptions.CancelledError):
                exc = self._make_cancelled_error()
            self._must_cancel = False
        self._fut_waiter = None

        _py_enter_task(self._loop, self)
        try:
            self.__step_run_and_handle_result(exc)
        finally:
            _py_leave_task(self._loop, self)
            self = None  # break reference cycles

    def __step_run_and_handle_result(self, exc):
        coro = self._coro
        try:
            if exc is None:
                # coroutines have no __next__: use send() directly
                result = coro.send(None)
            else:
                result = coro.throw(exc)
        except StopIteration as exc:
            if self._must_cancel:
                # cancelled right before the coroutine returned
                self._must_cancel = False
                super().cancel(msg=self._cancel_message)
            else:
                super().set_result(exc.value)
        except exceptions.CancelledError as exc:
            # keep the original exception to chain it later
            self._cancelled_exc = exc
            super().cancel()
        except (KeyboardInterrupt, SystemExit) as exc:
            super().set_exception(exc)
            raise
        except BaseException as exc:
            super().set_exception(exc)
        else:
            blocking = getattr(result, '_asyncio_future_blocking', None)
            if blocking is not None:
                # a Future yielded by Future.__await__()
                if futures._get_loop(result) is not self._loop:
                    new_exc = RuntimeError(
                        f'Task {self!r} got Future '
                        f'{result!r} attached to a different loop')
                    self._loop.call_soon(
                        self.__step, new_exc, context=self._context)
                elif blocking:
                    if result is self:
                        new_exc = RuntimeError(
                            f'Task cannot await on itself: {self!r}')
                        self._loop.call_soon(
                            self.__step, new_exc, context=self._context)
                    else:
                        futures.future_add_to_awaited_by(result, self)
                        result._asyncio_future_blocking = False
                        result.add_done_callback(
                            self.__wakeup, context=self._context)
                        self._fut_waiter = result
                        if self._must_cancel:
                            if self._fut_waiter.cancel(
                                    msg=self._cancel_message):
                                self._must_cancel = False
                else:
                    new_exc = RuntimeError(
                        f'yield was used instead of yield from '
                        f'in task {self!r} with {result!r}')
                    self._loop.call_soon(
                        self.__step, new_exc, context=self._context)

            elif result is None:
                # a bare yield gives up one loop iteration
                self._loop.call_soon(self.__step, context=self._context)
            elif inspect.isgenerator(result):
                new_exc = RuntimeError(
                    f'yield was used instead of yield from for '
                    f'generator in task {self!r} with {result!r}')
                self._loop.call_soon(
                    self.__step, new_exc, context=self._context)
            else:
                new_exc = RuntimeError(f'Task got bad yield: {result!r}')
                self._loop.call_soon(
                    self.__step, new_exc, context=self._context)
        finally:
            self = None  # break reference cycles

    def __wakeup(self, future):
        futures.future_discard_from_awaited_by(future, self)
        try:
            future.result()
        except BaseException as exc:
            # possibly a cancellation
            self.__step(exc)
        else:
            # Future.__await__ does not need the value: resume with
            # send(None), the fast path.
            self.__step()
        self = None  # break reference cycles


_PyTask = Task


try:
    import _asyncio
except ImportError:
    pass
else:
    # _CTask is needed for tests.
    Task = _CTask = _asyncio.Task


def create_task(coro, **kwargs):
    """Schedule the execution of a coroutine object in a spawn task.

    Return a Task object.
    """
    loop = events.get_running_loop()
    return loop.create_task(coro, **kwargs)


# wait() and as_wait() return values (same as concurrent.futures).
FIRST_COMPLETED = 'FIRST_COMPLETED'
FIRST_EXCEPTION = 'FIRST_EXCEPTION'
ALL_COMPLETED = 'ALL_COMPLETED'


async def wait(fs, *, timeout=None, return_when=ALL_COMPLETED):
    """Wait for the Futures or Tasks in the iterable *fs* to complete.

    Returns two sets of futures: (done, pending).  Coroutines are not
    accepted; *timeout* does not cancel anything.
    """
    if futures.isfuture(fs) or coroutines.iscoroutine(fs):
        raise TypeError(f"expect a list of futures, not {type(fs).__name__}")
    if not fs:
        raise ValueError('Set of Tasks/Futures is empty.')
    if return_when not in (FIRST_COMPLETED, FIRST_EXCEPTION, ALL_COMPLETED):
        raise ValueError(f'Invalid return_when value: {return_when}')

    fs = set(fs)

    if any(coroutines.iscoroutine(f) for f in fs):
        raise TypeError("Passing coroutines is forbidden, use tasks explicitly.")

    loop = events.get_running_loop()
    return await _wait(fs, timeout, return_when, loop)


def _release_waiter(waiter, *args):
    if not waiter.done():
        waiter.set_result(None)


async def wait_for(fut, timeout):
    """Wait for the awaitable to complete, with a timeout.

    On timeout the awaitable is cancelled and TimeoutError is raised; if
    the waiting task is cancelled, the awaitable is cancelled too.
    """
    # timeout <= 0: cancel right away, but still let the awaitable finish
    # if it is already done (or finishes while being cancelled).
    if timeout is not None and timeout <= 0:
        fut = ensure_future(fut)

        if fut.done():
            return fut.result()

        await _cancel_and_wait(fut)
        try:
            return fut.result()
        except exceptions.CancelledError as exc:
            raise TimeoutError from exc

    async with timeouts.timeout(timeout):
        return await fut


async def _wait(fs, timeout, return_when, loop):
    """Internal helper for wait(); *fs* is a non-empty set of futures."""
    assert fs, 'Set of Futures is empty.'
    waiter = loop.create_future()
    timeout_handle = None
    if timeout is not None:
        timeout_handle = loop.call_later(timeout, _release_waiter, waiter)
    counter = len(fs)
    cur_task = current_task()

    def _on_completion(f):
        nonlocal counter
        counter -= 1
        if (counter <= 0 or
            return_when == FIRST_COMPLETED or
            return_when == FIRST_EXCEPTION and (not f.cancelled() and
                                                f.exception() is not None)):
            if timeout_handle is not None:
                timeout_handle.cancel()
            if not waiter.done():
                waiter.set_result(None)
        futures.future_discard_from_awaited_by(f, cur_task)

    for f in fs:
        f.add_done_callback(_on_completion)
        futures.future_add_to_awaited_by(f, cur_task)

    try:
        await waiter
    finally:
        if timeout_handle is not None:
            timeout_handle.cancel()
        for f in fs:
            f.remove_done_callback(_on_completion)
            futures.future_discard_from_awaited_by(f, cur_task)

    done, pending = set(), set()
    for f in fs:
        if f.done():
            done.add(f)
        else:
            pending.add(f)
    return done, pending


async def _cancel_and_wait(fut):
    """Cancel *fut* and wait until it is really done."""
    loop = events.get_running_loop()
    waiter = loop.create_future()
    cb = functools.partial(_release_waiter, waiter)
    fut.add_done_callback(cb)

    try:
        fut.cancel()
        # the cancellation may be ignored: wait for completion either way
        await waiter
    finally:
        fut.remove_done_callback(cb)


class _AsCompletedIterator:
    """Iterator (sync and async) of futures in completion order."""

    def __init__(self, aws, timeout):
        self._done = queues.Queue()
        self._timeout_handle = None

        loop = events.get_event_loop()
        todo = {ensure_future(aw, loop=loop) for aw in set(aws)}
        for f in todo:
            f.add_done_callback(self._handle_completion)
        if todo and timeout is not None:
            self._timeout_handle = (
                loop.call_later(timeout, self._handle_timeout)
            )
        self._todo = todo
        self._all = set(todo)
        self._todo_left = len(todo)

    def __aiter__(self):
        return self

    def __iter__(self):
        return self

    async def __anext__(self):
        if not self._todo_left:
            raise StopAsyncIteration
        assert self._todo_left > 0
        self._todo_left -= 1
        return await self._wait_for_one()

    def __next__(self):
        if not self._todo_left:
            raise StopIteration
        assert self._todo_left > 0
        self._todo_left -= 1
        return self._wait_for_one(resolve=True)

    def _handle_timeout(self):
        for f in self._todo:
            f.remove_done_callback(self._handle_completion)
            self._done.put_nowait(None)  # wakes up _wait_for_one()
        self._todo.clear()

    def _handle_completion(self, f):
        if not self._todo:
            return  # timed out already
        self._todo.remove(f)
        self._done.put_nowait(f)
        if not self._todo and self._timeout_handle is not None:
            self._timeout_handle.cancel()

    async def _wait_for_one(self, resolve=False):
        # Return the next done future, or (resolve=True) its result.
        cur_task = current_task()
        if cur_task is not None:
            for fut in self._todo:
                futures.future_add_to_awaited_by(fut, cur_task)
        try:
            f = await self._done.get()
        finally:
            if cur_task is not None:
                for fut in self._all:
                    futures.future_discard_from_awaited_by(fut, cur_task)
        if f is None:
            # sentinel from _handle_timeout()
            raise exceptions.TimeoutError
        return f.result() if resolve else f


def as_completed(fs, *, timeout=None):
    """Iterate over awaitables in the order they complete.

    Usable with "for" (yields awaitables) and "async for" (yields done
    futures).  TimeoutError is raised if *timeout* expires first.
    """
    if futures.isfuture(fs) or coroutines.iscoroutine(fs):
        raise TypeError(f"expect an iterable of futures, not {type(fs).__name__}")

    return _AsCompletedIterator(fs, timeout)


@types.coroutine
def __sleep0():
    """Skip one event loop run cycle (bare yield, see Task.__step)."""
    yield


async def sleep(delay, result=None):
    """Coroutine that completes after a given time (in seconds)."""
    if delay <= 0:
        await __sleep0()
        return result

    if math.isnan(delay):
        raise ValueError("Invalid delay: NaN (not a number)")

    loop = events.get_running_loop()
    future = loop.create_future()
    h = loop.call_later(delay,
                        futures._set_result_unless_cancelled,
                        future, result)
    try:
        return await future
    finally:
        h.cancel()


def ensure_future(coro_or_future, *, loop=None):
    """Wrap a coroutine or an awaitable in a future.

    Futures are returned unchanged.
    """
    if futures.isfuture(coro_or_future):
        if loop is not None and loop is not futures._get_loop(coro_or_future):
            raise ValueError('The future belongs to a different loop than '
                             'the one specified as the loop argument')
        return coro_or_future
    should_close = True
    if not coroutines.iscoroutine(coro_or_future):
        if inspect.isawaitable(coro_or_future):
            async def _wrap_awaitable(awaitable):
                return await awaitable

            coro_or_future = _wrap_awaitable(coro_or_future)
            should_close = False
        else:
            raise TypeError('An asyncio.Future, a coroutine or an awaitable '
                            'is required')

    if loop is None:
        loop = events.get_event_loop()
    try:
        return loop.create_task(coro_or_future)
    except RuntimeError:
        if should_close:
            coro_or_future.close()
        raise


class _GatheringFuture(futures.Future):
    """The future returned by gather(): cancelling it cancels the children."""

    def __init__(self, children, *, loop):
        assert loop is not None
        super().__init__(loop=loop)
        self._children = children
        self._cancel_requested = False

    def cancel(self, msg=None):
        if self.done():
            return False
        ret = False
        for child in self._children:
            if child.cancel(msg=msg):
                ret = True
        if ret:
            # a real child cancellation propagates whatever the value of
            # return_exceptions
            self._cancel_requested = True
        return ret


def gather(*coros_or_futures, return_exceptions=False):
    """Return a future aggregating results from the given coroutines/futures.

    Results keep the order of the arguments.  With return_exceptions=True
    exceptions are returned as results; otherwise the first exception is
    propagated (the other awaitables keep running).  Cancelling the gather
    future cancels all unfinished children.
    """
    if not coros_or_futures:
        loop = events.get_event_loop()
        outer = loop.create_future()
        outer.set_result([])
        return outer

    loop = events._get_running_loop()
    if loop is not None:
        cur_task = current_task(loop)
    else:
        cur_task = None

    def _done_callback(fut, cur_task=cur_task):
        nonlocal nfinished
        nfinished += 1

        if cur_task is not None:
            futures.future_discard_from_awaited_by(fut, cur_task)

        if outer is None or outer.done():
            if not fut.cancelled():
                # mark the exception as retrieved
                fut.exception()
            return

        if not return_exceptions:
            if fut.cancelled():
                # exception() would raise CancelledError
                exc = fut._make_cancelled_error()
                outer.set_exception(exc)
                return
            else:
                exc = fut.exception()
                if exc is not None:
                    outer.set_exception(exc)
                    return

        if nfinished == nfuts:
            results = []

            for fut in children:
                if fut.cancelled():
                    res = exceptions.CancelledError(
                        '' if fut._cancel_message is None else
                        fut._cancel_message)
                else:
                    res = fut.exception()
                    if res is None:
                        res = fut.result()
                results.append(res)

            if outer._cancel_requested:
                exc = fut._make_cancelled_error()
                outer.set_exception(exc)
            else:
                outer.set_result(results)

    arg_to_fut = {}
    children = []
    nfuts = 0
    nfinished = 0
    done_futs = []
    outer = None
    for arg in coros_or_futures:
        if arg not in arg_to_fut:
            fut = ensure_future(arg, loop=loop)
            if loop is None:
                loop = futures._get_loop(fut)
            if fut is not arg:
                # a task created for the caller: it cannot be awaited
                # elsewhere, so do not warn if it is destroyed pending
                fut._log_destroy_pending = False

            nfuts += 1
            arg_to_fut[arg] = fut
            if fut.done():
                done_futs.append(fut)
            else:
                if cur_task is not None:
                    futures.future_add_to_awaited_by(fut, cur_task)
                fut.add_done_callback(_done_callback)

        else:
            # duplicate argument
            fut = arg_to_fut[arg]

        children.append(fut)

    outer = _GatheringFuture(children, loop=loop)
    # Already finished (e.g. eager) children are processed now that outer
    # exists; if all are done this completes the gather immediately.
    for fut in done_futs:
        _done_callback(fut)
    return outer


def shield(arg):
    """Wait for a future, shielding it from cancellation.

    Cancelling the task awaiting shield(x) does not cancel x.
    """
    inner = ensure_future(arg)
    if inner.done():
        return inner
    loop = futures._get_loop(inner)
    outer = loop.create_future()

    if loop is not None and (cur_task := current_task(loop)) is not None:
        futures.future_add_to_awaited_by(inner, cur_task)
    else:
        cur_task = None

    def _clear_awaited_by_callback(inner):
        futures.future_discard_from_awaited_by(inner, cur_task)

    def _inner_done_callback(inner):
        if outer.cancelled():
            if not inner.cancelled():
                # mark the result as retrieved
                inner.exception()
            return

        if inner.cancelled():
            outer.cancel()
        else:
            exc = inner.exception()
            if exc is not None:
                outer.set_exception(exc)
            else:
                outer.set_result(inner.result())

    def _outer_done_callback(outer):
        if not inner.done():
            inner.remove_done_callback(_inner_done_callback)

    if cur_task is not None:
        inner.add_done_callback(_clear_awaited_by_callback)

    inner.add_done_callback(_inner_done_callback)
    outer.add_done_callback(_outer_done_callback)
    return outer


def run_coroutine_threadsafe(coro, loop):
    """Submit a coroutine object to a given event loop (from another thread).

    Return a concurrent.futures.Future to access the result.
    """
    import concurrent.futures

    if not coroutines.iscoroutine(coro):
        raise TypeError('A coroutine object is required')
    future = concurrent.futures.Future()

    def callback():
        try:
            futures._chain_future(ensure_future(coro, loop=loop), future)
        except (SystemExit, KeyboardInterrupt):
            raise
        except BaseException as exc:
            if future.set_running_or_notify_cancel():
                future.set_exception(exc)
            raise

    loop.call_soon_threadsafe(callback)
    return future


def create_eager_task_factory(custom_task_constructor):
    """Create a task factory that starts tasks eagerly.

    The coroutine runs synchronously until its first suspension; if it
    completes without suspending, the task is returned already done.
    """

    def factory(loop, coro, *, eager_start=True, **kwargs):
        return custom_task_constructor(
            coro, loop=loop, eager_start=eager_start, **kwargs)

    return factory


eager_task_factory = create_eager_task_factory(Task)


# Active tasks: scheduled tasks are weakly referenced, eager tasks (while
# running their first step) are kept in a plain set.
_scheduled_tasks = weakref.WeakSet()
_eager_tasks = set()

# {event loop: currently running task}
_current_tasks = {}


def _register_task(task):
    """Register an asyncio Task scheduled to run on an event loop."""
    _scheduled_tasks.add(task)


def _register_eager_task(task):
    """Register an asyncio Task about to be eagerly executed."""
    _eager_tasks.add(task)


def _enter_task(loop, task):
    current_task = _current_tasks.get(loop)
    if current_task is not None:
        raise RuntimeError(f"Cannot enter into task {task!r} while another "
                           f"task {current_task!r} is being executed.")
    _current_tasks[loop] = task


def _leave_task(loop, task):
    current_task = _current_tasks.get(loop)
    if current_task is not task:
        raise RuntimeError(f"Leaving task {task!r} does not match "
                           f"the current task {current_task!r}.")
    del _current_tasks[loop]


def _swap_current_task(loop, task):
    prev_task = _current_tasks.get(loop)
    if task is None:
        del _current_tasks[loop]
    else:
        _current_tasks[loop] = task
    return prev_task


def _unregister_task(task):
    """Unregister a completed, scheduled Task."""
    _scheduled_tasks.discard(task)


def _unregister_eager_task(task):
    """Unregister a task which finished its first eager step."""
    _eager_tasks.discard(task)


_py_current_task = current_task
_py_register_task = _register_task
_py_register_eager_task = _register_eager_task
_py_unregister_task = _unregister_task
_py_unregister_eager_task = _unregister_eager_task
_py_enter_task = _enter_task
_py_leave_task = _leave_task
_py_swap_current_task = _swap_current_task
_py_all_tasks = all_tasks

try:
    from _asyncio import (_register_task, _register_eager_task,
                          _unregister_task, _unregister_eager_task,
                          _enter_task, _leave_task, _swap_current_task,
                          current_task, all_tasks)
except ImportError:
    pass
else:
    _c_current_task = current_task
    _c_register_task = _register_task
    _c_register_eager_task = _register_eager_task
    _c_unregister_task = _unregister_task
    _c_unregister_eager_task = _unregister_eager_task
    _c_enter_task = _enter_task
    _c_leave_task = _leave_task
    _c_swap_current_task = _swap_current_task
    _c_all_tasks = all_tasks
