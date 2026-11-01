"""Introspection of the await graph of tasks and futures."""

import dataclasses
import io
import sys
import types

from . import events
from . import futures
from . import tasks

__all__ = (
    'capture_call_graph',
    'format_call_graph',
    'print_call_graph',
    'FrameCallGraphEntry',
    'FutureCallGraph',
)


@dataclasses.dataclass(frozen=True, slots=True)
class FrameCallGraphEntry:
    frame: types.FrameType


@dataclasses.dataclass(frozen=True, slots=True)
class FutureCallGraph:
    future: futures.Future
    call_stack: tuple["FrameCallGraphEntry", ...]
    awaited_by: tuple["FutureCallGraph", ...]


def _coroutine_frames(coro):
    """Frames of a suspended coroutine chain, innermost first."""
    frames = []
    while coro is not None:
        frame = (getattr(coro, 'cr_frame', None) or
                 getattr(coro, 'gi_frame', None) or
                 getattr(coro, 'ag_frame', None))
        if frame is None:
            break
        frames.append(frame)
        coro = (getattr(coro, 'cr_await', None) or
                getattr(coro, 'gi_yieldfrom', None) or
                getattr(coro, 'ag_await', None))
    frames.reverse()
    return frames


def _apply_limit(frames, limit):
    if limit is None:
        return frames
    if limit >= 0:
        return frames[:limit]
    return frames[limit:]


def _awaiters(future, seen):
    found = []
    for parent in sorted(future._asyncio_awaited_by or (), key=id):
        if id(parent) in seen:
            continue
        found.append(_graph_for(parent, None, seen | {id(parent)}))
    return tuple(found)


def _graph_for(future, limit, seen):
    frames = []
    if isinstance(future, tasks.Task):
        frames = _coroutine_frames(future.get_coro())
    entries = tuple(FrameCallGraphEntry(f) for f in _apply_limit(frames, limit))
    return FutureCallGraph(future, entries, _awaiters(future, seen))


def capture_call_graph(future=None, /, *, depth=1, limit=None):
    """Return the FutureCallGraph of *future* (default: the current task).

    For the running task the live stack is captured, starting *depth*
    frames above the caller.  *limit* bounds the number of frames
    (negative: keep the outermost ones).  Returns None when there is no
    task to describe.
    """
    loop = events._get_running_loop()
    running = tasks.current_task(loop) if loop is not None else None
    if future is not None and future is not running:
        if not futures.isfuture(future):
            raise TypeError(f"{future!r} object does not appear to be "
                            f"compatible with asyncio.Future")
        return _graph_for(future, limit, {id(future)})

    if loop is None:
        raise RuntimeError("capture_call_graph() is supposed to be called "
                           "within a running event loop")
    current = tasks.current_task(loop)
    if current is None:
        return None
    root = current.get_coro()
    root_frame = (getattr(root, 'cr_frame', None) or
                  getattr(root, 'gi_frame', None))
    frames = []
    frame = sys._getframe(depth)
    while frame is not None:
        frames.append(frame)
        if frame is root_frame or getattr(frame, 'f_generator', None) is root:
            break
        frame = frame.f_back
    entries = tuple(FrameCallGraphEntry(f) for f in _apply_limit(frames, limit))
    return FutureCallGraph(current, entries,
                           _awaiters(current, {id(current)}))


def _describe_frame(frame):
    gen = getattr(frame, 'f_generator', None)
    code = frame.f_code
    name = getattr(code, 'co_qualname', code.co_name)
    if gen is None:
        kind = ''
    elif hasattr(gen, 'cr_code'):
        kind = 'async '
    elif hasattr(gen, 'ag_code'):
        kind = 'async generator '
    else:
        kind = 'generator '
    return (f"{kind}{name}() in {code.co_filename!r}, "
            f"line {frame.f_lineno}")


def format_call_graph(future=None, /, *, depth=1, limit=None):
    """Text rendering of capture_call_graph() (empty if there is none)."""
    graph = capture_call_graph(future, depth=depth + 1, limit=limit)
    if graph is None:
        return ""
    lines = []

    def render(node, indent):
        pad = '    ' * indent
        fut = node.future
        if isinstance(fut, tasks.Task):
            lines.append(f"{pad}* Task(name={fut.get_name()!r}, "
                         f"id={id(fut):#x})")
        else:
            lines.append(f"{pad}* Future(id={id(fut):#x})")
        if node.call_stack:
            lines.append(f"{pad}  + Call stack:")
            for entry in node.call_stack:
                lines.append(f"{pad}  |   {_describe_frame(entry.frame)}")
        if node.awaited_by:
            lines.append(f"{pad}  + Awaited by:")
            for parent in node.awaited_by:
                render(parent, indent + 1)

    try:
        render(graph, 0)
        return '\n'.join(lines)
    finally:
        graph = None


def print_call_graph(future=None, /, *, file=None, depth=1, limit=None):
    """Print format_call_graph() to *file* (default sys.stdout)."""
    print(format_call_graph(future, depth=depth + 1, limit=limit),
          file=file, flush=True)
