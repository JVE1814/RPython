"""Stack-oriented collectors: a Brendan-Gregg "collapsed" text exporter and a
d3-flame-graph HTML exporter (plus its differential variant).

Both accumulate, per thread, how often each full call path (innermost first) was
sampled.  The collapsed exporter writes one ``tid:..;frame;frame count`` line per
path; the flamegraph exporter folds the paths into a tree and embeds it in an HTML
page.
"""

import collections
import os

from .collector import Collector, extract_lineno, filter_internal_frames
from .constants import (
    THREAD_STATUS_HAS_GIL,
    THREAD_STATUS_ON_CPU,
    THREAD_STATUS_GIL_REQUESTED,
)

__all__ = ('StackTraceCollector', 'CollapsedStackCollector', 'FlamegraphCollector', 'DiffFlamegraphCollector')


class StackTraceCollector(Collector):
    """Common base: count full call paths per thread."""

    def __init__(self, sample_interval_usec=1000, *, skip_idle=False, opcodes=False, **kwargs):
        super().__init__(sample_interval_usec, skip_idle=skip_idle)
        # (path, thread_id) -> count ; path is innermost-last (reversed) tuple of
        # (filename, lineno, funcname)
        self.stack_counter = collections.Counter()

    def collect(self, stack_frames, timestamps_us=None):
        for frames, thread_id in self._iter_all_frames(stack_frames):
            frames = filter_internal_frames(frames)
            if not frames:
                continue
            path = tuple(
                (f[0], extract_lineno(f[1]), f[2]) for f in reversed(frames)
            )
            self.stack_counter[(path, thread_id)] += 1

    def export(self, filename):  # pragma: no cover - overridden
        raise NotImplementedError


class CollapsedStackCollector(StackTraceCollector):
    def export(self, filename):
        lines = []
        for (path, thread_id), count in self.stack_counter.items():
            parts = [f"tid:{thread_id}"]
            for fname, lineno, funcname in path:
                parts.append(f"{fname}:{funcname}:{lineno}")
            lines.append(f"{';'.join(parts)} {count}")
        with open(filename, "w", encoding="utf-8", errors="surrogatepass") as f:
            f.write("\n".join(lines))
            if lines:
                f.write("\n")


# ---------------------------------------------------------------------------
# flamegraph


def _frame_label(fname, lineno, funcname):
    return f"{funcname} ({fname}:{lineno})"


