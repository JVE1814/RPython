"""repr, get_stack() and print_stack() support for Task implementations."""

import linecache
import reprlib
import traceback

from . import base_futures
from . import coroutines


def _task_repr_info(task):
    parts = base_futures._future_repr_info(task)
    if task.cancelling() and not task.done():
        parts[0] = 'cancelling'
    extra = [f'name={task.get_name()!r}']
    if task._coro:
        extra.append(f'coro=<{coroutines._format_coroutine(task._coro)}>')
    if task._fut_waiter is not None:
        extra.append(f'wait_for={task._fut_waiter!r}')
    return parts[:1] + extra + parts[1:]


@reprlib.recursive_repr()
def _task_repr(task):
    return '<{} {}>'.format(type(task).__name__,
                            ' '.join(_task_repr_info(task)))


def _task_get_stack(task, limit):
    coro = task._coro
    frame = None
    for attr in ('cr_frame', 'gi_frame', 'ag_frame'):
        if hasattr(coro, attr):
            frame = getattr(coro, attr)
            break
    frames = []
    if frame is not None:
        # innermost first while walking, returned outermost first
        while frame is not None and (limit is None or len(frames) < limit):
            frames.append(frame)
            frame = frame.f_back
        frames.reverse()
        return frames
    if task._exception is not None:
        tb = task._exception.__traceback__
        while tb is not None and (limit is None or len(frames) < limit):
            frames.append(tb.tb_frame)
            tb = tb.tb_next
    return frames


def _task_print_stack(task, limit, file):
    entries = []
    refreshed = set()
    for frame in task.get_stack(limit=limit):
        code = frame.f_code
        if code.co_filename not in refreshed:
            refreshed.add(code.co_filename)
            linecache.checkcache(code.co_filename)
        source = linecache.getline(code.co_filename, frame.f_lineno,
                                   frame.f_globals)
        entries.append((code.co_filename, frame.f_lineno, code.co_name, source))

    exc = task._exception
    if not entries:
        print(f'No stack for {task!r}', file=file)
    else:
        kind = 'Traceback' if exc is not None else 'Stack'
        print(f'{kind} for {task!r} (most recent call last):', file=file)
    traceback.print_list(entries, file=file)
    if exc is not None:
        for line in traceback.format_exception_only(type(exc), exc):
            print(line, file=file, end='')
