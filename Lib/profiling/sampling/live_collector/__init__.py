"""Live (curses) top-style view of a sampling profile.

The actual terminal rendering needs a real display; tests drive it with
``MockDisplay``.  ``LiveStatsCollector`` keeps per-function running counts and
renders rows through whichever display it is given.
"""

import collections

from ..collector import Collector, extract_lineno, filter_internal_frames
from ..pstats_collector import PstatsCollector
from .trend_tracker import TrendTracker

__all__ = ('LiveStatsCollector', 'MockDisplay')


class MockDisplay:
    """A display that records what would have been drawn (for tests / headless)."""

    def __init__(self, width=80, height=24):
        self.width = width
        self.height = height
        self.lines = []
        self.closed = False
        self.key_queue = []

    def clear(self):
        self.lines = []

    def addstr(self, y, x, text, *args):
        self.lines.append((y, x, text))

    def refresh(self):
        pass

    def getch(self):
        if self.key_queue:
            return self.key_queue.pop(0)
        return -1

    def get_size(self):
        return self.height, self.width

    def close(self):
        self.closed = True


class LiveStatsCollector(Collector):
    def __init__(self, sample_interval_usec=1000, *, skip_idle=False, display=None,
                 mode=None, sort_by=1, opcodes=False, **kwargs):
        super().__init__(sample_interval_usec, skip_idle=skip_idle)
        self.display = display if display is not None else MockDisplay()
        self.mode = mode
        self.sort_by = sort_by
        self._pstats = PstatsCollector(sample_interval_usec, skip_idle=skip_idle)
        self.trend = TrendTracker()
        self.total_samples = 0

    def collect(self, stack_frames, timestamps_us=None):
        self.total_samples += 1
        self._pstats.collect(stack_frames, timestamps_us)

    def _rows(self):
        rows = []
        for key, entry in self._pstats.result.items():
            rows.append((key, entry["direct_calls"], entry["cumulative_calls"]))
        rows.sort(key=lambda r: r[2], reverse=True)
        return rows

    def render(self):
        self.display.clear()
        for i, (key, direct, cumulative) in enumerate(self._rows()):
            filename, lineno, funcname = key
            self.display.addstr(i, 0, f"{funcname} {filename}:{lineno} {direct}/{cumulative}")
        self.display.refresh()

    def export(self, filename):
        self._pstats.export(filename)

    def close(self):
        self.display.close()
