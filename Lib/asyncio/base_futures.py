"""State constants and repr support shared by Future implementations."""

__all__ = ()

import reprlib

from . import format_helpers

_PENDING = 'PENDING'
_CANCELLED = 'CANCELLED'
_FINISHED = 'FINISHED'


def isfuture(obj):
    """True for asyncio futures and Future-compatible objects (classes
    defining _asyncio_future_blocking)."""
    if not hasattr(obj.__class__, "_asyncio_future_blocking"):
        return False
    return obj._asyncio_future_blocking is not None


def _format_callbacks(cb):
    # first, last and a count of the ones in between
    names = [format_helpers._format_callback_source(entry[0], ())
             for entry in (cb[:1] + cb[-1:] if len(cb) > 2 else cb)]
    if len(cb) > 2:
        names.insert(1, f'<{len(cb) - 2} more>')
    return 'cb=[' + ', '.join(names) + ']'


def _future_repr_info(future):
    parts = [future._state.lower()]
    if future._state == _FINISHED:
        if future._exception is not None:
            parts.append(f'exception={future._exception!r}')
        else:
            parts.append(f'result={reprlib.repr(future._result)}')
    if future._callbacks:
        parts.append(_format_callbacks(future._callbacks))
    if future._source_traceback:
        origin = future._source_traceback[-1]
        parts.append(f'created at {origin[0]}:{origin[1]}')
    return parts


@reprlib.recursive_repr()
def _future_repr(future):
    return '<{} {}>'.format(type(future).__name__,
                            ' '.join(_future_repr_info(future)))
