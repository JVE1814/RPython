"""Stack-oriented collectors.

* ``CollapsedStackCollector`` writes the "collapsed stacks" text format of
  flame graph tools: one ``tid:N;frame;frame;... count`` line per distinct
  call path.
* ``FlamegraphCollector`` folds the call paths into a tree and writes an
  interactive HTML flame graph.  Each tree node knows its total and self
  samples per thread and the bytecode instructions seen in it.
* ``DiffFlamegraphCollector`` compares the current profile with a baseline
  recorded with ``--binary``: every node gets the baseline's time for the
  same function path, and the call paths only the baseline has are shown
  in a separate "elided" tree.
"""

import collections
import json
import os
import sys

from .collector import (
    Collector,
    extract_lineno,
    filter_internal_frames,
    is_idle,
)
from .constants import (
    THREAD_STATUS_GIL_REQUESTED,
    THREAD_STATUS_HAS_GIL,
    THREAD_STATUS_ON_CPU,
    THREAD_STATUS_UNKNOWN,
)

__all__ = ('StackTraceCollector', 'CollapsedStackCollector',
           'FlamegraphCollector', 'DiffFlamegraphCollector')

# A node is drawn if, for at least one thread, it holds this share of the
# thread's samples (and at least one sample).
_SIGNIFICANCE = 0.001

_SYNTHETIC_FILES = ("~", "")


def _is_async_sample(stack_frames):
    return bool(stack_frames) and hasattr(stack_frames[0], "awaited_by")


def _frame_key(frame):
    return (frame[0], extract_lineno(frame[1]), frame[2])


def _frame_opcode(frame):
    try:
        return frame[3]
    except (IndexError, TypeError):
        return None


class StackTraceCollector(Collector):
    """Common base: receives every thread's frame list with its weight."""

    aggregating = True

    def __init__(self, sample_interval_usec=1000, *, skip_idle=False,
                 opcodes=False, **kwargs):
        super().__init__(sample_interval_usec, skip_idle=skip_idle)
        self.opcodes_enabled = bool(opcodes)

    def collect(self, stack_frames, timestamps_us=None):
        weight = len(timestamps_us) if timestamps_us else 1
        if _is_async_sample(stack_frames):
            for frames, thread_id, _task in self._iter_async_frames(stack_frames):
                if frames:
                    self.process_frames(frames, thread_id, weight)
            return
        for frames, thread_id in self._iter_all_frames(stack_frames):
            frames = filter_internal_frames(frames)
            if frames:
                self.process_frames(frames, thread_id, weight)

    def process_frames(self, frames, thread_id=None, weight=1):
        raise NotImplementedError


class CollapsedStackCollector(StackTraceCollector):
    def __init__(self, sample_interval_usec=1000, *, skip_idle=False, **kwargs):
        super().__init__(sample_interval_usec, skip_idle=skip_idle, **kwargs)
        # (path, thread id) -> samples; a path runs from the outermost frame
        # to the innermost one, each frame as (filename, lineno, funcname)
        self.stack_counter = collections.Counter()

    def process_frames(self, frames, thread_id=None, weight=1):
        path = tuple(_frame_key(f) for f in reversed(frames))
        self.stack_counter[(path, thread_id)] += weight

    def export(self, filename):
        lines = []
        for (path, thread_id), count in self.stack_counter.items():
            parts = [f"tid:{thread_id}"]
            for fname, lineno, funcname in path:
                if fname in _SYNTHETIC_FILES:
                    parts.append(funcname)
                else:
                    parts.append(f"{fname}:{funcname}:{lineno}")
            lines.append(f"{';'.join(parts)} {count}")
        with open(filename, "w", encoding="utf-8", errors="surrogatepass") as f:
            for line in lines:
                f.write(line)
                f.write("\n")
        return True


# ---------------------------------------------------------------- the tree

def _new_node():
    return {
        "samples": 0,
        "self": 0,
        "children": {},
        "threads": {},          # tid -> [samples, self]
        "opcodes": {},          # opcode -> samples
        "thread_opcodes": {},   # tid -> {opcode: samples}
    }


def _label(key):
    filename, lineno, funcname = key
    if filename in _SYNTHETIC_FILES:
        return str(funcname)
    return f"{funcname} ({filename}:{lineno})"


class _Strings:
    def __init__(self):
        self.array = []
        self._ids = {}

    def __call__(self, text):
        text = str(text)
        idx = self._ids.get(text)
        if idx is None:
            idx = self._ids[text] = len(self.array)
            self.array.append(text)
        return idx