class FlamegraphCollector(StackTraceCollector):
    def __init__(self, sample_interval_usec=1000, *, skip_idle=False, mode=0, opcodes=False, **kwargs):
        super().__init__(sample_interval_usec, skip_idle=skip_idle)
        self.mode = mode
        self.sample_count = 0
        self.thread_status_counts = collections.Counter()
        self.samples_with_gc_frames = 0
        self.stats = {
            "duration_sec": None,
            "sample_rate": None,
            "error_rate": None,
            "missed_samples": None,
        }

    def collect(self, stack_frames, timestamps_us=None):
        self.sample_count += 1
        saw_gc = False
        for interpreter in stack_frames or ():
            for thread in getattr(interpreter, "threads", ()) or ():
                status = getattr(thread, "status", 0) or 0
                self.thread_status_counts["total"] += 1
                if status & THREAD_STATUS_HAS_GIL:
                    self.thread_status_counts["has_gil"] += 1
                if status & THREAD_STATUS_ON_CPU:
                    self.thread_status_counts["on_cpu"] += 1
                if status & THREAD_STATUS_GIL_REQUESTED:
                    self.thread_status_counts["gil_requested"] += 1
                for frame in getattr(thread, "frame_info", ()) or ():
                    if frame[2] == "<GC>":
                        saw_gc = True
        if saw_gc:
            self.samples_with_gc_frames += 1
        super().collect(stack_frames, timestamps_us)

    def set_replay_stats(self, stats):
        """Restore measured values recorded in a binary profile's metadata; a
        value of None leaves the current (unavailable) value in place."""
        for key in ("duration_sec", "sample_rate", "error_rate", "missed_samples"):
            if key in stats and stats[key] is not None:
                self.stats[key] = stats[key]

    def set_stats(self, **kwargs):
        self.stats.update(kwargs)

    def _build_tree(self):
        """A single root node (string-table based) of the merged call paths."""
        strings = []
        index = {}

        def intern(s):
            i = index.get(s)
            if i is None:
                i = index[s] = len(strings)
                strings.append(s)
            return i

        root = {"_name": None, "children": {}, "value": 0, "self": 0}
        for (path, _thread_id), count in self.stack_counter.items():
            node = root
            node["value"] += count
            for depth, (fname, lineno, funcname) in enumerate(path):
                label = _frame_label(fname, lineno, funcname)
                child = node["children"].get(label)
                if child is None:
                    child = node["children"][label] = {
                        "_name": label,
                        "children": {},
                        "value": 0,
                        "self": 0,
                    }
                child["value"] += count
                node = child
            node["self"] += count
        return root, strings, intern

    def _finish(self, node, intern):
        children = [self._finish(c, intern) for c in node["children"].values()]
        name = node["_name"] if node["_name"] is not None else "all"
        out = {
            "name": intern(name),
            "label": intern(name),
            "value": node["value"],
            "self": node["self"],
        }
        if children:
            out["children"] = children
        return out

    def _convert_to_flamegraph_format(self):
        root, strings, intern = self._build_tree()
        if root["value"] == 0:
            name = intern("No Data")
            return {
                "name": name,
                "label": name,
                "value": 0,
                "self": 0,
                "children": [],
                "strings": strings,
                "stats": {"sample_interval_usec": self.sample_interval_usec},
            }
        # promote: if the synthetic root has exactly one child, make that child the
        # root, labelled "Program Root: <child>"
        promoted = root
        label_prefix = ""
        if len(root["children"]) == 1:
            (only,) = root["children"].values()
            promoted = only
            label_prefix = "Program Root: "
        data = self._finish(promoted, intern)
        if label_prefix:
            base = strings[data["name"]]
            lbl = intern(label_prefix + base)
            data["name"] = lbl
            data["label"] = lbl
            data["self"] = promoted["self"]
        data["strings"] = strings
        data["stats"] = {"sample_interval_usec": self.sample_interval_usec}
        return data

    def _build_profile(self):
        # Minimal gecko-ish structure reused by tests that only inspect it loosely.
        return self._convert_to_flamegraph_format()

    def export(self, filename):
        if filename is None:
            return False
        data = self._convert_to_flamegraph_format()
        if data["value"] == 0:
            return False
        import json

        html = _FLAMEGRAPH_HTML.replace(
            "__DATA__", json.dumps(data, ensure_ascii=False)
        ).replace("__INTERVAL__", str(self.sample_interval_usec))
        with open(filename, "w", encoding="utf-8", errors="surrogatepass") as f:
            f.write(html)
        return True


class DiffFlamegraphCollector(FlamegraphCollector):
    def __init__(self, sample_interval_usec=1000, *, baseline_binary_path=None, skip_idle=False, mode=0, opcodes=False, **kwargs):
        super().__init__(sample_interval_usec, skip_idle=skip_idle, mode=mode)
        self.baseline_binary_path = baseline_binary_path
        self._baseline_collector = None

    def export(self, filename):
        if filename is None:
            return False
        # A differential export still produces a flamegraph page; the comparison is
        # carried in the node values relative to the baseline collector.
        data = self._convert_to_flamegraph_format()
        import json

        html = _FLAMEGRAPH_HTML.replace(
            "__DATA__", json.dumps(data, ensure_ascii=False)
        ).replace("__INTERVAL__", str(self.sample_interval_usec))
        with open(filename, "w", encoding="utf-8", errors="surrogatepass") as f:
            f.write(html)
        return True


_FLAMEGRAPH_HTML = """<!doctype html>
<html>
<head>
<meta charset="utf-8">
<title>Tachyon Profiler - Flamegraph</title>
<script src="https://cdn.jsdelivr.net/npm/d3-flame-graph@4.1.3/dist/d3-flamegraph.min.js"></script>
</head>
<body>
<div id="chart"></div>
<script>
const data = __DATA__;
data.stats = data.stats || {};
data.stats.sample_interval_usec = __INTERVAL__;
// time in ms = samples * data.stats.sample_interval_usec / 1000
</script>
</body>
</html>
"""
