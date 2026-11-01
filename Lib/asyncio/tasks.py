"""Tasks (coroutines driven by the event loop) and coordination helpers."""

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

_task_names = itertools.count(1)


# ---------------------------------------------------------------- registry

# Tasks scheduled on some loop (weakly held) and tasks currently running
# their first, eager step (strongly held until they suspend or finish).
_scheduled_tasks = weakref.WeakSet()
_eager_tasks = set()

# loop -> task currently executing a step on that loop
_current_tasks = {}


def _register_task(task):
    """Add a task to the set of known tasks."""
    _scheduled_tasks.add(task)


def _register_eager_task(task):
    _eager_tasks.add(task)


def _unregister_task(task):
    _scheduled_tasks.discard(task)


def _unregister_eager_task(task):
    _eager_tasks.discard(task)


def _enter_task(loop, task):
    running = _current_tasks.get(loop)
    if running is not None:
        raise RuntimeError(f"Cannot enter into task {task!r} while another "
                           f"task {running!r} is being executed.")
    _current_tasks[loop] = task


def _leave_task(loop, task):
    running = _current_tasks.get(loop)
    if running is not task:
        raise RuntimeError(f"Leaving task {task!r} does not match "
                           f"the current task {running!r}.")
    del _current_tasks[loop]


def _swap_current_task(loop, task):
    previous = _current_tasks.get(loop)
    if task is None:
        _current_tasks.pop(loop, None)
    else:
        _current_tasks[loop] = task
    return previous


def current_task(loop=None):
    """The task currently running on *loop* (default: the running loop)."""
    if loop is None:
        loop = events.get_running_loop()
    return _current_tasks.get(loop)


def all_tasks(loop=None):
    """Unfinished tasks of *loop* (default: the running loop)."""
    if loop is None:
        loop = events.get_running_loop()
    # Copying a WeakSet can fail if it changes size concurrently.
    for attempt in range(1000):
        try:
            candidates = list(_scheduled_tasks)
            break
        except RuntimeError:
            if attempt == 999:
                raise
    candidates += list(_eager_tasks)
    return {t for t in candidates
            if futures._get_loop(t) is loop and not t.done()}


# ---------------------------------------------------------------- Task