def _json_dumps(obj):
    """``json.dumps(obj)`` without recursion (flame graph trees can be much
    deeper than the recursion limit)."""
    out = []
    stack = [obj]
    while stack:
        item = stack.pop()
        if isinstance(item, _Raw):
            out.append(item.text)
        elif isinstance(item, dict):
            out.append("{")
            pending = []
            for i, (key, value) in enumerate(item.items()):
                if isinstance(key, str):
                    key_text = json.dumps(key)
                elif isinstance(key, bool) or key is None:
                    key_text = json.dumps(json.dumps(key))
                else:
                    key_text = json.dumps(str(key))
                pending.append(_Raw((", " if i else "") + key_text + ": "))
                pending.append(value)
            pending.append(_Raw("}"))
            stack.extend(reversed(pending))
        elif isinstance(item, (list, tuple)):
            out.append("[")
            pending = []
            for i, value in enumerate(item):
                if i:
                    pending.append(_Raw(", "))
                pending.append(value)
            pending.append(_Raw("]"))
            stack.extend(reversed(pending))
        else:
            out.append(json.dumps(item))
    return "".join(out)


class _Raw:
    __slots__ = ("text",)

    def __init__(self, text):
        self.text = text


def _pct(part, whole):
    return 100.0 * part / whole if whole else 0.0


