"""Unix shell-style filename pattern matching.

Patterns:
    *       matches everything
    ?       matches any single character
    [seq]   matches any character in seq
    [!seq]  matches any character not in seq

fnmatch() normalizes case with os.path.normcase(); fnmatchcase() does not.
"""

import functools
import itertools
import os
import posixpath
import re

__all__ = ["filter", "filterfalse", "fnmatch", "fnmatchcase", "translate"]


def fnmatch(name, pat):
    """Test whether name matches pat (case-normalized per platform)."""
    name = os.path.normcase(name)
    pat = os.path.normcase(pat)
    return fnmatchcase(name, pat)


@functools.lru_cache(maxsize=32768, typed=True)
def _compile_pattern(pat):
    if isinstance(pat, bytes):
        res = bytes(translate(str(pat, 'ISO-8859-1')), 'ISO-8859-1')
    else:
        res = translate(pat)
    return re.compile(res).match


def filter(names, pat):
    """Return the subset of names that match pat."""
    result = []
    pat = os.path.normcase(pat)
    match = _compile_pattern(pat)
    if os.path is posixpath:
        # normcase is the identity on POSIX.
        for name in names:
            if match(name):
                result.append(name)
    else:
        for name in names:
            if match(os.path.normcase(name)):
                result.append(name)
    return result


def filterfalse(names, pat):
    """Return the subset of names that do not match pat."""
    pat = os.path.normcase(pat)
    match = _compile_pattern(pat)
    if os.path is posixpath:
        return list(itertools.filterfalse(match, names))
    result = []
    for name in names:
        if match(os.path.normcase(name)) is None:
            result.append(name)
    return result


def fnmatchcase(name, pat):
    """Test whether name matches pat, case-sensitively."""
    match = _compile_pattern(pat)
    return match(name) is not None


def translate(pat):
    """Translate a shell pattern to a regular expression (no special
    handling of path separators)."""
    parts, star_indices = _translate(pat, '*', '.')
    return _join_translated_parts(parts, star_indices)


_re_setops_sub = re.compile(r'([&~|])').sub


def _translate(pat, star, question_mark):
    """Return (parts, star_indices): the pattern as a list of regex pieces
    (one per pattern element, `star` for each run of '*') and the indices
    of the star pieces."""
    res = []
    add = res.append
    star_indices = []

    i, n = 0, len(pat)
    while i < n:
        c = pat[i]
        i += 1
        if c == '*':
            # Consecutive stars collapse into one.
            if not res or res[-1] is not star:
                star_indices.append(len(res))
                add(star)
        elif c == '?':
            add(question_mark)
        elif c == '[':
            j = i
            if j < n and pat[j] == '!':
                j += 1
            if j < n and pat[j] == ']':
                j += 1
            while j < n and pat[j] != ']':
                j += 1
            if j >= n:
                add('\\[')
                continue
            stuff = pat[i:j]
            if '-' not in stuff:
                stuff = stuff.replace('\\', r'\\')
            else:
                # Split at range hyphens; ranges are kept, other hyphens
                # and backslashes are escaped.
                chunks = []
                k = i + 2 if pat[i] == '!' else i + 1
                while True:
                    k = pat.find('-', k, j)
                    if k < 0:
                        break
                    chunks.append(pat[i:k])
                    i = k + 1
                    k = k + 3
                chunk = pat[i:j]
                if chunk:
                    chunks.append(chunk)
                else:
                    chunks[-1] += '-'
                # Drop empty ranges (invalid in a regex set).
                for k in range(len(chunks) - 1, 0, -1):
                    if chunks[k - 1][-1] > chunks[k][0]:
                        chunks[k - 1] = chunks[k - 1][:-1] + chunks[k][1:]
                        del chunks[k]
                stuff = '-'.join(s.replace('\\', r'\\').replace('-', r'\-')
                                 for s in chunks)
            # Escape characters that would start a set operation.
            stuff = _re_setops_sub(r'\\\1', stuff)
            i = j + 1
            if not stuff:
                # Empty set: never matches.
                add('(?!)')
            elif stuff == '!':
                # Negated empty set: any character.
                add('.')
            else:
                if stuff[0] == '!':
                    stuff = '^' + stuff[1:]
                elif stuff[0] in ('^', '['):
                    stuff = '\\' + stuff
                add(f'[{stuff}]')
        else:
            add(re.escape(c))
    assert i == n
    return res, star_indices


def _join_translated_parts(parts, star_indices):
    if not star_indices:
        return fr'(?s:{"".join(parts)})\z'
    it = iter(star_indices)
    j = next(it)
    buf = parts[:j]
    i = j + 1
    # Every star except the last becomes an atomic lazy group followed by
    # the fixed text up to the next star: no backtracking blow-up.
    for j in it:
        buf.append('(?>.*?')
        buf.extend(parts[i:j])
        buf.append(')')
        i = j + 1
    buf.append('.*')
    buf.extend(parts[i:])
    return fr'(?s:{"".join(buf)})\z'