class Task(futures._PyFuture):
    """A future driving a coroutine on the event loop."""

    # Whether destroying a still-pending task gets reported.
    _log_destroy_pending = True

    def __init__(self, coro, *, loop=None, name=None, context=None,
                 eager_start=False):
        super().__init__(loop=loop)
        if self._source_traceback:
            self._source_traceback.pop()
        if not coroutines.iscoroutine(coro):
            self._log_destroy_pending = False
            raise TypeError(f"a coroutine was expected, got {coro!r}")
        if context is not None and not isinstance(context, contextvars.Context):
            self._log_destroy_pending = False
            raise TypeError(f"context must be a contextvars.Context, not "
                            f"{type(context).__name__}")
        self._name = f'Task-{next(_task_names)}' if name is None else str(name)
        self._num_cancels_requested = 0
        self._must_cancel = False
        self._fut_waiter = None
        self._coro = coro
        self._context = contextvars.copy_context() if context is None \
            else context

        if eager_start and self._loop.is_running():
            self.__start_eagerly()
        else:
            self._loop.call_soon(self.__step, context=self._context)
            _py_register_task(self)

    def __del__(self):
        if self._state == futures._PENDING and self._log_destroy_pending:
            report = {'task': self,
                      'message': 'Task was destroyed but it is pending!'}
            if self._source_traceback:
                report['source_traceback'] = self._source_traceback
            self._loop.call_exception_handler(report)
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
        """Frames of the suspended coroutine (or of its exception's
        traceback once finished)."""
        return base_tasks._task_get_stack(self, limit)

    def print_stack(self, *, limit=None, file=None):
        """Print get_stack() like a traceback."""
        return base_tasks._task_print_stack(self, limit, file)

    def cancel(self, msg=None):
        """Request cancellation: CancelledError is thrown into the coroutine
        at its next suspension point.  False if the task is done."""
        self._log_traceback = False
        if self.done():
            return False
        self._num_cancels_requested += 1
        waiter = self._fut_waiter
        if waiter is not None and waiter.cancel(msg=msg):
            # the awaited future delivers the cancellation; it is kept as
            # _fut_waiter because a task may swallow it and need another
            return True
        self._must_cancel = True
        self._cancel_message = msg
        return True

    def cancelling(self):
        """Number of cancellation requests not yet withdrawn."""
        return self._num_cancels_requested

    def uncancel(self):
        """Withdraw one cancellation request; return the remaining count."""
        if self._num_cancels_requested > 0:
            self._num_cancels_requested -= 1
            if not self._num_cancels_requested:
                self._must_cancel = False
        return self._num_cancels_requested

    # -- execution

    def __start_eagerly(self):
        previous = _py_swap_current_task(self._loop, self)
        try:
            _py_register_eager_task(self)
            try:
                self._context.run(self.__resume, None)
            finally:
                _py_unregister_eager_task(self)
        finally:
            try:
                displaced = _py_swap_current_task(self._loop, previous)
                assert displaced is self
            finally:
                if self.done():
                    self._coro = None
                else:
                    _py_register_task(self)
                self = None

    def __step(self, exc=None):
        if self.done():
            raise exceptions.InvalidStateError(
                f'__step(): already done: {self!r}, {exc!r}')
        if self._must_cancel:
            self._must_cancel = False
            if not isinstance(exc, exceptions.CancelledError):
                exc = self._make_cancelled_error()
        self._fut_waiter = None
        _py_enter_task(self._loop, self)
        try:
            self.__resume(exc)
        finally:
            _py_leave_task(self._loop, self)
            self = None

    def __reschedule(self, exc=None):
        if exc is None:
            self._loop.call_soon(self.__step, context=self._context)
        else:
            self._loop.call_soon(self.__step, exc, context=self._context)

    def __resume(self, exc):
        """Advance the coroutine by one step and act on what it yields."""
        try:
            if exc is None:
                yielded = self._coro.send(None)
            else:
                yielded = self._coro.throw(exc)
        except StopIteration as stop:
            if self._must_cancel:
                # cancelled right as the coroutine returned
                self._must_cancel = False
                super().cancel(msg=self._cancel_message)
            else:
                super().set_result(stop.value)
        except exceptions.CancelledError as cancelled:
            self._cancelled_exc = cancelled
            super().cancel()
        except (KeyboardInterrupt, SystemExit) as fatal:
            super().set_exception(fatal)
            raise
        except BaseException as failure:
            super().set_exception(failure)
        else:
            self.__on_yield(yielded)
        finally:
            self = None

    def __on_yield(self, yielded):
        blocking = getattr(yielded, '_asyncio_future_blocking', None)
        if blocking is None:
            if yielded is None:
                # bare yield: give other callbacks one loop iteration
                self.__reschedule()
            elif inspect.isgenerator(yielded):
                self.__reschedule(RuntimeError(
                    f'yield was used instead of yield from for '
                    f'generator in task {self!r} with {yielded!r}'))
            else:
                self.__reschedule(RuntimeError(
                    f'Task got bad yield: {yielded!r}'))
            return
        if futures._get_loop(yielded) is not self._loop:
            self.__reschedule(RuntimeError(
                f'Task {self!r} got Future {yielded!r} attached to a '
                f'different loop'))
        elif not blocking:
            self.__reschedule(RuntimeError(
                f'yield was used instead of yield from in task {self!r} '
                f'with {yielded!r}'))
        elif yielded is self:
            self.__reschedule(RuntimeError(
                f'Task cannot await on itself: {self!r}'))
        else:
            yielded._asyncio_future_blocking = False
            futures.future_add_to_awaited_by(yielded, self)
            yielded.add_done_callback(self.__wakeup, context=self._context)
            self._fut_waiter = yielded
            if self._must_cancel and yielded.cancel(msg=self._cancel_message):
                self._must_cancel = False

    def __wakeup(self, awaited):
        futures.future_discard_from_awaited_by(awaited, self)
        try:
            awaited.result()
        except BaseException as exc:
            self.__step(exc)
        else:
            # the value is fetched again by Future.__await__ itself
            self.__step()
        self = None


_PyTask = Task

try:
    import _asyncio
except ImportError:
    pass
else:
    Task = _CTask = _asyncio.Task


