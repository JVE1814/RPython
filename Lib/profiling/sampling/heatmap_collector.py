"""Line-level heat map of a sampling profile, exported as HTML pages.

``HeatmapCollector`` counts, for every source line, how many samples had
it on the stack (and how many had it at the top), and records which line
called which.  ``export()`` writes a directory with an index page and one
page per source file where each line is shaded by its sample count and
has buttons leading to its callers and callees.
"""

import collections
import html
import json
import os
import platform
import stat
import sys
import sysconfig
from pathlib import Path

from .collector import Collector, extract_lineno

__all__ = ('HeatmapCollector', 'get_python_path_info', 'extract_module_name')


# ---------------------------------------------------------------------------
# where a file comes from

def get_python_path_info():
    """Directories used to classify source files: the standard library,
    the site-packages directories and sys.path (as Path objects)."""
    stdlib = None
    try:
        location = sysconfig.get_path('stdlib')
    except (KeyError, OSError, ValueError):
        location = None
    if not location:
        import os as _os
        location = os.path.dirname(_os.__file__)
    if location:
        stdlib = Path(location).resolve()
    site_packages = []
    for key in ('purelib', 'platlib'):
        try:
            location = sysconfig.get_path(key)
        except (KeyError, OSError, ValueError):
            continue
        if location:
            path = Path(location).resolve()
            if path not in site_packages:
                site_packages.append(path)
    sys_path = []
    for entry in sys.path:
        if entry:
            try:
                sys_path.append(Path(entry).resolve())
            except (OSError, RuntimeError):
                continue
    return {'stdlib': stdlib, 'site_packages': site_packages, 'sys_path': sys_path}


def _dotted(relative):
    parts = list(relative.parts)
    if parts and parts[-1].endswith('.py'):
        parts[-1] = parts[-1][:-3]
    if parts and parts[-1] == '__init__' and len(parts) > 1:
        parts.pop()
    return '.'.join(parts)


def extract_module_name(filename, path_info):
    """(module name, kind) of a source file; kind is 'stdlib',
    'site-packages', 'project' or 'other'."""
    if not filename:
        return 'unknown', 'other'
    if filename[0] in '<[':
        return filename, 'other'
    try:
        path = Path(filename).resolve()
    except (OSError, RuntimeError):
        path = Path(filename)
    for directory in path_info.get('site_packages') or ():
        try:
            return _dotted(path.relative_to(directory)), 'site-packages'
        except ValueError:
            pass
    stdlib = path_info.get('stdlib')
    if stdlib is not None:
        try:
            return _dotted(path.relative_to(stdlib)), 'stdlib'
        except ValueError:
            pass
    best = None
    for directory in path_info.get('sys_path') or ():
        try:
            relative = path.relative_to(directory)
        except ValueError:
            continue
        if best is None or len(relative.parts) < len(best.parts):
            best = relative
    if best is not None and str(best) != '.':
        return _dotted(best), 'project'
    name = path.name
    if name.endswith('.py'):
        name = name[:-3]
    return name or 'unknown', 'other'


# ---------------------------------------------------------------------------
# page templates

class _TemplateLoader:
    """The CSS and JavaScript of the generated pages."""

    def __init__(self):
        self.index_css = _INDEX_CSS
        self.file_css = _FILE_CSS
        self.file_js = _FILE_JS
        self.index_js = _INDEX_JS


_COMMON_CSS = """
body { font-family: system-ui, sans-serif; margin: 0; color: #1d2330;
       background: #f6f7f9; }
header { background: #23324a; color: #fff; padding: 12px 20px; }
header h1 { margin: 0; font-size: 20px; }
header .subtitle { opacity: 0.8; font-size: 13px; }
main { padding: 16px 20px; }
a { color: #2457b3; }
"""

_INDEX_CSS = _COMMON_CSS + """
table.files { border-collapse: collapse; width: 100%; background: #fff; }
table.files th, table.files td { padding: 6px 10px; border-bottom: 1px solid #e3e6ea;
                                 text-align: left; }
table.files td.num { text-align: right; font-variant-numeric: tabular-nums; }
.kind { font-size: 11px; padding: 1px 6px; border-radius: 8px; background: #e8ecf3; }
.stats { margin-bottom: 16px; font-size: 14px; }
.stats span { margin-right: 18px; }
"""

