"""Exporter for the Firefox Profiler ("Gecko") processed-profile JSON format.

Each sampled thread becomes a profile thread with column-oriented tables
(``funcTable``, ``frameTable``, ``stackTable``, ``samples`` and ``markers``)
that index into one shared string array.  Interval markers show how a
thread's state changed over time: whether it held the GIL ("Has GIL",
"No GIL", "Waiting for GIL"), whether it was on a CPU ("On CPU",
"Off CPU"), and, with ``opcodes=True``, which bytecode instruction was on
top of its stack.

Sample rows are buffered per thread and moved to temporary spill files
once a buffer exceeds ``DEFAULT_SPILL_BUFFER_BYTES``, so long recordings do
not have to stay in memory; ``export`` reads them back while writing.
"""

import json
import os
import struct
import tempfile
import time

from .collector import Collector, filter_internal_frames, normalize_location
from .constants import (
    THREAD_STATUS_GIL_REQUESTED,
    THREAD_STATUS_HAS_GIL,
    THREAD_STATUS_MAIN_THREAD,
    THREAD_STATUS_ON_CPU,
    THREAD_STATUS_UNKNOWN,
)
from .opcode_utils import format_opcode

__all__ = ('GeckoCollector',)

DEFAULT_SPILL_BUFFER_BYTES = 1 << 20

# one spilled sample: stack index, time in ms
_SAMPLE_ROW = struct.Struct("<qd")

# index into CATEGORIES
_CAT_OTHER = 0
_CAT_PYTHON = 1
_CAT_NATIVE = 2
_CAT_GC = 3
_CAT_GIL = 4
_CAT_CPU = 5
_CAT_OPCODE = 6

CATEGORIES = [
    {"name": "Other", "color": "grey", "subcategories": ["Other"]},
    {"name": "Python", "color": "yellow", "subcategories": ["Other"]},
    {"name": "Native", "color": "blue", "subcategories": ["Other"]},
    {"name": "GC", "color": "orange", "subcategories": ["Other"]},
    {"name": "GIL", "color": "green", "subcategories": ["Other"]},
    {"name": "CPU", "color": "purple", "subcategories": ["Other"]},
    {"name": "Opcodes", "color": "magenta", "subcategories": ["Other"]},
]

_OPCODE_SCHEMA = {
    "name": "Opcode",
    "display": ["marker-chart", "marker-table", "timeline-overview"],
    "tooltipLabel": "{marker.data.opname}",
    "tableLabel": "{marker.data.opname} at line {marker.data.line}",
    "chartLabel": "{marker.data.opname}",
    "fields": [
        {"key": "opname", "label": "Instruction", "format": "string"},
        {"key": "opcode", "label": "Opcode", "format": "integer"},
        {"key": "line", "label": "Line", "format": "integer"},
        {"key": "column", "label": "Column", "format": "integer"},
        {"key": "function", "label": "Function", "format": "string"},
        {"key": "file", "label": "File", "format": "string"},
    ],
}

_SYNTHETIC_CATEGORIES = {"<GC>": _CAT_GC, "<native>": _CAT_NATIVE}


class _ThreadData:
    """Tables and buffered samples of one profiled thread."""

    def __init__(self, tid):
        self.tid = tid
        self.is_main = False
        self.funcs = {}             # (name idx, file idx) -> func index
        self.func_rows = []         # (name idx, file idx, first line)
        self.frames = {}            # (func index, line, column) -> frame index
        self.frame_rows = []        # (func index, line, column, category)
        self.stacks = {}            # (prefix, frame index) -> stack index
        self.stack_rows = []        # (prefix, frame index, category)
        self.buffer = []            # [(stack index, time ms)]
        self.spill = None           # temporary file with older rows
        self.sample_total = 0
        self.markers = []           # (name idx, start, end, category, data)
        self.open_states = {}       # marker kind -> (name, start)


