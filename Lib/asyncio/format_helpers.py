"""Text rendering of callbacks and creation stacks (for reprs and logs)."""

import functools
import inspect
import reprlib
import sys
import traceback

from . import constants


def _get_function_source(func):
    """(filename, firstlineno) of the code behind *func*, or None."""
    func = inspect.unwrap(func)
    while isinstance(func, (functools.partial, functools.partialmethod)):
        func = inspect.unwrap(func.func)
    if inspect.isfunction(func):
        return func.__code__.co_filename, func.__code__.co_firstlineno
    return None


def _format_args_and_kwargs(args, kwargs, *, debug=False):
    # Arguments can hold secrets: they are only rendered in debug mode.
    if not debug:
        return '()'
    shown = [reprlib.repr(a) for a in args or ()]
    shown += [f'{name}={reprlib.repr(value)}'
              for name, value in (kwargs or {}).items()]
    return '(' + ', '.join(shown) + ')'


def _callable_name(func):
    for attr in ('__qualname__', '__name__'):
        name = getattr(func, attr, None)
        if name:
            return name
    return repr(func)


def _format_callback(func, args, kwargs, *, debug=False, suffix=''):
    # A partial is shown as its target followed by the bound arguments and
    # then the call arguments: f(bound)(called).
    calls = [_format_args_and_kwargs(args, kwargs, debug=debug)]
    while isinstance(func, functools.partial):
        calls.append(_format_args_and_kwargs(func.args, func.keywords,
                                             debug=debug))
        func = func.func
    return _callable_name(func) + ''.join(reversed(calls)) + suffix


def _format_callback_source(func, args, *, debug=False):
    text = _format_callback(func, args, None, debug=debug)
    where = _get_function_source(func)
    if where is not None:
        text += f' at {where[0]}:{where[1]}'
    return text


def extract_stack(f=None, limit=None):
    """Cheap stack summary (source lines are not read) of the caller."""
    if f is None:
        f = sys._getframe(1)
    if limit is None:
        limit = constants.DEBUG_STACK_DEPTH
    summary = traceback.StackSummary.extract(traceback.walk_stack(f),
                                             limit=limit, lookup_lines=False)
    summary.reverse()
    return summary
