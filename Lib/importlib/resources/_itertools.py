"""Small iteration helpers used by importlib.resources."""


def only(iterable, default=None, too_long=None):
    """Return the single item of *iterable*, *default* if it is empty.

    Raise *too_long* (default: ValueError) if it holds more than one item.
    """
    iterator = iter(iterable)
    first = next(iterator, default)
    for extra in iterator:
        if too_long is not None:
            raise too_long
        raise ValueError(
            f'Expected exactly one item in iterable, but got {first!r}, '
            f'{extra!r}, and perhaps more.'
        )
    return first
