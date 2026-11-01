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


def _translate_segment(part, sep_class):
    """Regex source for one path segment (no separators)."""
    if part == '*':
        return f'[^{sep_class}]+'
    out = []
    i, n = 0, len(part)
    while i < n:
        c = part[i]
        i += 1
        if c == '*':
            # collapse runs of '*'
            while i < n and part[i] == '*':
                i += 1
            out.append(f'[^{sep_class}]*')
        elif c == '?':
            out.append(f'[^{sep_class}]')
        elif c == '[':
            j = i
            if j < n and part[j] == '!':
                j += 1
            if j < n and part[j] == ']':
                j += 1
            while j < n and part[j] != ']':
                j += 1
            if j >= n:
                out.append('\\[')
            else:
                stuff = part[i:j]
                if '-' not in stuff:
                    stuff = stuff.replace('\\', r'\\')
                else:
                    chunks = []
                    k = i + 2 if part[i] == '!' else i + 1
                    lo = i
                    while True:
                        k = part.find('-', k, j)
                        if k < 0:
                            break
                        chunks.append(part[lo:k])
                        lo = k + 1
                        k = k + 3
                    chunk = part[lo:j]
                    if chunk:
                        chunks.append(chunk)
                    else:
                        chunks[-1] += '-'
                    # Remove empty ranges -- invalid in RE.
                    for k in range(len(chunks) - 1, 0, -1):
                        if chunks[k - 1][-1] > chunks[k][0]:
                            chunks[k - 1] = chunks[k - 1][:-1] + chunks[k][1:]
                            del chunks[k]
                    stuff = '-'.join(s.replace('\\', r'\\').replace('-', r'\-')
                                     for s in chunks)
                stuff = re.sub(r'([&~|])', r'\\\1', stuff)
                i = j + 1
                if not stuff:
                    out.append('(?!)')
                elif stuff == '!':
                    out.append('.')
                else:
                    if stuff[0] == '!':
                        stuff = '^' + stuff[1:]
                    elif stuff[0] in ('^', '['):
                        stuff = '\\' + stuff
                    # never match a separator
                    out.append(f'(?![{sep_class}])[{stuff}]')
        else:
            out.append(re.escape(c))
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


class Globber:
    """String-based globber.

    scandir(path_str) returns an iterable of (name, is_dir) pairs, where
    is_dir(follow_symlinks) tells whether the entry is a directory; it raises
    OSError if path_str cannot be scanned.  An empty path_str means the
    current directory.  lexists(path_str) tells whether a path exists
    (without following a final symlink)."""

    def __init__(self, sep, case_sensitive, scandir, lexists, recurse_symlinks):
        self.sep = sep
        self.case_sensitive = case_sensitive
        self.scandir = scandir
        self.lexists = lexists
        self.recurse_symlinks = recurse_symlinks

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
            if part == '**' or has_magic(part):
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
        for name, is_dir in entries:
            if not match(name):
                continue
            if last:
                yield path + name
            else:
                try:
                    d = is_dir(True)
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
            for name, is_dir in entries:
                try:
                    d = is_dir(self.recurse_symlinks)
                except OSError:
                    d = False
                if last:
                    yield dirpath + name
                if d:
                    subdirs.append(dirpath + name + sep)
            stack.extend(reversed(subdirs))


