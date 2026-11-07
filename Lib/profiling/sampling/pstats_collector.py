"""Collector that turns samples into a ``pstats``-compatible statistics table.

Every sample gives one *direct* hit to the frame on top of each thread's
stack and one *cumulative* hit to every distinct frame on the stack (a
recursive function is counted once per sample).  ``create_stats`` converts
the hit counts into the five-tuples ``pstats`` uses -- ``(cc, nc, tt, ct,
callers)`` with ``cc`` the direct and ``nc`` the cumulative count and the
times estimated as count x sampling interval.  ``export`` marshals the
table with a ``("__sampled__",)`` marker so ``pstats.Stats`` knows the times
are estimates; ``print_stats`` prints the report of the command line tool.
"""

import marshal
import sys

from .collector import Collector, extract_lineno, filter_internal_frames
from .constants import (
    PROFILING_MODE_CPU,
    PROFILING_MODE_EXCEPTION,
    PROFILING_MODE_GIL,
    SORT_MODE_CUMTIME,
    SORT_MODE_CUMUL_PCT,
    SORT_MODE_NSAMPLES,
    SORT_MODE_NSAMPLES_CUMUL,
    SORT_MODE_SAMPLE_PCT,
    SORT_MODE_TOTTIME,
)

__all__ = ('PstatsCollector',)

SORT_MODE_NAME = -1

# explanation printed when a filtered mode recorded nothing
_EMPTY_MODE_HINTS = {
    PROFILING_MODE_CPU: "In CPU mode only threads running on a CPU are sampled; "
                        "the target may have been idle.",
    PROFILING_MODE_GIL: "In GIL mode only threads holding the GIL are sampled; "
                        "the target may never have held it.",
    PROFILING_MODE_EXCEPTION: "In exception mode only threads handling an "
                              "exception are sampled; none was.",
}


def _frame_key(frame):
    """pstats key for a frame: (filename, lineno, funcname)."""
    return (frame[0], extract_lineno(frame[1]), frame[2])


def _time_unit(seconds):
    if seconds >= 1.0:
        return "s", 1.0
    if seconds >= 1e-3:
        return "ms", 1e3
    return "μs", 1e6