class GeckoCollector(Collector):
    aggregating = True

    def __init__(self, sample_interval_usec=1000, *, skip_idle=False,
                 mode=None, opcodes=False, **kwargs):
        super().__init__(sample_interval_usec, skip_idle=skip_idle)
        self.mode = mode
        self.opcodes_enabled = bool(opcodes)
        self.global_strings = ["(root)"]
        self._string_index = {"(root)": 0}
        self.threads = {}
        self.sample_count = 0
        self.opcode_state = {}      # tid -> (opcode, line, column, function, file, start)
        self.start_time = time.time() * 1000.0
        self._start_us = None
        self._last_time = 0.0
        self._exported = False

    # -- tables

    def _string(self, text):
        text = str(text)
        idx = self._string_index.get(text)
        if idx is None:
            idx = self._string_index[text] = len(self.global_strings)
            self.global_strings.append(text)
        return idx

    def _thread(self, tid):
        data = self.threads.get(tid)
        if data is None:
            data = self.threads[tid] = _ThreadData(tid)
        return data

    def _stack_index(self, data, frames):
        prefix = None
        for frame in reversed(frames):
            filename, location, funcname = frame[0], frame[1], frame[2]
            if location is None:
                line, column = 0, -1
            else:
                loc = normalize_location(location)
                line = loc[0] or 0
                column = loc[2] if loc[2] is not None else -1
            name_idx = self._string(funcname)
            file_idx = self._string(filename)
            func = data.funcs.get((name_idx, file_idx))
            if func is None:
                func = data.funcs[(name_idx, file_idx)] = len(data.func_rows)
                data.func_rows.append((name_idx, file_idx, line))
            category = _SYNTHETIC_CATEGORIES.get(funcname, _CAT_PYTHON)
            fkey = (func, line, column)
            frame_idx = data.frames.get(fkey)
            if frame_idx is None:
                frame_idx = data.frames[fkey] = len(data.frame_rows)
                data.frame_rows.append((func, line, column, category))
            skey = (prefix, frame_idx)
            stack = data.stacks.get(skey)
            if stack is None:
                stack = data.stacks[skey] = len(data.stack_rows)
                data.stack_rows.append((prefix, frame_idx, category))
            prefix = stack
        return prefix

    def _add_samples(self, data, stack, times):
        data.buffer.extend((stack, t) for t in times)
        data.sample_total += len(times)
        if len(data.buffer) * _SAMPLE_ROW.size > DEFAULT_SPILL_BUFFER_BYTES:
            self._spill(data)

    def _spill(self, data):
        if data.spill is None:
            data.spill = tempfile.TemporaryFile()
        data.spill.write(b"".join(
            _SAMPLE_ROW.pack(-1 if stack is None else stack, t)
            for stack, t in data.buffer))
        data.buffer = []

    def _iter_samples(self, data):
        if data.spill is not None:
            data.spill.flush()
            data.spill.seek(0)
            while True:
                chunk = data.spill.read(_SAMPLE_ROW.size * 4096)
                if not chunk:
                    break
                for stack, t in _SAMPLE_ROW.iter_unpack(chunk):
                    yield (None if stack < 0 else stack), t
        yield from data.buffer

    def _cleanup_spills(self):
        for data in self.threads.values():
            if data.spill is not None:
                try:
                    data.spill.close()
                except OSError:
                    pass
                data.spill = None

    # -- markers

    def _set_state(self, data, kind, name, now, category):
        current = data.open_states.get(kind)
        if current is not None and current[0] == name:
            return
        if current is not None:
            data.markers.append((self._string(current[0]), current[1], now,
                                 category, {"type": kind}))
        data.open_states[kind] = (name, now)

    def _track_status(self, data, status, now):
        if status & THREAD_STATUS_UNKNOWN:
            return
        if status & THREAD_STATUS_HAS_GIL:
            gil = "Has GIL"
        elif status & THREAD_STATUS_GIL_REQUESTED:
            gil = "Waiting for GIL"
        else:
            gil = "No GIL"
        self._set_state(data, "GIL", gil, now, _CAT_GIL)
        cpu = "On CPU" if status & THREAD_STATUS_ON_CPU else "Off CPU"
        self._set_state(data, "CPU", cpu, now, _CAT_CPU)

    def _emit_opcode_marker(self, data, state, end):
        opcode, line, column, function, filename, start = state
        opname = format_opcode(opcode)
        data.markers.append((self._string(opname), start, end, _CAT_OPCODE, {
            "type": "Opcode",
            "opname": opname,
            "opcode": opcode,
            "line": line,
            "column": column,
            "function": function,
            "file": filename,
        }))

    def _track_opcode(self, data, frames, now):
        tid = data.tid
        state = self.opcode_state.get(tid)
        top = frames[0] if frames else None
        opcode = top[3] if top is not None and len(top) > 3 else None
        if opcode is None:
            if state is not None:
                self._emit_opcode_marker(data, state, now)
                del self.opcode_state[tid]
            return
        location = normalize_location(top[1])
        line = location[0] or 0
        column = location[2] if location[2] is not None else -1
        key = (opcode, line, column, top[2], top[0])
        if state is not None and state[:5] == key:
            return
        if state is not None:
            self._emit_opcode_marker(data, state, now)
        self.opcode_state[tid] = key + (now,)

    # -- collecting

    def _times(self, timestamps_us):
        if timestamps_us:
            stamps = list(timestamps_us)
        else:
            stamps = [time.monotonic() * 1_000_000]
        if self._start_us is None:
            self._start_us = stamps[0]
        times = [max(0.0, (t - self._start_us) / 1000.0) for t in stamps]
        self._last_time = max(self._last_time, times[-1])
        return times

    def collect(self, stack_frames, timestamps_us=None):
        if self._exported:
            raise RuntimeError("cannot collect samples after export")
        times = self._times(timestamps_us)
        self.sample_count += len(times)
        if stack_frames and hasattr(stack_frames[0], "awaited_by"):
            for frames, thread_id, _task in self._iter_async_frames(stack_frames):
                if not frames:
                    continue
                data = self._thread(thread_id)
                self._add_samples(data, self._stack_index(data, frames), times)
            return
        for interpreter in stack_frames or ():
            for thread in self._iter_threads([interpreter]):
                data = self._thread(getattr(thread, "thread_id", None))
                status = getattr(thread, "status", 0) or 0
                if status & THREAD_STATUS_MAIN_THREAD:
                    data.is_main = True
                frames = filter_internal_frames(getattr(thread, "frame_info", None))
                self._track_status(data, status, times[0])
                if self.opcodes_enabled:
                    self._track_opcode(data, frames, times[0])
                if frames:
                    self._add_samples(data, self._stack_index(data, frames), times)

    # -- the profile

    def _thread_profile(self, data, end_time):
        markers = list(data.markers)
        for kind, (name, start) in data.open_states.items():
            category = _CAT_GIL if kind == "GIL" else _CAT_CPU
            markers.append((self._string(name), start, max(start, end_time),
                            category, {"type": kind}))
        state = self.opcode_state.get(data.tid)
        if state is not None:
            opcode, line, column, function, filename, start = state
            opname = format_opcode(opcode)
            markers.append((self._string(opname), start, max(start, end_time),
                            _CAT_OPCODE, {
                                "type": "Opcode", "opname": opname,
                                "opcode": opcode, "line": line,
                                "column": column, "function": function,
                                "file": filename}))
        markers.sort(key=lambda m: m[1])
        samples = list(self._iter_samples(data))
        funcs = data.func_rows
        frames = data.frame_rows
        stacks = data.stack_rows
        name = "MainThread" if data.is_main else f"Thread {data.tid}"
        return {
            "name": name,
            "processType": "default",
            "processName": "Python",
            "pid": str(os.getpid()),
            "tid": data.tid,
            "isMainThread": data.is_main,
            "registerTime": 0,
            "unregisterTime": None,
            "funcTable": {
                "length": len(funcs),
                "name": [f[0] for f in funcs],
                "isJS": [True] * len(funcs),
                "relevantForJS": [False] * len(funcs),
                "resource": [-1] * len(funcs),
                "fileName": [f[1] for f in funcs],
                "lineNumber": [f[2] for f in funcs],
                "columnNumber": [None] * len(funcs),
            },
            "frameTable": {
                "length": len(frames),
                "address": [-1] * len(frames),
                "inlineDepth": [0] * len(frames),
                "category": [f[3] for f in frames],
                "subcategory": [0] * len(frames),
                "func": [f[0] for f in frames],
                "nativeSymbol": [None] * len(frames),
                "innerWindowID": [None] * len(frames),
                "implementation": [None] * len(frames),
                "line": [f[1] for f in frames],
                "column": [f[2] if f[2] >= 0 else None for f in frames],
            },
            "stackTable": {
                "length": len(stacks),
                "prefix": [s[0] for s in stacks],
                "frame": [s[1] for s in stacks],
                "category": [s[2] for s in stacks],
                "subcategory": [0] * len(stacks),
            },
            "samples": {
                "length": len(samples),
                "stack": [s[0] for s in samples],
                "time": [s[1] for s in samples],
                "eventDelay": [0] * len(samples),
                "weight": None,
                "weightType": "samples",
            },
            "markers": {
                "length": len(markers),
                "name": [m[0] for m in markers],
                "startTime": [m[1] for m in markers],
                "endTime": [m[2] for m in markers],
                "phase": [1] * len(markers),
                "category": [m[3] for m in markers],
                "data": [m[4] for m in markers],
            },
            "resourceTable": {"length": 0, "lib": [], "name": [], "host": [],
                              "type": []},
            "nativeSymbols": {"length": 0, "libIndex": [], "address": [],
                              "name": [], "functionSize": []},
        }

    def _build_profile(self):
        end_time = self._last_time
        ordered = sorted(self.threads.values(),
                         key=lambda d: (not d.is_main, str(d.tid)))
        threads = [self._thread_profile(d, end_time) for d in ordered]
        return {
            "meta": {
                "interval": self.sample_interval_usec / 1000.0,
                "startTime": self.start_time,
                "processType": 0,
                "product": "Python sampling profiler",
                "stackwalk": 0,
                "debug": False,
                "version": 29,
                "preprocessedProfileVersion": 48,
                "categories": CATEGORIES,
                "markerSchema": [_OPCODE_SCHEMA] if self.opcodes_enabled else [],
                "sampleUnits": {"time": "ms", "eventDelay": "ms",
                                "threadCPUDelta": "µs"},
                "symbolicated": True,
                "pausedRanges": [],
                "extensions": {"id": [], "name": [], "baseURL": [],
                               "length": 0},
            },
            "libs": [],
            "pages": [],
            "counters": [],
            "threads": threads,
            "shared": {"stringArray": list(self.global_strings)},
        }

    def _stream_profile(self, file):
        json.dump(self._build_profile(), file, ensure_ascii=False,
                  separators=(",", ":"))

    def export(self, filename):
        directory = os.path.dirname(os.path.abspath(filename))
        fd, tmp = tempfile.mkstemp(prefix=".gecko-", suffix=".json.tmp",
                                   dir=directory)
        try:
            with os.fdopen(fd, "w", encoding="utf-8",
                           errors="surrogatepass") as file:
                self._stream_profile(file)
            os.replace(tmp, filename)
        except BaseException:
            try:
                os.unlink(tmp)
            except OSError:
                pass
            raise
        finally:
            self._exported = True
            self._cleanup_spills()
        print(f"Gecko profile written to {filename}")
        print("Open it in the Firefox Profiler: https://profiler.firefox.com/")
        return True
