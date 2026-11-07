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

        For every thread's set of tasks, find the leaf tasks (those no other task
        awaits) and, from each, walk up the ``awaited_by`` links to the root,
        concatenating the coroutine frames.  Yields ``(frames, thread_id, leaf_task_id)``.
        """
        for info in awaited_info_list or ():
            tasks = {t.task_id: t for t in getattr(info, 'awaited_by', ()) or ()}
            awaiters = set()
            for t in tasks.values():
                for coro in getattr(t, 'awaited_by', ()) or ():
                    awaiters.add(coro.task_name)
            thread_id = getattr(info, 'thread_id', None)
            for leaf in tasks.values():
                if leaf.task_id in awaiters:
                    continue
                frames = []
                for coro in getattr(leaf, 'coroutine_stack', ()) or ():
                    frames.extend(coro.call_stack)
                cur = leaf
                seen = {cur.task_id}
                while True:
                    parents = getattr(cur, 'awaited_by', ()) or ()
                    if not parents:
                        break
                    coro = parents[0]
                    frames.extend(coro.call_stack)
                    parent = tasks.get(coro.task_name)
                    if parent is None or parent.task_id in seen:
                        break
                    seen.add(parent.task_id)
                    cur = parent
                yield frames, thread_id, leaf.task_id

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