def create_task(coro, **kwargs):
    """Wrap *coro* in a Task on the running loop and return it."""
    return events.get_running_loop().create_task(coro, **kwargs)


# ---------------------------------------------------------------- waiting

FIRST_COMPLETED = 'FIRST_COMPLETED'
FIRST_EXCEPTION = 'FIRST_EXCEPTION'
ALL_COMPLETED = 'ALL_COMPLETED'


def _release_waiter(waiter, *args):
    if not waiter.done():
        waiter.set_result(None)


async def wait(fs, *, timeout=None, return_when=ALL_COMPLETED):
    """Wait for futures/tasks from *fs*; return (done, pending) sets."""
    if futures.isfuture(fs) or coroutines.iscoroutine(fs):
        raise TypeError(f"expect a list of futures, not {type(fs).__name__}")
    if not fs:
        raise ValueError('Set of Tasks/Futures is empty.')
    if return_when not in (FIRST_COMPLETED, FIRST_EXCEPTION, ALL_COMPLETED):
        raise ValueError(f'Invalid return_when value: {return_when}')
    fs = set(fs)
    if any(coroutines.iscoroutine(f) for f in fs):
        raise TypeError("Passing coroutines is forbidden, use tasks explicitly.")
    return await _wait(fs, timeout, return_when, events.get_running_loop())


async def _wait(fs, timeout, return_when, loop):
    assert fs, 'Set of Futures is empty.'
    wakeup = loop.create_future()
    timer = None if timeout is None else \
        loop.call_later(timeout, _release_waiter, wakeup)
    me = current_task()
    left = len(fs)

    def on_done(f):
        nonlocal left
        left -= 1
        futures.future_discard_from_awaited_by(f, me)
        finished = (left <= 0 or return_when == FIRST_COMPLETED or
                    (return_when == FIRST_EXCEPTION and not f.cancelled()
                     and f.exception() is not None))
        if finished:
            if timer is not None:
                timer.cancel()
            _release_waiter(wakeup)

    for f in fs:
        f.add_done_callback(on_done)
        futures.future_add_to_awaited_by(f, me)
    try:
        await wakeup
    finally:
        if timer is not None:
            timer.cancel()
        for f in fs:
            f.remove_done_callback(on_done)
            futures.future_discard_from_awaited_by(f, me)
    done = {f for f in fs if f.done()}
    return done, fs - done


async def _cancel_and_wait(fut):
    """Cancel *fut* and wait until it is done (it may refuse to cancel)."""
    loop = events.get_running_loop()
    finished = loop.create_future()
    notify = functools.partial(_release_waiter, finished)
    fut.add_done_callback(notify)
    me = current_task(loop)
    if me is not None:
        futures.future_add_to_awaited_by(fut, me)
    try:
        fut.cancel()
        await finished
    finally:
        fut.remove_done_callback(notify)
        if me is not None:
            futures.future_discard_from_awaited_by(fut, me)


async def wait_for(fut, timeout):
    """Await *fut* with a time limit; on expiry cancel it and raise
    TimeoutError."""
    if timeout is None or timeout > 0:
        async with timeouts.timeout(timeout):
            return await fut
    # Non-positive timeout: cancel immediately, but a future that is
    # already done (or completes while being cancelled) still wins.
    fut = ensure_future(fut)
    if not fut.done():
        await _cancel_and_wait(fut)
    try:
        return fut.result()
    except exceptions.CancelledError as exc:
        raise TimeoutError from exc


