"""Decorators used by importlib.metadata."""

import functools
import types


def method_cache(method, cache_wrapper=functools.lru_cache()):
    """Cache *method*'s results per instance.

    Unlike decorating the method with lru_cache directly, the cache lives on
    the instance (created at first call), so it does not keep instances
    alive and is not shared between them.  ``cache_clear`` is available on
    the bound attribute after the first call.
    """

    def first_call(self, *args, **kwargs):
        bound = types.MethodType(method, self)
        cached = cache_wrapper(bound)
        setattr(self, method.__name__, cached)
        return cached(*args, **kwargs)

    first_call.cache_clear = lambda: None
    return first_call


def pass_none(func):
    """Return None without calling *func* when its first argument is None.

    >>> print(pass_none(str.upper)(None))
    None
    >>> pass_none(str.upper)('x')
    'X'
    """

    @functools.wraps(func)
    def wrapper(param, *args, **kwargs):
        if param is None:
            return None
        return func(param, *args, **kwargs)

    return wrapper
