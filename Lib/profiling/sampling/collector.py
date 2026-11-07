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