_FILE_CSS = _COMMON_CSS + """
.code { font-family: ui-monospace, monospace; font-size: 13px; background: #fff; }
.code-line { display: flex; white-space: pre; scroll-margin-top: 80px; }
.code-line:target { animation: line-flash 1.5s ease-out; outline: 2px solid #f0a020; }
@keyframes line-flash { from { background: #ffe3a0; } to { background: inherit; } }
.line-number { width: 5em; text-align: right; padding-right: 8px; color: #8a93a3;
               user-select: none; }
.line-sample { width: 6em; text-align: right; padding-right: 8px; color: #5b6475; }
.line-self { width: 6em; text-align: right; padding-right: 8px; color: #8a6a1a; }
.line-nav { width: 4em; }
.line-source { flex: 1; padding-left: 6px; }
.nav-btn { border: none; background: #dde5f3; border-radius: 3px; cursor: pointer;
           font-size: 11px; padding: 0 4px; margin-right: 2px; }
.nav-btn:hover { background: #b9c9e6; }
.unavailable { padding: 20px; color: #8a3a2a; }
#nav-menu { position: fixed; display: none; background: #fff; border: 1px solid #ccd;
            box-shadow: 0 2px 8px rgba(0,0,0,0.2); padding: 4px 0; z-index: 10; }
#nav-menu a { display: block; padding: 2px 12px; text-decoration: none; }
"""

_INDEX_JS = """
// nothing dynamic on the index page yet
"""

_FILE_JS = r"""
function restartLineHighlight(target) {
    // re-run the :target animation when the same line is selected again
    target.style.animation = 'none';
    void target.offsetWidth;
    target.style.animation = '';
}

function navigateToLine(link) {
    const url = new URL(link, window.location.href);
    if (url.href === window.location.href) {
        const target = document.getElementById(url.hash.slice(1));
        if (target) {
            restartLineHighlight(target);
            target.scrollIntoView({block: 'center'});
        }
        return;
    }
    window.location.href = url.href;
}

function closeMenu() {
    const menu = document.getElementById('nav-menu');
    if (menu) menu.style.display = 'none';
}

function showMenu(button, entries) {
    const menu = document.getElementById('nav-menu');
    menu.innerHTML = '';
    for (const linkData of entries) {
        const item = document.createElement('a');
        item.href = linkData.link;
        item.textContent = linkData.func + ' (' + linkData.file + ':' + linkData.line +
                           ', ' + linkData.count + ' samples)';
        item.addEventListener('click', (event) => {
            event.preventDefault();
            closeMenu();
            navigateToLine(linkData.link);
        });
        menu.appendChild(item);
    }
    const box = button.getBoundingClientRect();
    menu.style.left = box.left + 'px';
    menu.style.top = (box.bottom + 2) + 'px';
    menu.style.display = 'block';
}

document.addEventListener('click', (event) => {
    const button = event.target.closest('.nav-btn');
    if (!button) {
        closeMenu();
        return;
    }
    event.stopPropagation();
    const navData = button.getAttribute('data-nav');
    const entries = JSON.parse(button.getAttribute('data-entries') || '[]');
    if (entries.length > 1) {
        showMenu(button, entries);
    } else if (navData) {
        navigateToLine(JSON.parse(navData).link);
    }
});
"""


def _page(title, css, body, js=''):
    return ('<!DOCTYPE html>\n<html lang="en">\n<head>\n<meta charset="utf-8">\n'
            '<title>%s</title>\n<style>%s</style>\n</head>\n<body>\n%s\n'
            '<script>%s</script>\n</body>\n</html>\n'
            % (html.escape(title), css, body, js))


# ---------------------------------------------------------------------------

