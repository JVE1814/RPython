"""The sampling engine: repeatedly snapshot a target process through
``_remote_debugging.RemoteUnwinder`` and feed the snapshots to a collector.

Collectors that only aggregate (``collector.aggregating`` true) receive runs of
identical consecutive snapshots as one call,
``collect(stack_frames, timestamps_us=[...])``; the others get every snapshot
on its own as ``collect(stack_frames)``.
"""

import os
import sys
import time

import _remote_debugging

from .constants import (
    PROFILING_MODE_ALL,
    PROFILING_MODE_CPU,
    PROFILING_MODE_EXCEPTION,
    PROFILING_MODE_GIL,
    PROFILING_MODE_WALL,
)

__all__ = ('SampleProfiler', 'sample', 'sample_live', 'dump_stack',
           '_is_process_running')

# Most identical snapshots an aggregating collector is handed at once.
MAX_PENDING_SAMPLES = 1000

# Share of the expected samples that may be missed before a warning is printed.
_MISSED_WARNING_THRESHOLD = 0.05

# Errors a single snapshot may fail with while the target keeps running
# (it changed under the reader, or a page was unreadable for a moment).
_TRANSIENT_ERRORS = (RuntimeError, UnicodeDecodeError, MemoryError, OSError)


def _is_process_running(pid):
    if pid <= 0:
        return False
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    except OSError:
        return False
    return True


def _new_unwinder(pid, *, all_threads, mode, native, gc, opcodes,
                  skip_non_matching_threads, cache_frames=True, stats=False):
    return _remote_debugging.RemoteUnwinder(
        pid,
        all_threads=all_threads,
        mode=mode,
        native=native,
        gc=gc,
        opcodes=opcodes,
        skip_non_matching_threads=skip_non_matching_threads,
        cache_frames=cache_frames,
        stats=stats,
    )


class SampleProfiler:
    """Takes snapshots of process *pid* every *sample_interval_usec*."""

    def __init__(self, pid, sample_interval_usec, all_threads, *,
                 mode=PROFILING_MODE_WALL, native=False, gc=True,
                 opcodes=False, skip_non_matching_threads=True,
                 collect_stats=False, blocking=False):
        self.pid = pid
        self.sample_interval_usec = sample_interval_usec
        self.all_threads = all_threads
        self.mode = mode
        self.native = native
        self.gc = gc
        self.opcodes = opcodes
        self.blocking = blocking
        self.collect_stats = collect_stats
        self.realtime_stats = False
        self.total_samples = 0
        self.failed_samples = 0
        self.unwinder = _new_unwinder(
            pid, all_threads=all_threads, mode=mode, native=native, gc=gc,
            opcodes=opcodes,
            skip_non_matching_threads=skip_non_matching_threads,
            stats=collect_stats,
        )

    def _snapshot(self, async_aware):
        unwinder = self.unwinder
        if self.blocking:
            unwinder.pause_threads()
        try:
            if async_aware == "all":
                return unwinder.get_all_awaited_by()
            if async_aware:
                return unwinder.get_async_stack_trace()
            return unwinder.get_stack_trace()
        finally:
            if self.blocking:
                unwinder.resume_threads()

    def sample(self, collector, duration_sec=10, *, async_aware=None):
        interval = self.sample_interval_usec / 1_000_000
        if duration_sec is None:
            duration_sec = float("inf")     # until the target ends
        aggregating = bool(getattr(collector, "aggregating", False))
        pending = None          # the snapshot being accumulated
        pending_times = []

        def flush():
            nonlocal pending, pending_times
            if pending_times:
                collector.collect(pending, timestamps_us=pending_times)
            pending = None
            pending_times = []

        attempts = 0
        interrupted = False
        start = time.perf_counter()
        elapsed = 0.0
        while elapsed < duration_sec:
            now = time.perf_counter()
            # sample when the next slot is due (a small tolerance absorbs
            # rounding of the slot times)
            if now >= start + attempts * interval - 1e-9:
                attempts += 1
                try:
                    snapshot = self._snapshot(async_aware)
                except KeyboardInterrupt:
                    interrupted = True
                    attempts -= 1
                    break
                except ProcessLookupError:
                    self.failed_samples += 1
                    break
                except _TRANSIENT_ERRORS:
                    self.failed_samples += 1
                else:
                    self.total_samples += 1
                    if aggregating:
                        if pending_times and not (snapshot is pending or snapshot == pending):
                            flush()
                        pending = snapshot
                        pending_times.append(int(now * 1_000_000))
                        if len(pending_times) >= MAX_PENDING_SAMPLES:
                            flush()
                    else:
                        collector.collect(snapshot)
            else:
                pause = start + attempts * interval - now
                if pause > 0:
                    time.sleep(min(pause, 0.1))
            elapsed = time.perf_counter() - start
        flush()

        if interrupted:
            print("Interrupted by user.")
        elapsed = max(elapsed, 1e-9)
        rate = self.total_samples / elapsed
        error_rate = (100.0 * self.failed_samples / attempts) if attempts else 0.0
        expected = int(elapsed / interval) if interval > 0 else attempts
        missed = max(expected - attempts, 0)
        missed_pct = (100.0 * missed / expected) if expected else 0.0

        print(f"Captured {self.total_samples} samples in {elapsed:.2f} seconds")
        print(f"Sample rate: {rate:.2f} samples/sec")
        print(f"Error rate: {error_rate:.2f}%")
        if (not interrupted and expected
                and missed / expected > _MISSED_WARNING_THRESHOLD):
            print(f"Warning: missed {missed} samples from the expected total "
                  f"of {expected} ({missed_pct:.2f}%)")

        set_stats = getattr(collector, "set_stats", None)
        if set_stats is not None:
            set_stats(self.sample_interval_usec, elapsed, rate, error_rate,
                      missed_samples=missed_pct)
        return collector


