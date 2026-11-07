"""The sampling engine: repeatedly snapshot a target process through
``_remote_debugging.RemoteUnwinder`` and feed the samples to a collector.
"""

import os
import time

from .constants import PROFILING_MODE_WALL

__all__ = ('SampleProfiler', 'sample', 'sample_live', 'dump_stack', '_is_process_running')


def _is_process_running(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    except OSError:
        return False
    return True


class SampleProfiler:
    def __init__(self, pid, sample_interval_usec=1000, *, all_threads=False,
                 mode=PROFILING_MODE_WALL, native=False, only_active_thread=False,
                 skip_non_matching_threads=True, **kwargs):
        self.pid = pid
        self.sample_interval_usec = sample_interval_usec
        self.all_threads = all_threads
        self.mode = mode
        self.native = native
        self.only_active_thread = only_active_thread
        self.skip_non_matching_threads = skip_non_matching_threads
        self.total_samples = 0
        self.missed_samples = 0
        self.realtime_stats = False
        import _remote_debugging
        self.unwinder = _remote_debugging.RemoteUnwinder(
            pid,
            all_threads=all_threads,
            only_active_thread=only_active_thread,
            mode=mode,
            native=native,
            skip_non_matching_threads=skip_non_matching_threads,
        )

    def sample(self, collector, duration_sec=10):
        interval_sec = self.sample_interval_usec / 1_000_000
        deadline = time.perf_counter() + duration_sec
        start = time.perf_counter()
        while True:
            now = time.perf_counter()
            if now >= deadline:
                break
            try:
                stack = self.unwinder.get_stack_trace()
                collector.collect(stack)
                self.total_samples += 1
            except (RuntimeError, OSError, UnicodeDecodeError):
                self.missed_samples += 1
                collector.collect_failed_sample()
            if not _is_process_running(self.pid):
                break
            nxt = now + interval_sec
            sleep = nxt - time.perf_counter()
            if sleep > 0:
                time.sleep(sleep)
        elapsed = max(time.perf_counter() - start, 1e-9)
        if hasattr(collector, "set_stats"):
            collector.set_stats(
                duration_sec=elapsed,
                sample_rate=self.total_samples / elapsed,
                error_rate=(self.missed_samples / max(self.total_samples + self.missed_samples, 1)),
                missed_samples=self.missed_samples,
            )
        return collector


def dump_stack(pid, *, async_aware=None, mode=PROFILING_MODE_WALL, blocking=False,
               all_threads=True, **kwargs):
    """A single stack snapshot of *pid*, formatted for display."""
    import _remote_debugging
    from .dump import format_stack_dump

    unwinder = _remote_debugging.RemoteUnwinder(
        pid, all_threads=all_threads, mode=mode
    )
    if async_aware == "running":
        trace = unwinder.get_async_stack_trace()
    elif async_aware == "all":
        trace = unwinder.get_all_awaited_by()
    else:
        trace = unwinder.get_stack_trace()
    return format_stack_dump(trace, pid)


def sample(pid, collector=None, *, sample_interval_usec=1000, duration_sec=10,
           filename=None, output_format="pstats", all_threads=False,
           mode=PROFILING_MODE_WALL, native=False, **kwargs):
    profiler = SampleProfiler(
        pid, sample_interval_usec=sample_interval_usec,
        all_threads=all_threads, mode=mode, native=native,
    )
    if collector is None:
        from .pstats_collector import PstatsCollector
        collector = PstatsCollector(sample_interval_usec)
    profiler.sample(collector, duration_sec=duration_sec)
    if filename is not None:
        collector.export(filename)
    return collector


def sample_live(pid, *, sample_interval_usec=1000, duration_sec=10,
                all_threads=False, mode=PROFILING_MODE_WALL, **kwargs):
    from .live_collector import LiveStatsCollector
    profiler = SampleProfiler(
        pid, sample_interval_usec=sample_interval_usec,
        all_threads=all_threads, mode=mode,
    )
    collector = LiveStatsCollector(sample_interval_usec)
    profiler.sample(collector, duration_sec=duration_sec)
    return collector
