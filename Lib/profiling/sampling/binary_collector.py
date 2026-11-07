"""Compact binary recorder of raw samples, replayable later by ``BinaryReader``.

Samples are stored run-length encoded (identical consecutive samples are kept
once with a repeat count); the body is marshalled and may be zstd-compressed.
The header records the sampling interval, mode and capture configuration so a
replay can restore the measured statistics.
"""

import marshal
import struct

from .collector import Collector

__all__ = ('BinaryCollector',)

MAGIC = b"TACHYON1"


def _frame_tuple(frame):
    loc = frame[1]
    if loc is None:
        location = None
    elif isinstance(loc, int):
        location = (loc, loc, -1, -1)
    else:
        location = (loc[0], loc[1], loc[2], loc[3])
    return (frame[0], location, frame[2], frame[3])


def _sample_repr(stack_frames):
    out = []
    for interp in stack_frames or ():
        threads = []
        for thread in getattr(interp, "threads", ()) or ():
            frames = [_frame_tuple(f) for f in getattr(thread, "frame_info", ()) or ()]
            threads.append((getattr(thread, "thread_id", None),
                            getattr(thread, "status", 0) or 0,
                            frames))
        out.append((getattr(interp, "interpreter_id", 0), threads))
    return out


class BinaryCollector(Collector):
    def __init__(self, filename, sample_interval_usec=1000, *, compression="none",
                 mode=None, capture_config=None, skip_idle=False, opcodes=False, **kwargs):
        super().__init__(sample_interval_usec, skip_idle=skip_idle)
        self.filename = filename
        self.compression = compression or "none"
        self.mode = mode
        self.capture_config = capture_config
        self._runs = []           # list of [sample_repr, count]
        self.sample_count = 0
        self.stats = {}

    def collect(self, stack_frames, timestamps_us=None):
        n = len(timestamps_us) if timestamps_us else 1
        rep = _sample_repr(stack_frames)
        if self._runs and self._runs[-1][0] == rep:
            self._runs[-1][1] += n
        else:
            self._runs.append([rep, n])
        self.sample_count += n

    def set_stats(self, **kwargs):
        self.stats.update(kwargs)

    def export(self, filename=None):
        path = filename or self.filename
        header = {
            "interval": self.sample_interval_usec,
            "mode": self.mode,
            "capture_config": self.capture_config,
            "compression": self.compression,
            "stats": self.stats,
        }
        body = marshal.dumps(self._runs)
        if self.compression == "zstd":
            try:
                from compression import zstd
                body = zstd.compress(body)
            except Exception:
                header["compression"] = "none"
        header_bytes = marshal.dumps(header)
        with open(path, "wb") as f:
            f.write(MAGIC)
            f.write(struct.pack("<I", len(header_bytes)))
            f.write(header_bytes)
            f.write(struct.pack("<I", len(body)))
            f.write(body)
