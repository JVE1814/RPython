"""Collector that turns samples into a ``pstats``-compatible statistics table.

Each sample contributes one *direct* hit to the function on top of every
thread's stack and one *cumulative* hit to every distinct function on the
stack.  ``create_stats`` turns the hit counts into the five-tuple pstats uses
(``(cc, nc, tt, ct, callers)``) by multiplying by the sample interval, and
``export`` marshals the table with a ``("__sampled__",)`` marker so
``pstats.Stats`` knows the times are sampled estimates.
"""

import marshal

from .collector import Collector, extract_lineno, filter_internal_frames

__all__ = ('PstatsCollector',)


class PstatsCollector(Collector):
    def __init__(self, sample_interval_usec=1000, *, skip_idle=False, opcodes=False, **kwargs):
        super().__init__(sample_interval_usec, skip_idle=skip_idle)
        self.sample_interval_sec = sample_interval_usec / 1_000_000
        # key -> {"direct_calls", "cumulative_calls", "callers": {caller: count}}
        self.result = {}
        self.stats = {}
        self.total_samples = 0

    def collect(self, stack_frames, timestamps_us=None):
        self.total_samples += 1
        for frames, _thread_id in self._iter_all_frames(stack_frames):
            frames = filter_internal_frames(frames)
            if not frames:
                continue
            seen = set()
            for depth, frame in enumerate(frames):
                key = _key(frame)
                entry = self.result.get(key)
                if entry is None:
                    entry = self.result[key] = {
                        "direct_calls": 0,
                        "cumulative_calls": 0,
                        "callers": {},
                    }
                if depth == 0:
                    entry["direct_calls"] += 1
                if key not in seen:
                    entry["cumulative_calls"] += 1
                    seen.add(key)
                if depth + 1 < len(frames):
                    caller = _key(frames[depth + 1])
                    entry["callers"][caller] = entry["callers"].get(caller, 0) + 1

    def print_stats(self, *, show_summary=True, sort=None, limit=None, mode=None, **kwargs):
        if show_summary:
            print(f"Captured {self.total_samples} samples")
        self.create_stats()
        rows = sorted(
            self.stats.items(), key=lambda kv: kv[1][1], reverse=True
        )
        if limit:
            rows = rows[:limit]
        for (filename, lineno, funcname), (direct, cumulative, tt, ct, _c) in rows:
            print(f"{cumulative:8d} {tt:10.4f} {ct:10.4f}  {funcname} ({filename}:{lineno})")

    def create_stats(self):
        self.stats = {}
        interval = self.sample_interval_sec
        for key, entry in self.result.items():
            direct = entry["direct_calls"]
            cumulative = entry["cumulative_calls"]
            tt = direct * interval
            ct = cumulative * interval
            callers = {}
            for caller, count in entry["callers"].items():
                # pstats caller value: (call_count, nonrec_calls, tt, ct)
                callers[caller] = (count, count, 0.0, count * interval)
            self.stats[key] = (direct, cumulative, tt, ct, callers)
        return self.stats

    def export(self, filename):
        self.create_stats()
        data = dict(self.stats)
        data[("__sampled__",)] = True
        with open(filename, "wb") as f:
            marshal.dump(data, f)


def _key(frame):
    """pstats key for a frame: (filename, lineno, funcname)."""
    return (frame[0], extract_lineno(frame[1]), frame[2])
