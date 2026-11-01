"""Race coroutines started at staggered times ("happy eyeballs")."""

__all__ = 'staggered_race',

from . import events
from . import exceptions as exceptions_mod
from . import futures
from . import tasks


async def staggered_race(coro_fns, delay, *, loop=None):
    """Start the coroutine functions of *coro_fns* one after another and
    return the first successful result.

    The next candidate starts as soon as the previous one fails, or
    *delay* seconds after it started (delay None: only on failure).  Once
    one succeeds all others are cancelled.

    Returns (winner_result, winner_index, exceptions) where exceptions has
    one entry per started candidate (None for the winner).  winner_index is
    None when every candidate failed.
    """
    loop = loop or events.get_running_loop()
    parent = tasks.current_task(loop)
    errors = []
    running = set()
    outcome = {'index': None, 'result': None}
    wake = None          # future resolved on a failure or a win
    all_done = None      # future resolved when `running` empties

    def poke():
        if wake is not None and not wake.done():
            wake.set_result(None)

    def attempt_finished(task):
        running.discard(task)
        futures.future_discard_from_awaited_by(task, parent)
        if not running and all_done is not None and not all_done.done():
            all_done.set_result(None)

    async def attempt(index, fn):
        try:
            value = await fn()
        except (SystemExit, KeyboardInterrupt):
            raise
        except BaseException as exc:
            errors[index] = exc
            poke()
            return
        if outcome['index'] is not None:
            return
        outcome['index'], outcome['result'] = index, value
        me = tasks.current_task(loop)
        for other in list(running):
            if other is not me:
                other.cancel()
        poke()

    def cancel_everything(*args):
        for task in list(running):
            task.cancel(*args)

    cancelled_with = None
    try:
        for index, fn in enumerate(coro_fns):
            if outcome['index'] is not None:
                break
            errors.append(None)
            wake = loop.create_future()
            task = loop.create_task(attempt(index, fn))
            futures.future_add_to_awaited_by(task, parent)
            if task.done():
                attempt_finished(task)
            else:
                running.add(task)
                task.add_done_callback(attempt_finished)
            if wake.done() or not running:
                continue
            timer = None if delay is None else loop.call_later(delay, poke)
            try:
                await wake
            finally:
                if timer is not None:
                    timer.cancel()
            if delay is None and outcome['index'] is None and running:
                # failure of this candidate: start the next one
                continue
        # wait for the stragglers (cancelled losers)
        while running:
            all_done = loop.create_future()
            try:
                await all_done
            except exceptions_mod.CancelledError as exc:
                cancelled_with = exc
                cancel_everything(*exc.args)
    except exceptions_mod.CancelledError as exc:
        cancelled_with = exc
        cancel_everything(*exc.args)
        while running:
            all_done = loop.create_future()
            try:
                await all_done
            except exceptions_mod.CancelledError:
                cancel_everything(*exc.args)
    if cancelled_with is not None:
        try:
            raise cancelled_with
        finally:
            cancelled_with = None
    return outcome['result'], outcome['index'], errors
