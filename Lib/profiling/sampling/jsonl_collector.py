"""JSONL (one JSON object per line) profile exporter.

The stream is a ``meta`` record, ``string_table`` and ``frame_table`` records
that give strings and frames their ids, ``agg`` records with the self and
cumulative sample counts of every frame, and an ``end`` record.  Strings and
frames get ids in the order they are first seen; tables and aggregates are
written in chunks of at most ``_CHUNK_SIZE`` items.  Every record carries the
same ``run_id``.  Location fields the sampler could not determine (negative
columns, a missing end line) are left out of a frame instead of being zero.
"""

import json
import uuid

from .collector import Collector, filter_internal_frames, normalize_location
from .constants import (
    PROFILING_MODE_ALL,
    PROFILING_MODE_CPU,
    PROFILING_MODE_EXCEPTION,
    PROFILING_MODE_GIL,
    PROFILING_MODE_WALL,
)

__all__ = ('JsonlCollector',)

_CHUNK_SIZE = 256

_MODE_NAMES = {
    PROFILING_MODE_WALL: "wall",
    PROFILING_MODE_CPU: "cpu",
    PROFILING_MODE_GIL: "gil",
    PROFILING_MODE_ALL: "all",
    PROFILING_MODE_EXCEPTION: "exception",
}


def _dumps(obj):
    return json.dumps(obj, ensure_ascii=False, separators=(",", ":"))


class JsonlCollector(Collector):
    aggregating = True

    def __init__(self, sample_interval_usec=1000, *, skip_idle=False,
                 mode=None, opcodes=False, **kwargs):
        super().__init__(sample_interval_usec, skip_idle=skip_idle)
        self.mode = mode
        self.run_id = uuid.uuid4().hex
        self._strings = []
        self._string_ids = {}
        self._frames = []           # frame_id -> frame definition (dict)
        self._frame_ids = {}
        self._self = []             # frame_id -> self samples
        self._cumulative = []       # frame_id -> cumulative samples
        self.samples_total = 0

    # -- interning

    def _string_id(self, text):
        sid = self._string_ids.get(text)
        if sid is None:
            sid = self._string_ids[text] = len(self._strings)
            self._strings.append(text)
        return sid

    def _get_or_create_frame_id(self, frame):
        filename, location, funcname = frame[0], frame[1], frame[2]
        if location is None:
            loc = None
        else:
            loc = tuple(normalize_location(location))
        key = (filename, funcname, loc)
        fid = self._frame_ids.get(key)
        if fid is not None:
            return fid
        func_id = self._string_id(funcname)
        path_id = self._string_id(filename)
        definition = {"frame_id": len(self._frames), "path_str_id": path_id,
                      "func_str_id": func_id}
        if loc is None:
            definition["line"] = 0
        else:
            lineno, end_lineno, col, end_col = (list(loc) + [None] * 4)[:4]
            definition["line"] = lineno or 0
            if end_lineno is not None and end_lineno > 0:
                definition["end_line"] = end_lineno
            if col is not None and col >= 0:
                definition["col"] = col
            if end_col is not None and end_col >= 0:
                definition["end_col"] = end_col
        fid = self._frame_ids[key] = len(self._frames)
        self._frames.append(definition)
        self._self.append(0)
        self._cumulative.append(0)
        return fid

    # -- collecting

    def collect(self, stack_frames, timestamps_us=None):
        weight = len(timestamps_us) if timestamps_us else 1
        if stack_frames and hasattr(stack_frames[0], "awaited_by"):
            for frames, thread_id, _task in self._iter_async_frames(stack_frames):
                self.process_frames(frames, thread_id, weight)
            return
        for frames, thread_id in self._iter_all_frames(stack_frames):
            self.process_frames(filter_internal_frames(frames), thread_id, weight)

    def process_frames(self, frames, _thread_id=None, weight=1):
        if not frames:
            return
        self.samples_total += weight
        seen = set()
        for depth, frame in enumerate(frames):
            fid = self._get_or_create_frame_id(frame)
            if depth == 0:
                self._self[fid] += weight
            if fid not in seen:
                seen.add(fid)
                self._cumulative[fid] += weight

    # -- export

    def _record(self, record_type, **fields):
        record = {"type": record_type, "v": 0, "run_id": self.run_id}
        record.update(fields)
        return _dumps(record)

    def export(self, filename):
        lines = []
        meta = {"sample_interval_usec": self.sample_interval_usec}
        if self.mode is not None:
            meta["mode"] = _MODE_NAMES.get(self.mode, self.mode)
        lines.append(self._record("meta", **meta))
        for start in range(0, len(self._strings), _CHUNK_SIZE):
            chunk = self._strings[start:start + _CHUNK_SIZE]
            lines.append(self._record("string_table", strings=[
                {"str_id": start + i, "value": text} for i, text in enumerate(chunk)
            ]))
        for start in range(0, len(self._frames), _CHUNK_SIZE):
            lines.append(self._record(
                "frame_table", frames=self._frames[start:start + _CHUNK_SIZE]))
        for start in range(0, len(self._frames), _CHUNK_SIZE):
            entries = [
                {"frame_id": fid, "self": self._self[fid],
                 "cumulative": self._cumulative[fid]}
                for fid in range(start, min(start + _CHUNK_SIZE, len(self._frames)))
            ]
            lines.append(self._record(
                "agg", kind="frame", scope="final",
                samples_total=self.samples_total, entries=entries))
        lines.append(self._record("end", samples_total=self.samples_total))
        with open(filename, "w", encoding="utf-8", errors="surrogatepass") as f:
            for line in lines:
                f.write(line)
                f.write("\n")
        return True