class HeatmapCollector(Collector):
    """Counts samples per source line and the calls between lines."""

    def __init__(self, sample_interval_usec=1000, *, skip_idle=False):
        super().__init__(sample_interval_usec, skip_idle=skip_idle)
        # (filename, line) -> samples with that line on the stack
        self.line_samples = collections.Counter()
        # filename -> Counter(line -> samples)
        self.file_samples = collections.defaultdict(collections.Counter)
        # same, counting only the innermost frame
        self.line_self_samples = collections.Counter()
        self.file_self_samples = collections.defaultdict(collections.Counter)
        # (filename, line) of a caller -> set of callee (filename, line)
        self.call_graph = collections.defaultdict(set)
        # (filename, line) of a callee -> set of caller (filename, line)
        self.callers_graph = collections.defaultdict(set)
        # (caller key, callee key) -> samples
        self.edge_samples = collections.Counter()
        # (filename, function) -> the first line seen of the function
        self.function_definitions = {}
        # (filename, line) -> function name
        self.line_to_function = {}
        self.file_index = {}
        self.stats = {}
        self._total_samples = 0
        self._path_info = get_python_path_info()
        self._templates = _TemplateLoader()

    # -- recording

    def set_stats(self, sample_interval_usec, duration_sec, sample_rate,
                  error_rate=None, missed_samples=None, **kwargs):
        self.stats = {
            'sample_interval_usec': sample_interval_usec,
            'duration_sec': duration_sec,
            'sample_rate': sample_rate,
            'error_rate': error_rate,
            'missed_samples': missed_samples,
            'python_version': sys.version.split()[0],
            'python_implementation': platform.python_implementation(),
            'platform': platform.platform(),
        }
        self.stats.update(kwargs)

    def collect(self, stack_frames, timestamps_us=None):
        weight = len(timestamps_us) if timestamps_us else 1
        for frames, thread_id in self._iter_all_frames(stack_frames):
            self.process_frames(frames, thread_id, weight=weight)

    @staticmethod
    def _usable(filename, lineno):
        if not filename or not isinstance(filename, str):
            return False
        if filename[0] in '<[':
            return False
        return lineno is not None and lineno > 0

    def process_frames(self, frames, thread_id, weight=1):
        """Record one stack (innermost frame first)."""
        self._total_samples += weight
        previous = None   # the frame called by the current one
        for depth, frame in enumerate(frames):
            filename, location, funcname = frame[0], frame[1], frame[2]
            lineno = extract_lineno(location)
            if not self._usable(filename, lineno):
                previous = None
                continue
            key = (filename, lineno)
            self.line_samples[key] += weight
            self.file_samples[filename][lineno] += weight
            if depth == 0:
                self.line_self_samples[key] += weight
                self.file_self_samples[filename][lineno] += weight
            if funcname:
                self.line_to_function[key] = funcname
                func_key = (filename, funcname)
                known = self.function_definitions.get(func_key)
                if known is None or lineno < known:
                    self.function_definitions[func_key] = lineno
            if previous is not None:
                callee_key, callee_func = previous
                self.call_graph[key].add(callee_key)
                self.callers_graph[callee_key].add(key)
                if callee_func:
                    definition = self.function_definitions.get((callee_key[0], callee_func))
                    if definition is not None:
                        self.callers_graph[(callee_key[0], definition)].add(key)
                self.edge_samples[(key, callee_key)] += weight
            previous = (key, funcname)

    # -- export

    def export(self, output_path):
        output = Path(output_path)
        if output.suffix == '.html':
            output = output.with_suffix('')
        output.mkdir(parents=True, exist_ok=True)
        files = sorted(self.file_samples,
                       key=lambda name: -sum(self.file_samples[name].values()))
        self.file_index = {name: 'file_%04d.html' % (index + 1)
                           for index, name in enumerate(files)}
        for name in files:
            page = self._file_page(name)
            (output / self.file_index[name]).write_text(page, encoding='utf-8')
        (output / 'index.html').write_text(self._index_page(files), encoding='utf-8')
        print('Heatmap written to %s' % (output / 'index.html'))

    def _stats_html(self):
        parts = ['<span>Samples: %d</span>' % self._total_samples]
        duration = self.stats.get('duration_sec')
        if duration is not None:
            parts.append('<span>Duration: %.2f s</span>' % duration)
        rate = self.stats.get('sample_rate')
        if rate is not None:
            parts.append('<span>Rate: %.1f samples/s</span>' % rate)
        errors = self.stats.get('error_rate')
        if errors is not None:
            parts.append('<span>Errors: %.2f%%</span>' % errors)
        return '<div class="stats">%s</div>' % ''.join(parts)

    def _index_page(self, files):
        rows = []
        for name in files:
            module, kind = extract_module_name(name, self._path_info)
            total = sum(self.file_samples[name].values())
            own = sum(self.file_self_samples.get(name, {}).values())
            rows.append(
                '<tr><td><a href="%s">%s</a><br><small>%s</small></td>'
                '<td><span class="kind">%s</span></td>'
                '<td class="num">%d</td><td class="num">%d</td></tr>'
                % (self.file_index[name], html.escape(module), html.escape(name),
                   html.escape(kind), total, own))
        if not rows:
            rows.append('<tr><td colspan="4">No samples were collected.</td></tr>')
        body = (
            '<header><h1>Tachyon Profiler</h1>'
            '<div class="subtitle">Line heat map</div></header>\n<main>%s\n'
            '<table class="files"><thead><tr><th>File</th><th>Kind</th>'
            '<th>Samples</th><th>Self</th></tr></thead>\n<tbody>\n%s\n</tbody></table>'
            '</main>' % (self._stats_html(), '\n'.join(rows)))
        return _page('Tachyon Profiler - Heatmap', self._templates.index_css, body,
                     self._templates.index_js)

    @staticmethod
    def _read_source(filename):
        """The source lines, or None if the file is not a readable regular
        file (FIFOs and directories are never opened)."""
        try:
            info = os.stat(filename)
        except (OSError, ValueError):
            return None
        if not stat.S_ISREG(info.st_mode):
            return None
        try:
            text = Path(filename).read_text(encoding='utf-8', errors='replace')
        except OSError:
            return None
        return text.splitlines()

    def _nav_entries(self, key, graph, as_caller):
        entries = []
        for other in sorted(graph.get(key, ())):
            if as_caller:
                count = self.edge_samples.get((other, key), 0)
            else:
                count = self.edge_samples.get((key, other), 0)
            page = self.file_index.get(other[0])
            if page is None:
                continue
            entries.append({
                'link': '%s#line-%d' % (page, other[1]),
                'file': os.path.basename(other[0]),
                'line': other[1],
                'func': self.line_to_function.get(other, '?'),
                'count': count,
            })
        entries.sort(key=lambda entry: -entry['count'])
        return entries

    def _nav_button(self, label, title, entries):
        if not entries:
            return ''
        return ('<button class="nav-btn" title="%s" data-nav="%s" data-entries="%s">%s</button>'
                % (title, html.escape(json.dumps(entries[0])),
                   html.escape(json.dumps(entries)), label))

    def _file_page(self, filename):
        counts = self.file_samples[filename]
        own = self.file_self_samples.get(filename, {})
        peak = max(counts.values()) if counts else 1
        module, kind = extract_module_name(filename, self._path_info)
        lines = self._read_source(filename)
        header = ('<header><h1>Tachyon Profiler</h1><div class="subtitle">%s '
                  '(%s) &middot; <a style="color:#cde" href="index.html">index</a>'
                  '</div></header>' % (html.escape(filename), html.escape(kind)))
        if lines is None:
            body = (header + '<main><div class="unavailable">Source file not '
                    'available.</div>%s</main>' % self._sample_table(counts, own))
            return _page('%s - Tachyon Profiler' % module, self._templates.file_css,
                         body, self._templates.file_js)
        rows = []
        for lineno, text in enumerate(lines, 1):
            samples = counts.get(lineno, 0)
            self_samples = own.get(lineno, 0)
            key = (filename, lineno)
            callers = self._nav_entries(key, self.callers_graph, True)
            callees = self._nav_entries(key, self.call_graph, False)
            if samples:
                alpha = 0.12 + 0.6 * samples / peak
                style = ' style="background: rgba(230, 80, 40, %.3f)"' % alpha
            else:
                style = ''
            rows.append(
                '<div class="code-line" id="line-%d"%s>'
                '<span class="line-number">%d</span>'
                '<span class="line-sample">%s</span>'
                '<span class="line-self">%s</span>'
                '<span class="line-nav">%s%s</span>'
                '<span class="line-source">%s</span></div>'
                % (lineno, style, lineno, samples or '', self_samples or '',
                   self._nav_button('&uarr;', 'callers', callers),
                   self._nav_button('&darr;', 'callees', callees),
                   html.escape(text)))
        body = (header + '<main>%s<div class="code">\n%s\n</div>'
                '<div id="nav-menu"></div></main>'
                % (self._stats_html(), '\n'.join(rows)))
        return _page('%s - Tachyon Profiler' % module, self._templates.file_css, body,
                     self._templates.file_js)

    @staticmethod
    def _sample_table(counts, own):
        rows = ''.join('<tr><td>line-%d</td><td class="line-sample">%d</td><td>%d</td></tr>'
                       % (line, count, own.get(line, 0))
                       for line, count in sorted(counts.items()))
        return '<table>%s</table>' % rows
