"""A race between coroutines that start one after another ("happy
eyeballs", RFC 8305), used by loop.create_connection()."""

__all__ = 'staggered_race',

from . import events
from . import exceptions as exceptions_mod
from . import futures
from . import tasks


async def staggered_race(coro_fns, delay, *, loop=None):
    """Race the awaitables produced by the zero-argument callables in
    *coro_fns*, starting them one at a time.

    The first callable is started at once.  The next one is started as
    soon as the most recent attempt fails, or, when *delay* is not None,
    once *delay* seconds have passed since the most recent start.  The
    first attempt that returns wins: every other running attempt is
    cancelled and awaited.

    Returns ``(result, index, exceptions)``: the winner's result and its
    position in *coro_fns* (both None when nothing succeeded), and one
    entry per started attempt holding the exception it ended with (None
    for the winner, CancelledError for the attempts cancelled because of
    it).

    Cancelling the race cancels the running attempts; the race waits for
    them to end and then raises CancelledError.
    """
    loop = loop or events.get_running_loop()
    parent = tasks.current_task(loop)
    remaining = iter(coro_fns)
    outcomes = []          # one slot per started attempt
    running = set()
    winner = None          # (index, result) of the first success
    launch_now = False     # an attempt failed or the delay elapsed
    wake = None            # what the race itself is waiting on
    timer = None

    def nudge():
        if wake is not None and not wake.done():
            wake.set_result(None)

    def time_is_up():
        nonlocal launch_now
        launch_now = True
        nudge()

    def attempt_done(task):
        running.discard(task)
        if parent is not None:
            futures.future_discard_from_awaited_by(task, parent)
        nudge()

    async def run_one_coro(index, fn):
        nonlocal winner, launch_now
        try:
            result = await fn()
        except (SystemExit, KeyboardInterrupt):
            raise
        except BaseException as exc:
            outcomes[index] = exc
            launch_now = True
            nudge()
            return
        if winner is None:
            winner = (index, result)
            me = tasks.current_task(loop)
            for other in list(running):
                if other is not me:
                    other.cancel()
        nudge()

    def launch():
        """Start the next attempt; False when there is none left."""
        nonlocal timer
        fn = next(remaining, None)
        if fn is None:
            return False
        index = len(outcomes)
        outcomes.append(None)
        if timer is not None:
            timer.cancel()
            timer = None
        task = loop.create_task(run_one_coro(index, fn))
        running.add(task)
        if parent is not None:
            futures.future_add_to_awaited_by(task, parent)
        task.add_done_callback(attempt_done)
        if delay is not None:
            timer = loop.call_later(delay, time_is_up)
        return True

    cancelled = False
    more = True
    try:
        launch_now = True
        while True:
            if launch_now and more and winner is None and not cancelled:
                launch_now = False   # an eager attempt may set it again
                more = launch()
                continue
            if not running:
                if more and winner is None and not cancelled:
                    launch_now = True
                    continue
                break
            wake = loop.create_future()
            try:
                await wake
            except exceptions_mod.CancelledError:
                # Stop launching, cancel what runs, and keep waiting for
                # it (any further cancellation is passed on as well).
                cancelled = True
                for task in list(running):
                    task.cancel()
            finally:
                wake = None
    finally:
        if timer is not None:
            timer.cancel()

    if cancelled:
        raise exceptions_mod.CancelledError()
    if winner is None:
        return None, None, outcomes
    return winner[1], winner[0], outcomes
