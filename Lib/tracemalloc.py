"""Trace memory allocations made by Python code.

Call ``start()`` (or run with ``-X tracemalloc`` / ``PYTHONTRACEMALLOC``),
then ``take_snapshot()`` to obtain a Snapshot of the currently allocated
blocks, each with the traceback where it was allocated.  Snapshots can be
filtered, grouped into statistics and compared with each other.
"""

import fnmatch
import linecache
import os.path
import pickle
from collections.abc import Iterable, Sequence
from functools import total_ordering

from _tracemalloc import *
from _tracemalloc import _get_object_traceback, _get_traces


_SIZE_UNITS = ('B', 'KiB', 'MiB', 'GiB', 'TiB')


def _format_size(size, sign):
    """Human readable size: at most four significant digits and a unit;
    with *sign*, positive values get a '+'."""
    value = size
    for position, unit in enumerate(_SIZE_UNITS):
        last = position == len(_SIZE_UNITS) - 1
        if unit != 'B' and abs(value) < 100:
            spec = '+.1f' if sign else '.1f'
            return f'{value:{spec}} {unit}'
        if abs(value) < 10 * 1024 or last:
            spec = '+.0f' if sign else '.0f'
            return f'{value:{spec}} {unit}'
        value /= 1024


class Statistic:
    """Total size and number of the memory blocks allocated at one
    traceback."""

    __slots__ = ('traceback', 'size', 'count')

    def __init__(self, traceback, size, count):
        self.traceback = traceback
        self.size = size
        self.count = count

    def _key(self):
        return (self.traceback, self.size, self.count)

    def __hash__(self):
        return hash(self._key())

    def __eq__(self, other):
        if not isinstance(other, Statistic):
            return NotImplemented
        return self._key() == other._key()

    def __str__(self):
        text = (f"{self.traceback}: size={_format_size(self.size, False)}, "
                f"count={self.count}")
        if self.count:
            text += f", average={_format_size(self.size / self.count, False)}"
        return text

    def __repr__(self):
        return (f'<Statistic traceback={self.traceback!r} '
                f'size={self.size} count={self.count}>')

    def _sort_key(self):
        return (self.size, self.count, self.traceback)


class StatisticDiff:
    """Difference of the size and count of the memory blocks allocated at
    one traceback between an old and a new snapshot."""

    __slots__ = ('traceback', 'size', 'size_diff', 'count', 'count_diff')

    def __init__(self, traceback, size, size_diff, count, count_diff):
        self.traceback = traceback
        self.size = size
        self.size_diff = size_diff
        self.count = count
        self.count_diff = count_diff

    def _key(self):
        return (self.traceback, self.size, self.size_diff,
                self.count, self.count_diff)

    def __hash__(self):
        return hash(self._key())

    def __eq__(self, other):
        if not isinstance(other, StatisticDiff):
            return NotImplemented
        return self._key() == other._key()

    def __str__(self):
        text = (f"{self.traceback}: "
                f"size={_format_size(self.size, False)} "
                f"({_format_size(self.size_diff, True)}), "
                f"count={self.count} ({self.count_diff:+d})")
        if self.count:
            text += f", average={_format_size(self.size / self.count, False)}"
        return text

    def __repr__(self):
        return (f'<StatisticDiff traceback={self.traceback!r} '
                f'size={self.size} ({self.size_diff:+d}) '
                f'count={self.count} ({self.count_diff:+d})>')

    def _sort_key(self):
        return (abs(self.size_diff), self.size,
                abs(self.count_diff), self.count,
                self.traceback)


def _compare_grouped_stats(old_group, new_group):
    diffs = []
    for traceback, stat in new_group.items():
        previous = old_group.get(traceback)
        if previous is None:
            diffs.append(StatisticDiff(traceback, stat.size, stat.size,
                                       stat.count, stat.count))
        else:
            diffs.append(StatisticDiff(traceback, stat.size,
                                       stat.size - previous.size,
                                       stat.count,
                                       stat.count - previous.count))
    for traceback, stat in old_group.items():
        if traceback not in new_group:
            diffs.append(StatisticDiff(traceback, 0, -stat.size,
                                       0, -stat.count))
    return diffs


@total_ordering
class Frame:
    """One frame of a traceback: a file name and a line number."""

    __slots__ = ('_frame',)

    def __init__(self, frame):
        # frame is a (filename: str, lineno: int) tuple
        self._frame = frame

    @property
    def filename(self):
        return self._frame[0]

    @property
    def lineno(self):
        return self._frame[1]

    def __eq__(self, other):
        if not isinstance(other, Frame):
            return NotImplemented
        return self._frame == other._frame

    def __lt__(self, other):
        if not isinstance(other, Frame):
            return NotImplemented
        return self._frame < other._frame

    def __hash__(self):
        return hash(self._frame)

    def __str__(self):
        return f"{self.filename}:{self.lineno}"

    def __repr__(self):
        return f"<Frame filename={self.filename!r} lineno={self.lineno!r}>"


