"""Running blocking callables in a thread from coroutines."""

import contextvars
import functools

from . import events

__all__ = ("to_thread",)


async def to_thread(func, /, *args, **kwargs):
    """Call func(*args, **kwargs) in the default executor and return its
    result.  The caller's context variables are visible to *func*."""
    loop = events.get_running_loop()
    snapshot = contextvars.copy_context()
    job = functools.partial(snapshot.run, func, *args, **kwargs)
    return await loop.run_in_executor(None, job)