class FlamegraphCollector(StackTraceCollector):
    def __init__(self, sample_interval_usec=1000, *, skip_idle=False,
                 mode=None, opcodes=False, **kwargs):
        super().__init__(sample_interval_usec, skip_idle=skip_idle,
                         opcodes=opcodes)
        self.mode = mode
        self._root = _new_node()
        self._total_samples = 0     # thread stacks recorded (weighted)
        self._sample_count = 0      # samples taken (weighted)
        self._all_threads = set()
        self.thread_status_counts = {
            "has_gil": 0, "on_cpu": 0, "gil_requested": 0, "unknown": 0,
            "total": 0,
        }
        self.samples_with_gc_frames = 0
        self.per_thread_stats = {}
        self.stats = {
            "sample_interval_usec": sample_interval_usec,
            "duration_sec": None,
            "sample_rate": None,
            "error_rate": None,
            "missed_samples": None,
            "mode": mode,
        }

    # -- collecting

    def _count_status(self, thread_id, status, has_gc, weight):
        counts = self.thread_status_counts
        per = self.per_thread_stats.get(thread_id)
        if per is None:
            per = self.per_thread_stats[thread_id] = {
                "has_gil": 0, "on_cpu": 0, "gil_requested": 0, "unknown": 0,
                "total": 0, "gc_samples": 0,
            }
        for name, flag in (("has_gil", THREAD_STATUS_HAS_GIL),
                           ("on_cpu", THREAD_STATUS_ON_CPU),
                           ("gil_requested", THREAD_STATUS_GIL_REQUESTED),
                           ("unknown", THREAD_STATUS_UNKNOWN)):
            if status & flag:
                counts[name] += weight
                per[name] += weight
        counts["total"] += weight
        per["total"] += weight
        if has_gc:
            per["gc_samples"] += weight

    def collect(self, stack_frames, timestamps_us=None):
        weight = len(timestamps_us) if timestamps_us else 1
        self._sample_count += weight
        if _is_async_sample(stack_frames):
            for frames, thread_id, _task in self._iter_async_frames(stack_frames):
                if frames:
                    self.process_frames(frames, thread_id, weight)
            return
        sample_has_gc = False
        for interpreter in stack_frames or ():
            for thread in getattr(interpreter, "threads", ()) or ():
                thread_id = getattr(thread, "thread_id", None)
                status = getattr(thread, "status", 0) or 0
                frames = filter_internal_frames(getattr(thread, "frame_info", None))
                has_gc = any(f[2] == "<GC>" for f in frames)
                sample_has_gc = sample_has_gc or has_gc
                self._count_status(thread_id, status, has_gc, weight)
                if self.skip_idle and is_idle(thread):
                    continue
                if frames:
                    self.process_frames(frames, thread_id, weight)
        if sample_has_gc:
            self.samples_with_gc_frames += weight

    def process_frames(self, frames, thread_id=None, weight=1):
        self._total_samples += weight
        self._all_threads.add(thread_id)
        node = self._root
        node["samples"] += weight
        node["threads"].setdefault(thread_id, [0, 0])[0] += weight
        for frame in reversed(frames):
            key = _frame_key(frame)
            child = node["children"].get(key)
            if child is None:
                child = node["children"][key] = _new_node()
            child["samples"] += weight
            child["threads"].setdefault(thread_id, [0, 0])[0] += weight
            opcode = _frame_opcode(frame)
            if opcode is not None:
                child["opcodes"][opcode] = child["opcodes"].get(opcode, 0) + weight
                per = child["thread_opcodes"].setdefault(thread_id, {})
                per[opcode] = per.get(opcode, 0) + weight
            node = child
        node["self"] += weight
        node["threads"][thread_id][1] += weight

    # -- statistics

    def set_stats(self, sample_interval_usec=None, duration_sec=None,
                  sample_rate=None, error_rate=None, missed_samples=None,
                  mode=None, **kwargs):
        if sample_interval_usec is not None:
            self.stats["sample_interval_usec"] = sample_interval_usec
        for key, value in (("duration_sec", duration_sec),
                           ("sample_rate", sample_rate),
                           ("error_rate", error_rate),
                           ("missed_samples", missed_samples),
                           ("mode", mode)):
            if value is not None:
                self.stats[key] = value

    def set_replay_stats(self, stats):
        """Take the measured statistics stored with a binary recording."""
        for key in ("duration_sec", "sample_rate", "error_rate", "missed_samples"):
            value = stats.get(key)
            if value is not None:
                self.stats[key] = value

    def _stats_payload(self):
        stats = dict(self.stats)
        counts = self.thread_status_counts
        total = counts["total"]
        stats["thread_stats"] = {
            "has_gil_pct": _pct(counts["has_gil"], total),
            "on_cpu_pct": _pct(counts["on_cpu"], total),
            "gil_requested_pct": _pct(counts["gil_requested"], total),
            "unknown_pct": _pct(counts["unknown"], total),
            "gc_pct": _pct(self.samples_with_gc_frames, self._sample_count),
            "total": total,
        }
        per_thread = {}
        for tid, per in self.per_thread_stats.items():
            per_thread[tid] = {
                "has_gil_pct": _pct(per["has_gil"], per["total"]),
                "on_cpu_pct": _pct(per["on_cpu"], per["total"]),
                "gil_requested_pct": _pct(per["gil_requested"], per["total"]),
                "unknown_pct": _pct(per["unknown"], per["total"]),
                "gc_pct": _pct(per["gc_samples"], self._sample_count),
                "total": per["total"],
            }
        stats["per_thread_stats"] = per_thread
        return stats

    # -- conversion

    def _decorate(self, out, node, fpath):
        """Hook: add fields to the output node of tree node *node*."""

    def _significant(self, node, totals):
        for tid, (samples, _self) in node["threads"].items():
            if samples >= max(1, _SIGNIFICANCE * totals.get(tid, 0)):
                return True
        return False

    def _output_node(self, key, node, strings, fpath):
        label = _label(key)
        out = {
            "name": strings(label),
            "label": strings(label),
            "value": node["samples"],
            "self": node["self"],
            "filename": strings(key[0]),
            "lineno": key[1],
            "funcname": strings(key[2]),
            "threads": sorted(node["threads"], key=str),
            "thread_values": {tid: list(v) for tid, v in node["threads"].items()},
            "children": [],
        }
        if node["opcodes"]:
            out["opcodes"] = dict(node["opcodes"])
            out["thread_opcodes"] = {tid: dict(ops) for tid, ops in
                                     node["thread_opcodes"].items()}
        self._decorate(out, node, fpath)
        return out

    def _build_tree(self, root, strings, keep):
        """Output tree of *root* (iteratively); *keep(node)* filters nodes."""
        out_root = {"children": []}
        work = [(root, out_root, ())]
        while work:
            node, out, fpath = work.pop()
            children = sorted(node["children"].items(),
                              key=lambda kv: kv[1]["samples"], reverse=True)
            for key, child in children:
                if not keep(child):
                    continue
                child_path = fpath + ((key[0], key[2]),)
                child_out = self._output_node(key, child, strings, child_path)
                out["children"].append(child_out)
                work.append((child, child_out, child_path))
        return out_root["children"]

    def _convert_to_flamegraph_format(self):
        strings = _Strings()
        root = self._root
        totals = {tid: v[0] for tid, v in root["threads"].items()}
        if self._total_samples == 0:
            name = strings("No Data")
            return {"name": name, "label": name, "value": 0, "self": 0,
                    "children": [], "threads": [], "thread_values": {},
                    "strings": strings.array, "stats": self._stats_payload()}
        children = self._build_tree(
            root, strings, lambda node: self._significant(node, totals))
        if len(children) == 1 and children[0]["value"] == root["samples"]:
            data = children[0]
            label = "Program Root: " + strings.array[data["label"]]
            data["name"] = data["label"] = strings(label)
        else:
            name = "Program Root" if children else "No significant data"
            data = {
                "name": strings(name),
                "label": strings(name),
                "value": root["samples"],
                "self": 0,
                "threads": sorted(root["threads"], key=str),
                "thread_values": {tid: [v[0], 0] for tid, v in root["threads"].items()},
                "children": children,
            }
            self._decorate(data, None, ())
        data["strings"] = strings.array
        data["stats"] = self._stats_payload()
        return data

    # -- export

    _TITLE = "Tachyon Profiler - Flamegraph"

    def _html(self, data):
        payload = _json_dumps(data).replace("</", "<\\/")
        return (_HTML_TEMPLATE
                .replace("__TITLE__", self._TITLE)
                .replace("__LEGEND__", self._legend_html())
                .replace("__DATA__", payload))

    def _legend_html(self):
        return ""

    def export(self, filename):
        if self._total_samples == 0:
            print("No samples were collected; the flame graph was not written.",
                  file=sys.stderr)
            return False
        data = self._convert_to_flamegraph_format()
        html = self._html(data)
        with open(filename, "w", encoding="utf-8", errors="surrogatepass") as f:
            f.write(html)
        print(f"Flamegraph saved to: {filename}")
        return True


