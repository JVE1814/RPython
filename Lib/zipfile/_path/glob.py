"""Translate glob patterns over '/'-separated archive paths into regexes."""

import os
import re

_default_seps = os.sep + str(os.altsep) * bool(os.altsep)


class Translator:
    """Glob -> regular expression translator.

    '*' matches within one path segment, '**' (a whole segment) matches any
    number of segments, '?' one non-separator character and [...] character
    classes.  Patterns ending in a separator only match directories.
    """

    seps: str

    def __init__(self, seps: str = _default_seps):
        assert seps and set(seps) <= set(_default_seps), "Invalid separators"
        self.seps = seps

    def translate(self, pattern):
        """Return a regex (without anchors) matching *pattern*; the result
        also matches the directory form with a trailing separator."""
        return self.extend(self.match_dirs(self.translate_core(pattern)))

    def extend(self, pattern):
        """Match the whole string; dot matches newline too."""
        return rf'(?s:{pattern})\Z'

    def match_dirs(self, pattern):
        """Allow an optional trailing separator (directories)."""
        return rf'{pattern}[/]?'

    def translate_core(self, pattern):
        self.restrict_rglob(pattern)
        sep = re.escape(self.seps)
        not_sep = f'[^{sep}]'
        out = []
        for segment in self._segments(pattern):
            if segment == '**':
                out.append('.*')
            elif segment.rstrip(self.seps) == '**':
                # any number (including zero) of leading directories
                out.append(f'(?:.*[{sep}])?')
            else:
                out.append(self._segment(segment, not_sep))
        return ''.join(out)

    def _segments(self, pattern):
        # Keep separators attached to the preceding segment.
        return re.split(f'(?<=[{re.escape(self.seps)}])', pattern)

    def _segment(self, seg, not_sep):
        res = []
        i = 0
        n = len(seg)
        while i < n:
            c = seg[i]
            if c == '*':
                res.append(f'{not_sep}*')
            elif c == '?':
                res.append(not_sep)
            elif c == '[':
                j = seg.find(']', i + 1)
                if j < 0:
                    res.append(re.escape(c))
                else:
                    body = seg[i + 1:j]
                    if body.startswith('!'):
                        body = '^' + body[1:]
                    res.append('[' + body.replace('\\', '\\\\') + ']')
                    i = j
            elif c in self.seps:
                res.append(f'[{re.escape(self.seps)}]')
            else:
                res.append(re.escape(c))
            i += 1
        return ''.join(res)

    def restrict_rglob(self, pattern):
        """'**' may only appear as a complete path segment."""
        seps_pattern = rf'[{re.escape(self.seps)}]+'
        segments = re.split(seps_pattern, pattern)
        if any('**' in segment and segment != '**' for segment in segments):
            raise ValueError("** must appear alone in a path segment")

    def star_not_empty(self, pattern):
        """Ensure that '*' matches at least one character in a segment."""
        return re.sub(r'(?<!\*)\*(?!\*)', '?*', pattern)