class _AsCompletedIterator:
    """Backs as_completed(): plain iteration yields awaitables, async
    iteration yields the finished futures."""

    def __init__(self, aws, timeout):
        loop = events.get_event_loop()
        self._finished = queues.Queue()
        self._pending = {ensure_future(aw, loop=loop) for aw in set(aws)}
        self._everything = set(self._pending)
        self._remaining = len(self._pending)
        for fut in self._pending:
            fut.add_done_callback(self._on_done)
        self._timer = None
        if self._pending and timeout is not None:
            self._timer = loop.call_later(timeout, self._on_timeout)

    def _on_done(self, fut):
        if not self._pending:
            return   # already timed out
        self._pending.discard(fut)
        self._finished.put_nowait(fut)
        if not self._pending and self._timer is not None:
            self._timer.cancel()

    def _on_timeout(self):
        for fut in self._pending:
            fut.remove_done_callback(self._on_done)
            self._finished.put_nowait(None)   # one wake-up per waiter
        self._pending.clear()

    def __iter__(self):
        return self

    def __next__(self):
        if not self._remaining:
            raise StopIteration
        self._remaining -= 1
        return self._wait_for_one(resolve=True)

    def __aiter__(self):
        return self

    async def __anext__(self):
        if not self._remaining:
            raise StopAsyncIteration
        self._remaining -= 1
        return await self._wait_for_one()

    async def _wait_for_one(self, resolve=False):
        me = current_task()
        if me is not None:
            for fut in self._pending:
                futures.future_add_to_awaited_by(fut, me)
        try:
            fut = await self._finished.get()
        except exceptions.CancelledError:
            # a cancelled wait does not use up one of the results
            self._remaining += 1
            raise
        finally:
            if me is not None:
                for other in self._everything:
                    futures.future_discard_from_awaited_by(other, me)
        if fut is None:
            raise exceptions.TimeoutError
        return fut.result() if resolve else fut


def as_completed(fs, *, timeout=None):
    """Iterate over the awaitables in *fs* in completion order (sync or
    async iteration); TimeoutError once *timeout* expires."""
    if futures.isfuture(fs) or coroutines.iscoroutine(fs):
        raise TypeError(f"expect an iterable of futures, not {type(fs).__name__}")
    return _AsCompletedIterator(fs, timeout)


@types.coroutine
def _yield_once():
    yield


async def sleep(delay, result=None):
    """Suspend for *delay* seconds and return *result*."""
    if delay <= 0:
        await _yield_once()
        return result
    if math.isnan(delay):
        raise ValueError("Invalid delay: NaN (not a number)")
    loop = events.get_running_loop()
    alarm = loop.create_future()
    timer = loop.call_later(delay, futures._set_result_unless_cancelled,
                            alarm, result)
    try:
        return await alarm
    finally:
        timer.cancel()


async def _await_any(awaitable):
    return await awaitable


def ensure_future(coro_or_future, *, loop=None):
    """Return a future for *coro_or_future* (futures pass through,
    coroutines and awaitables are wrapped in tasks)."""
    if futures.isfuture(coro_or_future):
        if loop is not None and loop is not futures._get_loop(coro_or_future):
            raise ValueError('The future belongs to a different loop than '
                             'the one specified as the loop argument')
        return coro_or_future
    owned = True
    if not coroutines.iscoroutine(coro_or_future):
        if not inspect.isawaitable(coro_or_future):
            raise TypeError('An asyncio.Future, a coroutine or an awaitable '
                            'is required')
        coro_or_future = _await_any(coro_or_future)
        owned = False
    if loop is None:
        loop = events.get_event_loop()
    try:
        return loop.create_task(coro_or_future)
    except RuntimeError:
        if owned:
            coro_or_future.close()
        raise


class _GatheringFuture(futures.Future):
    """Result future of gather(); cancelling it cancels the children."""

    def __init__(self, children, *, loop):
        assert loop is not None
        super().__init__(loop=loop)
        self._children = children
        self._cancel_requested = False

    def cancel(self, msg=None):
        if self.done():
            return False
        hit = [child.cancel(msg=msg) for child in self._children]
        if any(hit):
            # propagate cancellation even with return_exceptions=True
            self._cancel_requested = True
            return True
        return False