class DiffFlamegraphCollector(FlamegraphCollector):
    """Flame graph of the current profile annotated against a baseline."""

    _TITLE = "Tachyon Profiler - Differential Flamegraph"

    def __init__(self, sample_interval_usec=1000, *, baseline_binary_path=None,
                 skip_idle=False, mode=None, capture_config=None, opcodes=False,
                 **kwargs):
        super().__init__(sample_interval_usec, skip_idle=skip_idle, mode=mode,
                         opcodes=opcodes)
        if baseline_binary_path is None:
            raise ValueError("a baseline profile (--diff-flamegraph FILE) is required")
        if not os.path.exists(baseline_binary_path):
            raise FileNotFoundError(f"baseline profile not found: {baseline_binary_path}")
        self.baseline_binary_path = baseline_binary_path
        self.capture_config = capture_config
        self._baseline_collector = None
        self._diff_state = None

    def _load_baseline(self):
        from .binary_reader import BinaryReader
        with BinaryReader(self.baseline_binary_path) as reader:
            info = reader.get_info()
            base_mode = info.get("mode")
            if (base_mode is not None and self.mode is not None
                    and base_mode != self.mode):
                raise ValueError(
                    f"the baseline was recorded with profiling mode {base_mode}, "
                    f"the current profile with profiling mode {self.mode}")
            base_config = info.get("capture_config")
            if base_config and self.capture_config:
                for key, value in self.capture_config.items():
                    if key in base_config and bool(base_config[key]) != bool(value):
                        raise ValueError(
                            f"the baseline was recorded with {key}={base_config[key]}, "
                            f"the current profile with {key}={value}")
            baseline = FlamegraphCollector(info["sample_interval_us"],
                                           mode=base_mode)
            reader.replay_samples(baseline)
        self._baseline_collector = baseline

    # -- function paths

    @staticmethod
    def _function_paths(root):
        """{function path: [self, total, opcodes]} of a tree (line numbers
        merged) and {function path: summed samples of its line nodes}."""
        merged = {}
        work = [(root, ())]
        while work:
            node, fpath = work.pop()
            for key, child in node["children"].items():
                child_path = fpath + ((key[0], key[2]),)
                entry = merged.get(child_path)
                if entry is None:
                    entry = merged[child_path] = [0, 0, {}]
                entry[0] += child["self"]
                entry[1] += child["samples"]
                for op, n in child["opcodes"].items():
                    entry[2][op] = entry[2].get(op, 0) + n
                work.append((child, child_path))
        return merged

    def _decorate(self, out, node, fpath):
        state = self._diff_state
        if state is None:
            return
        scale, base_paths, cur_paths = state
        if node is None:
            # the synthetic root
            base_total = self._baseline_collector._total_samples * scale
            out.update({"self_time": 0, "baseline": 0.0,
                        "baseline_total": base_total, "diff": 0.0,
                        "diff_pct": 0.0})
            return
        current_self = node["self"]
        entry = base_paths.get(fpath)
        if entry is None:
            baseline = baseline_total = 0.0
        else:
            cur_total = cur_paths[fpath][1]
            share = node["samples"] / cur_total if cur_total else 0.0
            baseline = entry[0] * scale * share
            baseline_total = entry[1] * scale * share
        diff = current_self - baseline
        if baseline > 0:
            diff_pct = 100.0 * diff / baseline
        else:
            diff_pct = 100.0 if current_self > 0 else 0.0
        out.update({"self_time": current_self, "baseline": baseline,
                    "baseline_total": baseline_total, "diff": diff,
                    "diff_pct": diff_pct})

    def _elided_tree(self, base_root, cur_paths, scale):
        """The baseline call paths missing from the current profile, with
        their ancestors (which carry only the inclusive time of the missing
        parts).  Returns (tree, number of missing stack endings)."""
        strings = _Strings()
        # post-order: which nodes have missing paths below them, and how much
        order = []
        work = [(base_root, None, ())]
        while work:
            node, key, fpath = work.pop()
            order.append((node, key, fpath))
            for child_key, child in node["children"].items():
                work.append((child, child_key, fpath + ((child_key[0], child_key[2]),)))
        elided_value = {}
        count = 0
        for node, key, fpath in reversed(order):
            if key is None:
                continue
            if fpath not in cur_paths:
                elided_value[id(node)] = node["samples"] * scale
                if node["self"] > 0:
                    count += 1
            else:
                below = sum(elided_value.get(id(c), 0.0)
                            for c in node["children"].values())
                if below > 0:
                    elided_value[id(node)] = below

        def make(key, node, fpath):
            matched = fpath in cur_paths
            label = _label(key)
            value = elided_value[id(node)]
            baseline = 0.0 if matched else node["self"] * scale
            out = {
                "name": strings(label),
                "label": strings(label),
                "value": value,
                "self": 0 if matched else node["self"] * scale,
                "filename": strings(key[0]),
                "lineno": key[1],
                "funcname": strings(key[2]),
                "threads": sorted(node["threads"], key=str),
                "thread_values": {tid: [v[0] * scale, v[1] * scale]
                                  for tid, v in node["threads"].items()},
                "self_time": 0,
                "baseline": baseline,
                "baseline_total": value,
                "diff": -baseline,
                "diff_pct": -100.0 if baseline > 0 else 0.0,
                "children": [],
            }
            if not matched and node["opcodes"]:
                out["opcodes"] = dict(node["opcodes"])
            out["_matched"] = matched
            return out

        top = {"children": []}
        work = [(base_root, top, ())]
        while work:
            node, out, fpath = work.pop()
            for key, child in sorted(node["children"].items(),
                                     key=lambda kv: kv[1]["samples"], reverse=True):
                if id(child) not in elided_value:
                    continue
                child_path = fpath + ((key[0], key[2]),)
                child_out = make(key, child, child_path)
                out["children"].append(child_out)
                work.append((child, child_out, child_path))
        children = top["children"]
        if len(children) == 1 and children[0]["_matched"]:
            tree = children[0]
            label = "Program Root: " + strings.array[tree["label"]]
            tree["name"] = tree["label"] = strings(label)
        else:
            total = sum(c["value"] for c in children)
            name = strings("Program Root")
            tree = {"name": name, "label": name, "value": total, "self": 0,
                    "self_time": 0, "baseline": total, "baseline_total": total,
                    "diff": -total, "diff_pct": -100.0 if total > 0 else 0.0,
                    "threads": [], "thread_values": {}, "children": children}
        # drop the bookkeeping flag
        work = [tree]
        while work:
            item = work.pop()
            item.pop("_matched", None)
            work.extend(item["children"])
        tree["strings"] = strings.array
        return tree, count

    def _convert_to_flamegraph_format(self):
        if self._baseline_collector is None:
            self._load_baseline()
        base = self._baseline_collector
        if base._total_samples:
            scale = base.sample_interval_usec / self.sample_interval_usec
        else:
            scale = 1.0
        base_paths = self._function_paths(base._root)
        cur_paths = self._function_paths(self._root)
        self._diff_state = (scale, base_paths, cur_paths)
        try:
            data = super()._convert_to_flamegraph_format()
        finally:
            self._diff_state = None
        stats = data["stats"]
        stats["is_differential"] = True
        stats["baseline_samples"] = base._total_samples
        stats["current_samples"] = self._total_samples
        stats["baseline_scale"] = scale
        elided, count = self._elided_tree(base._root, cur_paths, scale)
        stats["elided_count"] = count
        if count:
            elided_stats = base._stats_payload()
            elided_stats.update({
                "is_differential": True,
                "baseline_samples": base._total_samples,
                "current_samples": self._total_samples,
                "baseline_scale": scale,
            })
            elided["stats"] = elided_stats
            stats["elided_flamegraph"] = elided
        return data

    def export(self, filename):
        if self._baseline_collector is None:
            self._load_baseline()
        if self._total_samples == 0 and self._baseline_collector._total_samples == 0:
            print("No samples were collected; the flame graph was not written.",
                  file=sys.stderr)
            return False
        data = self._convert_to_flamegraph_format()
        with open(filename, "w", encoding="utf-8", errors="surrogatepass") as f:
            f.write(self._html(data))
        print(f"Differential flamegraph saved to: {filename}")
        return True

    def _legend_html(self):
        return _DIFF_LEGEND


