"""JSONL (one JSON object per line) profile exporter.

The stream is: a ``meta`` record, then interleaved ``string_table`` and
``frame_table`` records defining ids, an ``agg`` record with per-frame self and
cumulative sample counts, and an ``end`` record.  Strings and frames are
interned in first-seen order.  Large profiles are split into several chunks.
"""

from .collector import Collector, extract_lineno, filter_internal_frames

__all__ = ('JsonlCollector',)

_FRAMES_PER_CHUNK = 4096


class JsonlCollector(Collector):
    def __init__(self, sample_interval_usec=1000, *, skip_idle=False, mode=None, opcodes=False, **kwargs):
        super().__init__(sample_interval_usec, skip_idle=skip_idle)
        self.mode = mode
        self.run_id = "run"
        self._strings = []
        self._string_index = {}
        self._frames = []           # frame_id -> (path_str_id, func_str_id, line, end_line)
        self._frame_index = {}
        self._self = {}             # frame_id -> count
        self._cumulative = {}       # frame_id -> count
        self.samples_total = 0

    def _intern_str(self, s):
        i = self._string_index.get(s)
        if i is None:
            i = self._string_index[s] = len(self._strings)
            self._strings.append(s)
        return i

    def _intern_frame(self, frame):
        funcname = frame[2]
        filename = frame[0]
        loc = frame[1]
        line = extract_lineno(loc)
        end_line = line
        if loc is not None and not isinstance(loc, int):
            end_line = loc[1] if loc[1] is not None else line
        func_id = self._intern_str(funcname)
        path_id = self._intern_str(filename)
        key = (path_id, func_id, line, end_line)
        fid = self._frame_index.get(key)
        if fid is None:
            fid = self._frame_index[key] = len(self._frames)
            self._frames.append(key)
        return fid

    def collect(self, stack_frames, timestamps_us=None):
        self.samples_total += 1
        for frames, _thread_id in self._iter_all_frames(stack_frames):
            frames = filter_internal_frames(frames)
            if not frames:
                continue
            seen = set()
            for depth, frame in enumerate(frames):
                fid = self._intern_frame(frame)
                if depth == 0:
                    self._self[fid] = self._self.get(fid, 0) + 1
                if fid not in seen:
                    self._cumulative[fid] = self._cumulative.get(fid, 0) + 1
                    seen.add(fid)

    def _meta_record(self):
        parts = [
            '{"type":"meta","v":0,"run_id":', _json_str(self.run_id),
            ',"sample_interval_usec":', str(self.sample_interval_usec),
        ]
        if self.mode is not None:
            parts.append(',"mode":')
            parts.append(str(self.mode))
        parts.append('}')
        return ''.join(parts)

    def export(self, filename):
        lines = [self._meta_record()]
        # string_table (chunked)
        for start in range(0, len(self._strings), _FRAMES_PER_CHUNK):
            chunk = self._strings[start:start + _FRAMES_PER_CHUNK]
            items = ','.join(
                '{"str_id":%d,"value":%s}' % (start + i, _json_str(s))
                for i, s in enumerate(chunk)
            )
            lines.append(
                '{"type":"string_table","v":0,"run_id":%s,"strings":[%s]}'
                % (_json_str(self.run_id), items)
            )
        if not self._strings:
            lines.append(
                '{"type":"string_table","v":0,"run_id":%s,"strings":[]}'
                % _json_str(self.run_id)
            )
        # frame_table (chunked)
        for start in range(0, len(self._frames), _FRAMES_PER_CHUNK):
            chunk = self._frames[start:start + _FRAMES_PER_CHUNK]
            items = ','.join(
                '{"frame_id":%d,"path_str_id":%d,"func_str_id":%d,"line":%d,"end_line":%d}'
                % (start + i, path_id, func_id, line, end_line)
                for i, (path_id, func_id, line, end_line) in enumerate(chunk)
            )
            lines.append(
                '{"type":"frame_table","v":0,"run_id":%s,"frames":[%s]}'
                % (_json_str(self.run_id), items)
            )
        if not self._frames:
            lines.append(
                '{"type":"frame_table","v":0,"run_id":%s,"frames":[]}'
                % _json_str(self.run_id)
            )
        # agg
        entries = ','.join(
            '{"frame_id":%d,"self":%d,"cumulative":%d}'
            % (fid, self._self.get(fid, 0), self._cumulative.get(fid, 0))
            for fid in range(len(self._frames))
        )
        lines.append(
            '{"type":"agg","v":0,"run_id":%s,"kind":"frame","scope":"final","samples_total":%d,"entries":[%s]}'
            % (_json_str(self.run_id), self.samples_total, entries)
        )
        # end
        lines.append(
            '{"type":"end","v":0,"run_id":%s,"samples_total":%d}'
            % (_json_str(self.run_id), self.samples_total)
        )
        with open(filename, "w", encoding="utf-8", errors="surrogatepass") as f:
            for line in lines:
                f.write(line)
                f.write("\n")


def _json_str(s):
    import json
    return json.dumps(s, ensure_ascii=False, separators=(",", ":"))
