"""Pattern matching and globbing for pathlib.

Patterns are translated segment by segment into regular expressions.  The
globber works on path *strings* so that it can serve both the concrete
local paths (scanning with os.scandir) and the abstract _ReadablePath
classes (scanning with iterdir() and PathInfo objects).
"""

import functools
import re

_SPECIAL = frozenset('*?[')


def has_magic(part):
    return any(c in _SPECIAL for c in part)


def _bracket(part, start, sep_class):
    """Parse a '[...]' class beginning at part[start] (the '[').

    Returns (regex, next_index), or None if the bracket is not closed (it
    is then matched literally)."""
    i = start + 1
    negate = i < len(part) and part[i] == '!'
    if negate:
        i += 1
    items = []
    first = True
    while i < len(part) and (part[i] != ']' or first):
        first = False
        lo = part[i]
        if i + 2 < len(part) and part[i + 1] == '-' and part[i + 2] != ']':
            hi = part[i + 2]
            if lo <= hi:   # reversed ranges match nothing
                items.append(re.escape(lo) + '-' + re.escape(hi))
            i += 3
        else:
            items.append(re.escape(lo))
            i += 1
    if i >= len(part):
        return None
    if not items:
        body = '(?!)' if not negate else f'[^{sep_class}]'
    else:
        inner = ''.join(items)
        body = f'[^{sep_class}{inner}]' if negate else f'(?![{sep_class}])[{inner}]'
    return body, i + 1


def _translate_segment(part, sep_class):
    """Regex source for one path segment: '*' and '?' never match a
    separator, '[...]' and '[!...]' are character classes."""
    if part == '*':
        return f'[^{sep_class}]+'
    out = []
    i = 0
    while i < len(part):
        c = part[i]
        if c == '*':
            while i < len(part) and part[i] == '*':
                i += 1
            out.append(f'[^{sep_class}]*')
            continue
        if c == '?':
            out.append(f'[^{sep_class}]')
        elif c == '[':
            parsed = _bracket(part, i, sep_class)
            if parsed is not None:
                out.append(parsed[0])
                i = parsed[1]
                continue
            out.append(re.escape(c))
        else:
            out.append(re.escape(c))
        i += 1
    return ''.join(out)



def translate(pattern, seps):
    """Translate a full pattern (path string) into a regex source that matches
    a whole path string.  '**' segments match any number of segments."""
    sep = seps[0]
    sep_class = re.escape(seps)
    if len(seps) > 1:
        for s in seps[1:]:
            pattern = pattern.replace(s, sep)
    parts = pattern.split(sep)
    out = []
    last = len(parts) - 1
    for idx, part in enumerate(parts):
        if part == '**':
            if idx < last:
                out.append(f'(?:.+[{sep_class}])?')
            else:
                out.append('.*')
            continue
        out.append(_translate_segment(part, sep_class))
        if idx < last:
            out.append(f'[{sep_class}]')
    return fr'(?s:{"".join(out)})\Z'


@functools.lru_cache(maxsize=512)
def compile_pattern(pattern, seps, case_sensitive):
    flags = 0 if case_sensitive else re.IGNORECASE
    return re.compile(translate(pattern, seps), flags).match


@functools.lru_cache(maxsize=512)
def _compile_segment(part, seps, case_sensitive):
    flags = 0 if case_sensitive else re.IGNORECASE
    return re.compile(fr'(?s:{_translate_segment(part, re.escape(seps))})\Z', flags).match


def _call_entry(entry, follow_symlinks):
    return entry(follow_symlinks)


class Globber:
    """String-based globber.

    scandir(path_str) returns an iterable of (name, entry) pairs, and
    is_dir(entry, follow_symlinks) tells whether an entry is a directory;
    scandir raises OSError if path_str cannot be scanned.  An empty path_str
    means the current directory.  lexists(path_str) tells whether a path
    exists (without following a final symlink)."""

    def __init__(self, sep, case_sensitive, scandir, lexists, recurse_symlinks,
                 case_pedantic=False, is_dir=None):
        self.sep = sep
        self.case_sensitive = case_sensitive
        self.scandir = scandir
        self.lexists = lexists
        self.recurse_symlinks = recurse_symlinks
        # True when case_sensitive differs from the file system's own rule:
        # then even literal segments must be compared with directory listings
        self.case_pedantic = case_pedantic
        # without is_dir, the entries are callables taking follow_symlinks
        self.is_dir = is_dir if is_dir is not None else _call_entry

    def _scan(self, path):
        try:
            return list(self.scandir(path))
        except OSError:
            return None

    def select(self, base, parts):
        """Yield path strings matching `parts` (list of segments; a trailing
        '' segment means "directories only") below `base` (a path string that
        is empty or ends with a separator)."""
        return self._select(base, parts, 0)

    def _select(self, path, parts, idx):
        sep = self.sep
        n = len(parts)
        # literal segments are appended without touching the file system
        while idx < n:
            part = parts[idx]
            if idx == n - 1 and part == '':
                # trailing separator: path must be an existing directory
                if self._scan(path) is not None:
                    yield path
                return
            if (part == '**' or has_magic(part)
                    or (self.case_pedantic and part != '..')):
                break
            if idx == n - 1:
                if self.lexists(path + part):
                    yield path + part
                return
            path = path + part + sep
            idx += 1
        else:
            return

        part = parts[idx]
        if part == '**':
            while idx + 1 < n and parts[idx + 1] == '**':
                idx += 1
            yield from self._select_recursive(path, parts, idx)
            return
        last = idx == n - 1
        entries = self._scan(path)
        if entries is None:
            return
        match = _compile_segment(part, sep, self.case_sensitive)
        for name, entry in entries:
            if not match(name):
                continue
            if last:
                yield path + name
            else:
                try:
                    d = self.is_dir(entry, True)
                except OSError:
                    d = False
                if d:
                    yield from self._select(path + name + sep, parts, idx + 1)

    def _select_recursive(self, path, parts, idx):
        sep = self.sep
        last = idx == len(parts) - 1
        if self._scan(path) is None:
            return
        if last:
            yield path
        stack = [path]
        while stack:
            dirpath = stack.pop()
            if not last:
                yield from self._select(dirpath, parts, idx + 1)
            entries = self._scan(dirpath)
            if entries is None:
                continue
            subdirs = []
            for name, entry in entries:
                try:
                    d = self.is_dir(entry, self.recurse_symlinks)
                except OSError:
                    d = False
                if last:
                    yield dirpath + name
                if d:
                    subdirs.append(dirpath + name + sep)
            stack.extend(reversed(subdirs))


