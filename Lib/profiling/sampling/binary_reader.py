"""Replay a binary profile written by ``BinaryCollector`` into any collector, and
convert one to another export format.
"""

import marshal
import struct

from .binary_collector import MAGIC

__all__ = ('BinaryReader', 'convert_binary_to_format')


def _make_location(loc):
    try:
        from _remote_debugging import LocationInfo
    except ImportError:
        return loc
    if loc is None:
        return None
    return LocationInfo(tuple(loc))


def _make_frame(frame):
    try:
        from _remote_debugging import FrameInfo
    except ImportError:
        return frame
    filename, loc, funcname, opcode = frame
    return FrameInfo((filename, _make_location(loc), funcname, opcode))


def _make_thread(thread):
    try:
        from _remote_debugging import ThreadInfo
    except ImportError:
        return thread
    thread_id, status, frames = thread
    return ThreadInfo((thread_id, status, [_make_frame(f) for f in frames]))


def _make_interp(interp):
    try:
        from _remote_debugging import InterpreterInfo
    except ImportError:
        return interp
    interp_id, threads = interp
    return InterpreterInfo((interp_id, [_make_thread(t) for t in threads]))


def _reconstruct(sample):
    return [_make_interp(i) for i in sample]


class BinaryReader:
    def __init__(self, filename):
        self.filename = filename
        self._file = None
        self.header = {}
        self._runs = []

    def __enter__(self):
        self._load()
        return self

    def __exit__(self, *exc):
        return False

    def _load(self):
        with open(self.filename, "rb") as f:
            data = f.read()
        if data[:8] != MAGIC:
            raise ValueError("not a Tachyon binary profile")
        pos = 8
        (hlen,) = struct.unpack_from("<I", data, pos)
        pos += 4
        self.header = marshal.loads(data[pos:pos + hlen])
        pos += hlen
        (blen,) = struct.unpack_from("<I", data, pos)
        pos += 4
        body = data[pos:pos + blen]
        if self.header.get("compression") == "zstd":
            from compression import zstd
            body = zstd.decompress(body)
        self._runs = marshal.loads(body)

    def replay_samples(self, collector):
        """Feed every recorded sample to *collector*; return the sample count."""
        count = 0
        for rep, repeat in self._runs:
            sample = _reconstruct(rep)
            for _ in range(repeat):
                collector.collect(sample, [0.0])
                count += 1
        return count

    def replay_stats(self):
        return self.header.get("stats", {})

    def get_info(self):
        """The recorded header: interval, mode, capture_config, compression."""
        return dict(self.header)

    @property
    def sample_interval_usec(self):
        return self.header.get("interval", 1000)

    @property
    def mode(self):
        return self.header.get("mode")


def convert_binary_to_format(input_path, output_path, output_format):
    """Load the binary profile at *input_path*, replay it into a collector for
    *output_format*, and export to *output_path*."""
    from .cli import _create_collector

    with BinaryReader(input_path) as reader:
        collector = _create_collector(
            output_format,
            sample_interval_usec=reader.sample_interval_usec,
            mode=reader.mode or 0,
        )
        if hasattr(collector, "set_replay_stats"):
            collector.set_replay_stats(reader.replay_stats())
        reader.replay_samples(collector)
        collector.export(output_path)
        return collector
