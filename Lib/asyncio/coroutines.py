"""Coroutine detection and formatting helpers."""

__all__ = 'iscoroutinefunction', 'iscoroutine'

import collections.abc
import inspect
import os
import sys
import types


def _is_debug_mode():
    """True under -X dev or when PYTHONASYNCIODEBUG is set (without -E)."""
    if sys.flags.dev_mode:
        return True
    if sys.flags.ignore_environment:
        return False
    return bool(os.environ.get('PYTHONASYNCIODEBUG'))


# Marker attribute value used by mock.AsyncMock (and the historical
# @asyncio.coroutine decorator) to flag coroutine functions.
_is_coroutine = object()


def _iscoroutinefunction(func):
    if inspect.iscoroutinefunction(func):
        return True
    return getattr(func, '_is_coroutine', None) is _is_coroutine


def iscoroutinefunction(func):
    """Deprecated: use inspect.iscoroutinefunction()."""
    import warnings
    warnings._deprecated("asyncio.iscoroutinefunction",
                         f"{warnings._DEPRECATED_MSG}; use "
                         "inspect.iscoroutinefunction() instead",
                         remove=(3, 16))
    return _iscoroutinefunction(func)


# Types already known to be coroutines (bounded to avoid unbounded growth).
_known_coroutine_types = {types.CoroutineType: True}


def iscoroutine(obj):
    """Return True if *obj* is a coroutine object."""
    kind = type(obj)
    if kind in _known_coroutine_types:
        return True
    if isinstance(obj, collections.abc.Coroutine):
        if len(_known_coroutine_types) < 100:
            _known_coroutine_types[kind] = True
        return True
    return False


def _format_coroutine(coro):
    assert iscoroutine(coro)

    name = None
    for attr in ('__qualname__', '__name__'):
        candidate = getattr(coro, attr, None)
        if candidate:
            name = candidate
            break
    if name is None:
        name = f'<{type(coro).__name__} without __name__>'
    label = f'{name}()'

    code = getattr(coro, 'cr_code', None) or getattr(coro, 'gi_code', None)
    if not code:
        running = getattr(coro, 'cr_running',
                          getattr(coro, 'gi_running', False))
        return f'{label} running' if running else label

    frame = getattr(coro, 'gi_frame', None) or getattr(coro, 'cr_frame', None)
    filename = code.co_filename or '<empty co_filename>'
    if frame is not None:
        return f'{label} running at {filename}:{frame.f_lineno}'
    return f'{label} done, defined at {filename}:{code.co_firstlineno}'