def gather(*coros_or_futures, return_exceptions=False):
    """Run awaitables concurrently; the returned future's result is the
    list of their results in argument order.  The first exception is
    propagated unless *return_exceptions* is true."""
    if not coros_or_futures:
        empty = events.get_event_loop().create_future()
        empty.set_result([])
        return empty

    loop = events._get_running_loop()
    parent = current_task(loop) if loop is not None else None
    children = []
    unique = {}
    already_done = []
    outer = None
    count = 0
    finished = 0

    def forget_pending():
        # children that outlive gather() are no longer awaited by parent
        if parent is not None:
            for item in children:
                if not item.done():
                    futures.future_discard_from_awaited_by(item, parent)

    def child_done(child):
        nonlocal finished
        finished += 1
        if parent is not None:
            futures.future_discard_from_awaited_by(child, parent)
        if outer is None or outer.done():
            if not child.cancelled():
                child.exception()   # mark as retrieved
            return
        if not return_exceptions:
            if child.cancelled():
                outer.set_exception(child._make_cancelled_error())
                forget_pending()
                return
            failure = child.exception()
            if failure is not None:
                outer.set_exception(failure)
                forget_pending()
                return
        if finished < count:
            return
        values = []
        for item in children:
            if item.cancelled():
                values.append(exceptions.CancelledError(
                    '' if item._cancel_message is None
                    else item._cancel_message))
            else:
                failure = item.exception()
                values.append(item.result() if failure is None else failure)
        if outer._cancel_requested:
            outer.set_exception(child._make_cancelled_error())
        else:
            outer.set_result(values)

    for arg in coros_or_futures:
        fut = unique.get(arg)
        if fut is None:
            fut = ensure_future(arg, loop=loop)
            if loop is None:
                loop = futures._get_loop(fut)
            if fut is not arg:
                # created here, nobody else can await it
                fut._log_destroy_pending = False
            unique[arg] = fut
            count += 1
            if fut.done():
                already_done.append(fut)
            else:
                if parent is not None:
                    futures.future_add_to_awaited_by(fut, parent)
                fut.add_done_callback(child_done)
        children.append(fut)

    outer = _GatheringFuture(children, loop=loop)
    for fut in already_done:
        child_done(fut)
    return outer


def shield(arg):
    """Protect an awaitable from being cancelled by its awaiter."""
    inner = ensure_future(arg)
    if inner.done():
        return inner
    loop = futures._get_loop(inner)
    outer = loop.create_future()
    parent = current_task(loop) if loop is not None else None
    if parent is not None:
        futures.future_add_to_awaited_by(inner, parent)

    def inner_done(inner):
        if parent is not None:
            futures.future_discard_from_awaited_by(inner, parent)
        if outer.cancelled():
            if not inner.cancelled():
                inner.exception()   # mark as retrieved
            return
        if inner.cancelled():
            outer.cancel()
            return
        failure = inner.exception()
        if failure is None:
            outer.set_result(inner.result())
        else:
            outer.set_exception(failure)

    def outer_done(outer):
        if not inner.done():
            inner.remove_done_callback(inner_done)
            if parent is not None:
                futures.future_discard_from_awaited_by(inner, parent)

    inner.add_done_callback(inner_done)
    outer.add_done_callback(outer_done)
    return outer


def run_coroutine_threadsafe(coro, loop):
    """Schedule *coro* on *loop* from another thread; return a
    concurrent.futures.Future."""
    import concurrent.futures
    if not coroutines.iscoroutine(coro):
        raise TypeError('A coroutine object is required')
    result = concurrent.futures.Future()

    def start():
        try:
            futures._chain_future(ensure_future(coro, loop=loop), result)
        except (SystemExit, KeyboardInterrupt):
            raise
        except BaseException as exc:
            if result.set_running_or_notify_cancel():
                result.set_exception(exc)
            raise

    loop.call_soon_threadsafe(start)
    return result


def create_eager_task_factory(custom_task_constructor):
    """Task factory running each new task's first step immediately."""
    def factory(loop, coro, *, eager_start=True, **kwargs):
        return custom_task_constructor(coro, loop=loop,
                                       eager_start=eager_start, **kwargs)
    return factory


eager_task_factory = create_eager_task_factory(Task)


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
    import _asyncio as _accel
except ImportError:
    pass
else:
    _c_current_task = current_task = _accel.current_task
    _c_all_tasks = all_tasks = _accel.all_tasks
    _c_register_task = _register_task = _accel._register_task
    _c_register_eager_task = _register_eager_task = _accel._register_eager_task
    _c_unregister_task = _unregister_task = _accel._unregister_task
    _c_unregister_eager_task = _unregister_eager_task = \
        _accel._unregister_eager_task
    _c_enter_task = _enter_task = _accel._enter_task
    _c_leave_task = _leave_task = _accel._leave_task
    _c_swap_current_task = _swap_current_task = _accel._swap_current_task
    del _accel
