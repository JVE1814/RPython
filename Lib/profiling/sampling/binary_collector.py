"""Record raw samples in the compact binary format (``--binary``).

The encoding itself lives in ``_remote_debugging.BinaryWriter``; this
collector adapts it to the collector protocol: it supplies timestamps,
stops cleanly when a sample does not fit the format, and records the
measured run statistics so a later ``replay`` can report them.
"""

import sys
import time

import _remote_debugging

from .collector import Collector

__all__ = ('BinaryCollector',)

_COMPRESSION = {"none": 0, "zstd": 1}


def _compression_code(compression):
    if compression in (None, "auto"):
        return 1 if _remote_debugging.zstd_available() else 0
    if isinstance(compression, int):
        return compression
    try:
        return _COMPRESSION[compression]
    except KeyError:
        raise ValueError(f"unknown compression {compression!r}") from None


class BinaryCollector(Collector):
    """Stream every sample to *filename*; ``export()`` completes the file."""

    def __init__(self, filename, sample_interval_usec=1000, *, skip_idle=False,
                 compression="auto", mode=None, capture_config=None, **kwargs):
        super().__init__(sample_interval_usec, skip_idle=skip_idle)
        self.filename = filename
        self.mode = mode
        self.capture_config = capture_config
        self.compression = compression
        self.running = True
        self._writer = _remote_debugging.BinaryWriter(
            filename, int(sample_interval_usec), 0,
            compression=_compression_code(compression),
            mode=mode, capture_config=capture_config,
        )

    @property
    def total_samples(self):
        return self._writer.total_samples

    def collect(self, stack_frames, timestamp_us=None):
        if not self.running:
            return
        if timestamp_us is None:
            timestamp_us = time.monotonic_ns() // 1000
        try:
            self._writer.write_sample(stack_frames, timestamp_us)
        except OverflowError as exc:
            if not self._writer.limit_reached:
                raise
            # A value outside the format's range: keep what was recorded so far.
            self.running = False
            print(f"Warning: {exc}; binary recording stopped, "
                  f"{self._writer.total_samples} samples kept", file=sys.stderr)

    def set_stats(self, sample_interval_usec=None, duration_sec=None,
                  sample_rate=None, error_rate=None, missed_samples=None,
                  **kwargs):
        if duration_sec is None or sample_rate is None:
            return
        self._writer.set_stats(duration_sec, sample_rate, error_rate,
                               missed_samples)

    def export(self, filename=None):
        self._writer.finalize()

    def get_stats(self):
        return self._writer.get_stats()

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc, tb):
        return self._writer.__exit__(exc_type, exc, tb)