def _skip_non_matching(mode):
    return mode != PROFILING_MODE_ALL


def dump_stack(pid, *, all_threads=False, async_aware=None, native=False,
               gc=True, opcodes=False, blocking=False, mode=PROFILING_MODE_ALL):
    """One snapshot of *pid*: the raw interpreter/thread records (or the
    asyncio task records with *async_aware*)."""
    unwinder = _new_unwinder(
        pid, all_threads=all_threads, mode=mode, native=native, gc=gc,
        opcodes=opcodes, skip_non_matching_threads=_skip_non_matching(mode),
    )
    if blocking:
        unwinder.pause_threads()
    try:
        if async_aware == "all":
            return unwinder.get_all_awaited_by()
        if async_aware:
            return unwinder.get_async_stack_trace()
        return unwinder.get_stack_trace()
    finally:
        if blocking:
            unwinder.resume_threads()


def _make_profiler(pid, collector, *, all_threads, mode, native, gc, opcodes,
                   realtime_stats, blocking):
    interval = getattr(collector, "sample_interval_usec", 1000)
    try:
        return SampleProfiler(
            pid, interval, all_threads=all_threads, mode=mode, native=native,
            gc=gc, opcodes=opcodes,
            skip_non_matching_threads=_skip_non_matching(mode),
            collect_stats=realtime_stats, blocking=blocking,
        )
    except PermissionError:
        raise
    except (OSError, RuntimeError, ValueError) as exc:
        raise SystemExit(f"Cannot profile process {pid}: {exc}") from exc


def sample(pid, collector, *, duration_sec=10, all_threads=False,
           realtime_stats=False, mode=PROFILING_MODE_WALL, async_aware=None,
           native=False, gc=True, opcodes=False, blocking=False):
    """Sample process *pid* into *collector* for *duration_sec* seconds."""
    profiler = _make_profiler(
        pid, collector, all_threads=all_threads, mode=mode, native=native,
        gc=gc, opcodes=opcodes, realtime_stats=realtime_stats,
        blocking=blocking,
    )
    profiler.realtime_stats = realtime_stats
    profiler.sample(collector, duration_sec, async_aware=async_aware)
    return collector


def sample_live(pid, collector, *, duration_sec=10, all_threads=False,
                realtime_stats=False, mode=PROFILING_MODE_WALL,
                async_aware=None, native=False, gc=True, opcodes=False,
                blocking=False):
    """Like :func:`sample`, showing the live terminal view of *collector*."""
    profiler = _make_profiler(
        pid, collector, all_threads=all_threads, mode=mode, native=native,
        gc=gc, opcodes=opcodes, realtime_stats=realtime_stats,
        blocking=blocking,
    )
    profiler.realtime_stats = realtime_stats
    run = getattr(collector, "run_live", None)
    if run is not None:
        run(profiler, duration_sec, async_aware=async_aware)
    else:
        profiler.sample(collector, duration_sec, async_aware=async_aware)
    return collector