_Frame = Frame


@total_ordering
class Traceback(Sequence):
    """A sequence of Frame objects, oldest call first.

    *frames* is given most recent first (as returned by _tracemalloc);
    *total_nframe* is the depth of the stack before it was truncated to
    the traceback limit.
    """

    __slots__ = ('_frames', '_total_nframe')

    def __init__(self, frames, total_nframe=None):
        Sequence.__init__(self)
        self._frames = tuple(reversed(frames))
        self._total_nframe = total_nframe

    @property
    def total_nframe(self):
        return self._total_nframe

    def __len__(self):
        return len(self._frames)

    def __getitem__(self, index):
        if isinstance(index, slice):
            return tuple(Frame(raw) for raw in self._frames[index])
        return Frame(self._frames[index])

    def __contains__(self, frame):
        return frame._frame in self._frames

    def __hash__(self):
        return hash(self._frames)

    def __eq__(self, other):
        if not isinstance(other, Traceback):
            return NotImplemented
        return self._frames == other._frames

    def __lt__(self, other):
        if not isinstance(other, Traceback):
            return NotImplemented
        return self._frames < other._frames

    def __str__(self):
        return str(self[0])

    def __repr__(self):
        text = f"<Traceback {tuple(self)}"
        if self._total_nframe is not None:
            text += f" total_nframe={self.total_nframe}"
        return text + ">"

    def format(self, limit=None, most_recent_first=False):
        """Return a list of lines like those of the traceback module.

        A positive *limit* keeps the most recent frames, a negative one the
        oldest; *most_recent_first* reverses the order.
        """
        frames = list(self)
        if limit is not None:
            frames = frames[-limit:] if limit > 0 else frames[:-limit]
        if most_recent_first:
            frames.reverse()
        lines = []
        for frame in frames:
            lines.append(f'  File "{frame.filename}", line {frame.lineno}')
            source = linecache.getline(frame.filename, frame.lineno).strip()
            if source:
                lines.append(f'    {source}')
        return lines


def get_object_traceback(obj):
    """Return the Traceback where *obj* was allocated, or None if it was
    not traced."""
    frames = _get_object_traceback(obj)
    if frames is None:
        return None
    return Traceback(frames)


class Trace:
    """One traced memory block: its domain, size and Traceback."""

    __slots__ = ('_trace',)

    def __init__(self, trace):
        # trace is a (domain, size, frames, total_nframe) tuple
        self._trace = trace

    @property
    def domain(self):
        return self._trace[0]

    @property
    def size(self):
        return self._trace[1]

    @property
    def traceback(self):
        return Traceback(*self._trace[2:])

    def __eq__(self, other):
        if not isinstance(other, Trace):
            return NotImplemented
        return self._trace == other._trace

    def __hash__(self):
        return hash(self._trace)

    def __str__(self):
        return f"{self.traceback}: {_format_size(self.size, False)}"

    def __repr__(self):
        return (f"<Trace domain={self.domain} "
                f"size={_format_size(self.size, False)}, "
                f"traceback={self.traceback!r}>")


class _Traces(Sequence):
    def __init__(self, traces):
        Sequence.__init__(self)
        # list of raw (domain, size, frames, total_nframe) tuples
        self._traces = traces

    def __len__(self):
        return len(self._traces)

    def __getitem__(self, index):
        if isinstance(index, slice):
            return tuple(Trace(raw) for raw in self._traces[index])
        return Trace(self._traces[index])

    def __contains__(self, trace):
        return trace._trace in self._traces

    def __eq__(self, other):
        if not isinstance(other, _Traces):
            return NotImplemented
        return self._traces == other._traces

    def __repr__(self):
        return f"<Traces len={len(self)}>"


def _normalize_filename(filename):
    filename = os.path.normcase(filename)
    if filename.endswith('.pyc'):
        filename = filename[:-1]
    return filename


class BaseFilter:
    def __init__(self, inclusive):
        self.inclusive = inclusive

    def _match(self, trace):
        raise NotImplementedError


class Filter(BaseFilter):
    """Keep (inclusive=True) or drop (inclusive=False) the traces whose
    traceback has a frame matching *filename_pattern* (fnmatch syntax) and
    *lineno* (None: any line).  Only the most recent frame is considered
    unless *all_frames* is true.  *domain* (None: any) restricts the filter
    to one address space."""

    def __init__(self, inclusive, filename_pattern,
                 lineno=None, all_frames=False, domain=None):
        super().__init__(inclusive)
        self.inclusive = inclusive
        self._filename_pattern = _normalize_filename(filename_pattern)
        self.lineno = lineno
        self.all_frames = all_frames
        self.domain = domain

    @property
    def filename_pattern(self):
        return self._filename_pattern

    def _frame_matches(self, filename, lineno):
        if not fnmatch.fnmatch(_normalize_filename(filename), self._filename_pattern):
            return False
        return self.lineno is None or lineno == self.lineno

    def _match_frame(self, filename, lineno):
        return self._frame_matches(filename, lineno) == self.inclusive

    def _match_traceback(self, traceback):
        if self.all_frames:
            found = any(self._frame_matches(filename, lineno)
                        for filename, lineno in traceback)
            return found == self.inclusive
        filename, lineno = traceback[0]
        return self._match_frame(filename, lineno)

    def _match(self, trace):
        domain, size, traceback, total_nframe = trace
        result = self._match_traceback(traceback)
        if self.domain is None:
            return result
        if self.inclusive:
            return result and domain == self.domain
        return result or domain != self.domain


