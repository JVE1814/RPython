"""Iteration helpers used by importlib.metadata."""

from itertools import filterfalse


def unique_everseen(iterable, key=None):
    """Yield the items of *iterable* whose key has not been seen before,
    in their original order."""
    seen = set()
    for item in iterable:
        marker = item if key is None else key(item)
        if marker in seen:
            continue
        seen.add(marker)
        yield item


def always_iterable(obj, base_type=(str, bytes)):
    """Return an iterator over *obj*, treating None as empty and scalars
    (including strings and bytes) as a single item.

    >>> list(always_iterable(None))
    []
    >>> list(always_iterable('ab'))
    ['ab']
    >>> list(always_iterable([1, 2]))
    [1, 2]
    """
    if obj is None:
        return iter(())
    if base_type is not None and isinstance(obj, base_type):
        return iter((obj,))
    try:
        return iter(obj)
    except TypeError:
        return iter((obj,))


def partition(pred, iterable):
    """Split *iterable* into (items where pred is false, items where true)."""
    items = list(iterable)
    if pred is None:
        pred = bool
    return filterfalse(pred, items), filter(pred, items)