_DIFF_LEGEND = """
<section id="diff-legend-section" class="legend">
  <h2>Differential Colors</h2>
  <p><span class="swatch slower"></span> more time than the baseline
     <span class="swatch faster"></span> less time than the baseline
     <span class="swatch same"></span> unchanged</p>
</section>
"""

_HTML_TEMPLATE = """<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>__TITLE__</title>
<style>
body { font-family: system-ui, sans-serif; margin: 0; padding: 1em; background: #fafafa; }
h1 { font-size: 1.3em; margin: 0 0 .5em; }
#stats { color: #444; margin-bottom: 1em; font-size: .9em; }
#chart { position: relative; width: 100%; }
.frame { position: absolute; height: 18px; box-sizing: border-box; overflow: hidden;
         white-space: nowrap; font-size: 12px; line-height: 18px; padding: 0 3px;
         border: 1px solid #fff; cursor: pointer; }
.swatch { display: inline-block; width: 1em; height: 1em; vertical-align: middle; }
.slower { background: #e05050; } .faster { background: #5070e0; } .same { background: #ccc; }
</style>
</head>
<body>
<h1>__TITLE__</h1>
<div id="stats"></div>
__LEGEND__
<!-- the data has the hierarchical shape d3-flame-graph uses: name, value, children -->
<div id="chart" class="d3-flame-graph"></div>
<script>
const data = __DATA__;
const strings = data.strings || [];
function text(index) { return typeof index === "number" ? strings[index] : String(index); }
function ms(samples) { return samples * data.stats.sample_interval_usec / 1000; }
function color(node) {
  if (data.stats.is_differential && node.diff !== undefined) {
    if (node.diff > 0) return "#e05050";
    if (node.diff < 0) return "#5070e0";
    return "#cccccc";
  }
  const h = (text(node.funcname || node.name).length * 37) % 60;
  return "hsl(" + (20 + h) + ", 85%, 60%)";
}
function render(root) {
  const chart = document.getElementById("chart");
  chart.innerHTML = "";
  const width = chart.clientWidth || 1000;
  const total = root.value || 1;
  const work = [[root, 0, 0, width]];
  let depth = 0;
  while (work.length) {
    const [node, level, x, w] = work.pop();
    depth = Math.max(depth, level);
    if (w < 1) continue;
    const div = document.createElement("div");
    div.className = "frame";
    div.style.left = x + "px";
    div.style.top = (level * 18) + "px";
    div.style.width = w + "px";
    div.style.background = color(node);
    const label = text(node.name);
    div.textContent = label;
    div.title = label + "\\n" + node.value + " samples (" + ms(node.value).toFixed(2) + " ms)";
    div.onclick = () => render(node);
    chart.appendChild(div);
    let cx = x;
    for (const child of node.children || []) {
      const cw = w * child.value / (node.value || 1);
      work.push([child, level + 1, cx, cw]);
      cx += cw;
    }
  }
  chart.style.height = ((depth + 1) * 18) + "px";
}
const s = data.stats;
document.getElementById("stats").textContent =
  data.value + " samples, interval " + s.sample_interval_usec + " us" +
  (s.duration_sec != null ? ", duration " + s.duration_sec + " s" : "");
render(data);
</script>
</body>
</html>
"""