class PstatsCollector(Collector):
    aggregating = True

    def __init__(self, sample_interval_usec=1000, *, skip_idle=False,
                 opcodes=False, **kwargs):
        super().__init__(sample_interval_usec, skip_idle=skip_idle)
        self.sample_interval_sec = sample_interval_usec / 1_000_000
        # key -> {"direct_calls", "cumulative_calls", "callers": {caller: count}}
        self.result = {}
        self.stats = {}
        self.total_samples = 0

    # -- collecting

    def collect(self, stack_frames, timestamps_us=None):
        weight = len(timestamps_us) if timestamps_us else 1
        self.total_samples += weight
        if stack_frames and hasattr(stack_frames[0], "awaited_by"):
            for frames, _thread_id, _task in self._iter_async_frames(stack_frames):
                self._add_stack(frames, weight)
            return
        for frames, _thread_id in self._iter_all_frames(stack_frames):
            self._add_stack(filter_internal_frames(frames), weight)

    def _add_stack(self, frames, weight):
        if not frames:
            return
        seen = set()
        result = self.result
        for depth, frame in enumerate(frames):
            key = _frame_key(frame)
            entry = result.get(key)
            if entry is None:
                entry = result[key] = {
                    "direct_calls": 0,
                    "cumulative_calls": 0,
                    "callers": {},
                }
            if depth == 0:
                entry["direct_calls"] += weight
            if key not in seen:
                entry["cumulative_calls"] += weight
                seen.add(key)
            if depth + 1 < len(frames):
                caller = _frame_key(frames[depth + 1])
                callers = entry["callers"]
                callers[caller] = callers.get(caller, 0) + weight

    # -- the pstats table

    def create_stats(self):
        self.stats = {}
        interval = self.sample_interval_sec
        for key, entry in self.result.items():
            direct = entry.get("direct_calls", 0)
            cumulative = entry.get("cumulative_calls", 0)
            callers = {}
            for caller, count in entry.get("callers", {}).items():
                # caller value: (calls, primitive calls, tottime, cumtime)
                callers[caller] = (count, count, 0.0, count * interval)
            self.stats[key] = (direct, cumulative, direct * interval,
                               cumulative * interval, callers)
        return self.stats

    def export(self, filename):
        self.create_stats()
        data = dict(self.stats)
        data[("__sampled__",)] = True
        with open(filename, "wb") as f:
            marshal.dump(data, f)
        return True

    # -- the text report

    def print_stats(self, sort=SORT_MODE_NSAMPLES, limit=None, show_summary=True,
                    mode=None, **kwargs):
        import _colorize
        colors = _colorize.get_colors(file=sys.stdout)
        self.create_stats()
        stats = self.stats
        if not stats:
            print("No samples were collected.")
            hint = _EMPTY_MODE_HINTS.get(mode)
            if hint:
                print(hint)
            return

        total_direct = sum(v[0] for v in stats.values()) or 1
        max_time = max(max(v[2], v[3]) for v in stats.values())
        unit, scale = _time_unit(max_time)

        rows = list(stats.items())
        sort_keys = {
            SORT_MODE_NSAMPLES: lambda kv: kv[1][0],
            SORT_MODE_TOTTIME: lambda kv: kv[1][2],
            SORT_MODE_CUMTIME: lambda kv: kv[1][3],
            SORT_MODE_SAMPLE_PCT: lambda kv: kv[1][0] / total_direct,
            SORT_MODE_CUMUL_PCT: lambda kv: kv[1][1] / total_direct,
            SORT_MODE_NSAMPLES_CUMUL: lambda kv: kv[1][1],
        }
        if sort == SORT_MODE_NAME:
            rows.sort(key=lambda kv: (str(kv[0][2]), str(kv[0][0]), kv[0][1]))
        else:
            rows.sort(key=sort_keys.get(sort, sort_keys[SORT_MODE_NSAMPLES]),
                      reverse=True)
        shown = rows[:limit] if limit else rows

        print(f"{colors.BOLD_BLUE}Profile Stats:{colors.RESET}")
        header = (f"{'nsamples':>14}  {'sample%':>8}  {'tottime (' + unit + ')':>12}  "
                  f"{'cumul%':>8}  {'cumtime (' + unit + ')':>12}  "
                  f"filename:lineno(function)")
        print(f"{colors.BOLD}{header}{colors.RESET}")
        for (filename, lineno, funcname), (cc, nc, tt, ct, _callers) in shown:
            count = f"{cc}/{nc}" if cc != nc else f"{cc}"
            print(f"{count:>14}  {100.0 * cc / total_direct:>7.1f}%  "
                  f"{tt * scale:>12.3f}  {100.0 * nc / total_direct:>7.1f}%  "
                  f"{ct * scale:>12.3f}  "
                  f"{colors.CYAN}{filename}{colors.RESET}:{lineno}"
                  f"({colors.GREEN}{funcname}{colors.RESET})")
        if limit and len(rows) > limit:
            print(f"  ... {len(rows) - limit} more entries not shown")

        if show_summary:
            self._print_summary(rows, colors)

    def _print_summary(self, rows, colors):
        print()
        print(f"{colors.BOLD_BLUE}Summary of Interesting Functions:{colors.RESET}")

        def label(key):
            filename, lineno, funcname = key
            return f"{funcname} ({filename or '?'}:{lineno})"

        def section(title, scored, describe):
            scored = [item for item in scored if item[0] > 0]
            scored.sort(key=lambda item: item[0], reverse=True)
            print()
            print(f"{colors.BOLD}{title}{colors.RESET}")
            if not scored:
                print("  (none)")
            for score, key, entry in scored[:5]:
                print(f"  {describe(score, entry)}  {label(key)}")

        unit, scale = _time_unit(max((max(e[2], e[3]) for _, e in rows), default=0))

        section(
            "Functions with Highest Direct/Cumulative Ratio (Hot Spots):",
            [((e[2] / e[3]) if e[3] else 0.0, k, e) for k, e in rows],
            lambda s, e: f"{s:.3f} direct/cumulative ratio, "
                         f"{e[2] * scale:.3f}{unit} direct",
        )
        section(
            "Functions with Highest Call Frequency (Indirect Calls):",
            [(e[1] - e[0], k, e) for k, e in rows],
            lambda s, e: f"{s} indirect samples, {e[1]} on stack",
        )
        section(
            "Functions with Highest Call Magnification (Cumulative/Direct):",
            [((e[3] / e[2]) if e[2] else 0.0, k, e) for k, e in rows],
            lambda s, e: f"{s:.1f}x magnification, "
                         f"{e[3] * scale:.3f}{unit} cumulative",
        )
