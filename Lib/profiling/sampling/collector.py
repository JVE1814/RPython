"""Base class of the sampling profiler's collectors, and helpers.

A sample is a sequence of interpreter records, each holding thread
records; a thread record has ``thread_id``, ``status`` (THREAD_STATUS_*
flags) and ``frame_info``: the frames of its stack, innermost first.  A
frame is a 4-sequence (filename, location, funcname, opcode) where the
location is (lineno, end_lineno, col_offset, end_col_offset) or None for
frames without Python source.
"""

from abc import ABC, abstractmethod

from .constants import (
    DEFAULT_LOCATION,
    THREAD_STATUS_HAS_GIL,
    THREAD_STATUS_ON_CPU,
    THREAD_STATUS_UNKNOWN,
)


def normalize_location(location):
    """The 4-tuple form of a frame location (None and bare line numbers
    are accepted)."""
    if location is None:
        return DEFAULT_LOCATION
    if isinstance(location, int):
        return (location, location, -1, -1)
    return location


_INTERNAL_FILES = ('_sync_coordinator.py',)


def filter_internal_frames(frames):
    """Drop the profiler's own coordination frames from a stack, wherever they
    appear (the synchronising child launched to attach to a started script)."""
    out = []
    for frame in frames or ():
        filename = frame[0] if not hasattr(frame, 'filename') else frame.filename
        if filename and any(filename.endswith(name) for name in _INTERNAL_FILES):
            continue
        out.append(frame)
    return out


def extract_lineno(location):
    """The (first) line number of a frame location; 0 if unknown."""
    if location is None:
        return 0
    if isinstance(location, int):
        return location
    return location[0]


try:
    from _remote_debugging import FrameInfo as _FrameInfo
except ImportError:
    from collections import namedtuple as _namedtuple
    _FrameInfo = _namedtuple('FrameInfo', 'filename location funcname opcode')
    _FrameInfo = (lambda make: (lambda values: make(*values)))(_FrameInfo)


def _task_marker(name):
    """The synthetic frame separating the frames of one asyncio task from the
    task awaiting it."""
    return _FrameInfo(("<task>", None, name, None))


def _task_order(task_id):
    # awaiting tasks are referred to by id; order ids before anything else
    return (0, task_id, "") if isinstance(task_id, int) else (1, 0, str(task_id))


def is_idle(thread):
    """Whether a sampled thread was neither running nor holding the GIL."""
    status = getattr(thread, 'status', 0) or 0
    if status & THREAD_STATUS_UNKNOWN:
        return False
    return not status & (THREAD_STATUS_ON_CPU | THREAD_STATUS_HAS_GIL)


class Collector(ABC):
    """Receives samples one by one and produces a report at the end."""

    def __init__(self, sample_interval_usec=1000, *, skip_idle=False):
        self.sample_interval_usec = sample_interval_usec
        self.skip_idle = skip_idle

    @abstractmethod
    def collect(self, stack_frames, timestamps_us=None):
        """Record one sample (see the module docstring)."""

    @abstractmethod
    def export(self, filename):
        """Write the report."""

    def collect_failed_sample(self):
        """Called when a sample could not be taken."""

    def _iter_threads(self, stack_frames):
        for interpreter in stack_frames or ():
            for thread in getattr(interpreter, 'threads', ()) or ():
                if self.skip_idle and is_idle(thread):
                    continue
                yield thread

    def _iter_async_frames(self, awaited_info_list):
        """Reconstruct linear call stacks from an asyncio task graph.

        For every thread's tasks, the leaves are the tasks no other task waits
        for.  From each leaf the stack is built innermost first: the task's
        coroutine frames, then a ``<task>`` marker frame naming it, then the
        same for the task awaiting it, up to a task nobody awaits.  A task
        awaited by several tasks follows the one with the lowest id (its marker
        says how many there are); a missing or already visited awaiter ends the
        walk.  Yields ``(frames, thread_id, leaf_task_id)``.
        """
        for info in awaited_info_list or ():
            tasks = {}
            for task in getattr(info, 'awaited_by', ()) or ():
                tasks[task.task_id] = task
            awaited = set()
            for task in tasks.values():
                for waiter in getattr(task, 'awaited_by', ()) or ():
                    awaited.add(waiter.task_name)
            thread_id = getattr(info, 'thread_id', None)
            for leaf_id, leaf in tasks.items():
                if leaf_id in awaited:
                    continue
                frames = []
                visited = set()
                task = leaf
                while task is not None and task.task_id not in visited:
                    visited.add(task.task_id)
                    for coro in getattr(task, 'coroutine_stack', ()) or ():
                        frames.extend(coro.call_stack or ())
                    waiters = sorted(
                        (w.task_name for w in getattr(task, 'awaited_by', ()) or ()),
                        key=_task_order)
                    name = str(task.task_name)
                    if len(waiters) > 1:
                        name = f"{name} ({len(waiters)} parents)"
                    frames.append(_task_marker(name))
                    task = tasks.get(waiters[0]) if waiters else None
                yield frames, thread_id, leaf_id

    def print_stats(self, *, show_summary=True, sort=None, limit=None, mode=None, **kwargs):
        """Default textual summary (overridden by collectors with richer output)."""
        total = getattr(self, 'total_samples', None)
        if show_summary and total is not None:
            print(f"Captured {total} samples")

    def process_frames(self, frames, thread_id=None, weight=1):
        """Feed one thread's frame list straight into the collector (used by tests and
        replay paths that already have per-thread frames)."""
        thread = type('T', (), {
            'thread_id': thread_id, 'status': 0, 'frame_info': frames,
        })()
        interp = type('I', (), {'interpreter_id': 0, 'threads': [thread]})()
        for _ in range(max(int(weight), 1)):
            self.collect([interp], [0.0])

    def _iter_all_frames(self, stack_frames, skip_idle=False):
        """(frames, thread id) for every thread of the sample."""
        saved = self.skip_idle
        self.skip_idle = skip_idle or saved
        try:
            for thread in self._iter_threads(stack_frames):
                frames = getattr(thread, 'frame_info', None)
                if frames:
                    yield frames, getattr(thread, 'thread_id', None)
        finally:
            self.skip_idle = saved
