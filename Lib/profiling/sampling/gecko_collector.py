"""Exporter for the Firefox Profiler ("gecko") JSON format.

Each sampled thread becomes a gecko thread with the column-oriented
``funcTable``/``frameTable``/``stackTable``/``samples`` tables and a shared
string array.
"""

import json

from .collector import Collector, extract_lineno, filter_internal_frames
from .constants import THREAD_STATUS_MAIN_THREAD

__all__ = ('GeckoCollector',)


class _Strings:
    def __init__(self):
        self.array = []
        self._index = {}

    def intern(self, s):
        i = self._index.get(s)
        if i is None:
            i = self._index[s] = len(self.array)
            self.array.append(s)
        return i


class _ThreadData:
    def __init__(self, thread_id):
        self.thread_id = thread_id
        self.is_main = False
        # func key (name_id, file_id, line) -> func index
        self.funcs = {}
        self.func_rows = []       # (name_id, file_id, line)
        # frame key (func_index) -> frame index
        self.frames = {}
        self.frame_rows = []      # func_index
        # stack key (prefix_or_None, frame_index) -> stack index
        self.stacks = {}
        self.stack_rows = []      # (prefix, frame_index)
        self.samples = []         # stack index per sample


class GeckoCollector(Collector):
    def __init__(self, sample_interval_usec=1000, *, skip_idle=False, mode=None, opcodes=False, **kwargs):
        super().__init__(sample_interval_usec, skip_idle=skip_idle)
        self.mode = mode
        self.strings = _Strings()
        self.threads = {}
        self.sample_count = 0

    def _thread(self, thread_id):
        td = self.threads.get(thread_id)
        if td is None:
            td = self.threads[thread_id] = _ThreadData(thread_id)
        return td

    def collect(self, stack_frames, timestamps_us=None):
        self.sample_count += 1
        for interpreter in stack_frames or ():
            for thread in getattr(interpreter, "threads", ()) or ():
                if self.skip_idle:
                    from .collector import is_idle
                    if is_idle(thread):
                        continue
                frames = getattr(thread, "frame_info", None)
                if not frames:
                    continue
                frames = filter_internal_frames(frames)
                if not frames:
                    continue
                thread_id = getattr(thread, "thread_id", None)
                td = self._thread(thread_id)
                status = getattr(thread, "status", 0) or 0
                if status & THREAD_STATUS_MAIN_THREAD:
                    td.is_main = True
                # build a stack from outermost to innermost
                prefix = None
                for frame in reversed(frames):
                    name_id = self.strings.intern(frame[2])
                    file_id = self.strings.intern(frame[0])
                    line = extract_lineno(frame[1])
                    fkey = (name_id, file_id, line)
                    func_idx = td.funcs.get(fkey)
                    if func_idx is None:
                        func_idx = td.funcs[fkey] = len(td.func_rows)
                        td.func_rows.append(fkey)
                    frame_idx = td.frames.get(func_idx)
                    if frame_idx is None:
                        frame_idx = td.frames[func_idx] = len(td.frame_rows)
                        td.frame_rows.append(func_idx)
                    skey = (prefix, frame_idx)
                    stack_idx = td.stacks.get(skey)
                    if stack_idx is None:
                        stack_idx = td.stacks[skey] = len(td.stack_rows)
                        td.stack_rows.append((prefix, frame_idx))
                    prefix = stack_idx
                td.samples.append(prefix)

    def _build_profile(self):
        threads = []
        # one thread first, main thread preferred in position 0
        ordered = sorted(self.threads.values(), key=lambda t: (not t.is_main, t.thread_id or 0))
        for td in ordered:
            func_name = [r[0] for r in td.func_rows]
            func_file = [r[1] for r in td.func_rows]
            func_line = [r[2] for r in td.func_rows]
            n_func = len(td.func_rows)
            frame_func = list(td.frame_rows)
            n_frame = len(td.frame_rows)
            stack_frame = [r[1] for r in td.stack_rows]
            stack_prefix = [r[0] for r in td.stack_rows]
            n_stack = len(td.stack_rows)
            n_samples = len(td.samples)
            threads.append({
                "name": "Thread",
                "tid": td.thread_id,
                "isMainThread": td.is_main,
                "funcTable": {
                    "length": n_func,
                    "name": func_name,
                    "fileName": func_file,
                    "lineNumber": func_line,
                },
                "frameTable": {
                    "length": n_frame,
                    "func": frame_func,
                },
                "stackTable": {
                    "length": n_stack,
                    "frame": stack_frame,
                    "prefix": stack_prefix,
                },
                "samples": {
                    "length": n_samples,
                    "stack": list(td.samples),
                    "time": [i * (self.sample_interval_usec / 1000.0) for i in range(n_samples)],
                    "eventDelay": [0] * n_samples,
                },
                "markers": {
                    "length": 0,
                    "name": [],
                    "data": [],
                    "startTime": [],
                    "endTime": [],
                },
            })
        if not threads:
            threads.append({
                "name": "Thread",
                "tid": 0,
                "isMainThread": True,
                "funcTable": {"length": 0, "name": [], "fileName": [], "lineNumber": []},
                "frameTable": {"length": 0, "func": []},
                "stackTable": {"length": 0, "frame": [], "prefix": []},
                "samples": {"length": 0, "stack": [], "time": [], "eventDelay": []},
                "markers": {"length": 0, "name": [], "data": [], "startTime": [], "endTime": []},
            })
        return {
            "meta": {
                "interval": self.sample_interval_usec / 1000.0,
                "sampleUnit": {"time": "ms"},
                "startTime": 0,
                "profilingStartTime": 0,
                "product": "Tachyon Sampling Profiler",
                "version": 0,
            },
            "shared": {"stringArray": list(self.strings.array)},
            "threads": threads,
        }

    def export(self, filename):
        profile = self._build_profile()
        with open(filename, "w", encoding="utf-8", errors="surrogatepass") as f:
            json.dump(profile, f)
