"""High-level helper to run blocking code in a worker thread."""

import functools
import contextvars

from . import events


__all__ = "to_thread",


async def to_thread(func, /, *args, **kwargs):
    """Run *func* in the loop's default executor and await its result.

    The current context is propagated to the worker thread, so context
    variables of the calling task are visible in *func*.
    """
    loop = events.get_running_loop()
    ctx = contextvars.copy_context()
    func_call = functools.partial(ctx.run, func, *args, **kwargs)
    return await loop.run_in_executor(None, func_call)