class DomainFilter(BaseFilter):
    """Keep (inclusive=True) or drop the traces of one address space."""

    def __init__(self, inclusive, domain):
        super().__init__(inclusive)
        self._domain = domain

    @property
    def domain(self):
        return self._domain

    def _match(self, trace):
        domain, size, traceback, total_nframe = trace
        return (domain == self.domain) == self.inclusive


class Snapshot:
    """The traces of the memory blocks allocated at one point in time."""

    def __init__(self, traces, traceback_limit):
        # traces: list of raw (domain, size, frames, total_nframe) tuples;
        # the list is not copied.
        self.traces = _Traces(traces)
        self.traceback_limit = traceback_limit

    def dump(self, filename):
        """Write the snapshot to *filename* (pickle format)."""
        with open(filename, "wb") as fp:
            pickle.dump(self, fp, pickle.HIGHEST_PROTOCOL)

    @staticmethod
    def load(filename):
        """Load a snapshot written by dump().

        The file is a pickle (the documented format): only load trusted
        files.
        """
        with open(filename, "rb") as fp:
            return pickle.load(fp)

    def filter_traces(self, filters):
        """Return a new Snapshot with only the traces accepted by
        *filters* (a sequence of Filter/DomainFilter objects): a trace is
        kept if it matches at least one inclusive filter (when there are
        any) and no exclusive filter excludes it."""
        if not isinstance(filters, Iterable):
            raise TypeError(f"filters must be a list of filters, "
                            f"not {type(filters).__name__}")
        if not filters:
            return Snapshot(self.traces._traces.copy(), self.traceback_limit)
        include = [f for f in filters if f.inclusive]
        exclude = [f for f in filters if not f.inclusive]
        kept = []
        for trace in self.traces._traces:
            if include and not any(f._match(trace) for f in include):
                continue
            if not all(f._match(trace) for f in exclude):
                continue
            kept.append(trace)
        return Snapshot(kept, self.traceback_limit)

    def _group_by(self, key_type, cumulative):
        if key_type not in ('traceback', 'filename', 'lineno'):
            raise ValueError(f"unknown key_type: {key_type!r}")
        if cumulative and key_type not in ('lineno', 'filename'):
            raise ValueError(f"cumulative mode cannot be used "
                             f"with key type {key_type!r}")

        stats = {}
        tracebacks = {}

        def key_traceback(raw_frames):
            tb = tracebacks.get(raw_frames)
            if tb is None:
                tb = tracebacks[raw_frames] = Traceback(raw_frames)
            return tb

        def account(traceback, size):
            stat = stats.get(traceback)
            if stat is None:
                stats[traceback] = Statistic(traceback, size, 1)
            else:
                stat.size += size
                stat.count += 1

        for domain, size, frames, total_nframe in self.traces._traces:
            if not cumulative:
                if key_type == 'traceback':
                    key = frames
                elif key_type == 'lineno':
                    key = frames[:1]
                else:
                    key = ((frames[0][0], 0),)
                account(key_traceback(key), size)
                continue
            seen = set()
            for filename, lineno in frames:
                frame = (filename, lineno if key_type == 'lineno' else 0)
                if frame in seen:
                    continue
                seen.add(frame)
                account(key_traceback((frame,)), size)
        return stats

    def statistics(self, key_type, cumulative=False):
        """Return a list of Statistic grouped by *key_type* ('filename',
        'lineno' or 'traceback'), biggest first.  With *cumulative*, every
        frame of a traceback (not only the most recent) is accounted."""
        grouped = self._group_by(key_type, cumulative)
        return sorted(grouped.values(), key=Statistic._sort_key, reverse=True)

    def compare_to(self, old_snapshot, key_type, cumulative=False):
        """Return a list of StatisticDiff between *old_snapshot* and this
        one, grouped by *key_type*, biggest difference first."""
        new_group = self._group_by(key_type, cumulative)
        old_group = old_snapshot._group_by(key_type, cumulative)
        diffs = _compare_grouped_stats(old_group, new_group)
        return sorted(diffs, key=StatisticDiff._sort_key, reverse=True)


def take_snapshot():
    """Return a Snapshot of the traces of the memory blocks currently
    allocated by Python."""
    if not is_tracing():
        raise RuntimeError("the tracemalloc module must be tracing memory "
                           "allocations to take a snapshot")
    traces = _get_traces()
    traceback_limit = get_traceback_limit()
    return Snapshot(traces, traceback_limit)
