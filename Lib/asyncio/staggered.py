"""Support for running coroutines in parallel with staggered start times."""

__all__ = 'staggered_race',

import contextlib

from . import events
from . import exceptions as exceptions_mod
from . import futures
from . import locks
from . import tasks


async def staggered_race(coro_fns, delay, *, loop=None):
    """Run coroutines with staggered start times and take the first to finish.

    Each coroutine function in *coro_fns* is started either when the
    previous one failed or *delay* seconds after it started, whichever
    comes first (delay None: wait for the failure).  The first coroutine
    to complete successfully wins and all others are cancelled.

    Returns (winner_result, winner_index, exceptions): winner_index is None
    if all coroutines failed; exceptions lists the exception of each
    started coroutine (None for the winner and cancelled ones).
    """
    loop = loop or events.get_running_loop()
    parent_task = tasks.current_task(loop)
    enum_coro_fns = enumerate(coro_fns)
    winner_result = None
    winner_index = None
    unhandled_exceptions = []
    exceptions = []
    running_tasks = set()
    on_completed_fut = None

    def task_done(task):
        running_tasks.discard(task)
        futures.future_discard_from_awaited_by(task, parent_task)
        if (
            on_completed_fut is not None
            and not on_completed_fut.done()
            and not running_tasks
        ):
            on_completed_fut.set_result(None)

        if task.cancelled():
            return

        exc = task.exception()
        if exc is None:
            return
        unhandled_exceptions.append(exc)

    async def run_one_coro(ok_to_start, previous_failed) -> None:
        # With eager tasks this lets the caller register the task in
        # running_tasks before the coroutine starts.
        await ok_to_start.wait()
        if previous_failed is not None:
            with contextlib.suppress(exceptions_mod.TimeoutError):
                # wait_for() (not wait()) so that a cancellation here also
                # cancels the Event.wait() coroutine
                await tasks.wait_for(previous_failed.wait(), delay)
        try:
            this_index, coro_fn = next(enum_coro_fns)
        except StopIteration:
            return
        this_failed = locks.Event()
        next_ok_to_start = locks.Event()
        next_task = loop.create_task(run_one_coro(next_ok_to_start, this_failed))
        futures.future_add_to_awaited_by(next_task, parent_task)
        running_tasks.add(next_task)
        next_task.add_done_callback(task_done)
        next_ok_to_start.set()
        exceptions.append(None)
        assert len(exceptions) == this_index + 1

        try:
            result = await coro_fn()
        except (SystemExit, KeyboardInterrupt):
            raise
        except BaseException as e:
            exceptions[this_index] = e
            this_failed.set()  # start the next coroutine now
        else:
            nonlocal winner_index, winner_result
            assert winner_index is None
            winner_index = this_index
            winner_result = result
            # Cancel the others, but not ourselves: there is no await left
            # in this task to deliver a CancelledError to.
            current_task = tasks.current_task(loop)
            for t in running_tasks:
                if t is not current_task:
                    t.cancel()

    propagate_cancellation_error = None
    try:
        ok_to_start = locks.Event()
        first_task = loop.create_task(run_one_coro(ok_to_start, None))
        futures.future_add_to_awaited_by(first_task, parent_task)
        running_tasks.add(first_task)
        first_task.add_done_callback(task_done)
        ok_to_start.set()
        propagate_cancellation_error = None
        # never leave tasks running behind
        while running_tasks:
            on_completed_fut = loop.create_future()
            try:
                await on_completed_fut
            except exceptions_mod.CancelledError as ex:
                propagate_cancellation_error = ex
                for task in running_tasks:
                    task.cancel(*ex.args)
            on_completed_fut = None
        if __debug__ and unhandled_exceptions:
            # an exception escaping run_one_coro() is a bug: surface it
            raise ExceptionGroup("staggered race failed", unhandled_exceptions)
        if propagate_cancellation_error is not None:
            raise propagate_cancellation_error
        return winner_result, winner_index, exceptions
    finally:
        del exceptions, propagate_cancellation_error, unhandled_exceptions, parent_task
