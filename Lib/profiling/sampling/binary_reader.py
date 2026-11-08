"""Replay a profile recorded with ``--binary`` into any collector, and convert
it to the other output formats.

The decoding is done by ``_remote_debugging.BinaryReader``; each replayed
sample reaches the collector as ``collect(stack_frames, timestamps_us)``
with one thread and the timestamps of the identical consecutive samples it
stands for.
"""

import _remote_debugging

__all__ = ('BinaryReader', 'convert_binary_to_format')


class BinaryReader:
    """Context manager over a binary profile file."""

    def __init__(self, filename):
        self.filename = filename
        self._reader = None
        self._closed = False

    def _get(self):
        if self._closed:
            raise RuntimeError("the binary profile reader is closed")
        if self._reader is None:
            self._reader = _remote_debugging.BinaryReader(self.filename)
        return self._reader

    def __enter__(self):
        self._get()
        return self

    def __exit__(self, *exc):
        self.close()
        return False

    def close(self):
        if self._reader is not None:
            self._reader.close()
            self._reader = None
        self._closed = True

    def get_info(self):
        return self._get().get_info()

    def replay_samples(self, collector, progress_callback=None):
        """Feed every recorded sample to *collector*; return how many there were."""
        return self._get().replay(collector, progress_callback)

    @property
    def sample_interval_usec(self):
        return self.get_info()["sample_interval_us"]

    @property
    def mode(self):
        return self.get_info()["mode"]

    def replay_stats(self):
        info = self.get_info()
        return {key: info[key] for key in
                ("duration_sec", "sample_rate", "error_rate", "missed_samples")
                if info.get(key) is not None}


def apply_recorded_stats(collector, info):
    """Give *collector* the run statistics stored in a binary profile."""
    set_stats = getattr(collector, "set_stats", None)
    if set_stats is None or info.get("duration_sec") is None:
        return
    set_stats(info["sample_interval_us"], info["duration_sec"],
              info["sample_rate"], error_rate=info.get("error_rate"),
              missed_samples=info.get("missed_samples"))


def convert_binary_to_format(input_path, output_path, output_format, **options):
    """Replay the binary profile at *input_path* into a collector for
    *output_format* and export it to *output_path*; returns the collector."""
    from .cli import _create_collector

    with BinaryReader(input_path) as reader:
        info = reader.get_info()
        collector = _create_collector(
            output_format,
            info["sample_interval_us"],
            skip_idle=False,
            mode=info["mode"],
            capture_config=info.get("capture_config"),
            **options,
        )
        reader.replay_samples(collector)
        apply_recorded_stats(collector, info)
        collector.export(output_path)
        return collector
