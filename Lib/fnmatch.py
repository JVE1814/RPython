"""Shell-style wildcard matching of file names.

    *       any sequence of characters (also none)
    ?       any single character
    [seq]   one character from seq; ranges such as a-z are allowed
    [!seq]  one character not in seq

fnmatch() first applies os.path.normcase() to both arguments (so matching
is case-insensitive on Windows); fnmatchcase() compares as given.
translate() gives the equivalent regular expression.
"""

import functools
import itertools
import os
import posixpath
import re

__all__ = ["filter", "filterfalse", "fnmatch", "fnmatchcase", "translate"]


def fnmatch(name, pat):
    """True if name matches the pattern pat, after os.path.normcase()."""
    return fnmatchcase(os.path.normcase(name), os.path.normcase(pat))


def fnmatchcase(name, pat):
    """True if name matches the pattern pat (no case normalisation)."""
    return _matcher(pat)(name) is not None


def filter(names, pat):
    """The elements of names that match pat (as fnmatch() would)."""
    match = _matcher(os.path.normcase(pat))
    if os.path is posixpath:            # normcase does nothing here
        return [name for name in names if match(name)]
    return [name for name in names if match(os.path.normcase(name))]


def filterfalse(names, pat):
    """The elements of names that do not match pat."""
    match = _matcher(os.path.normcase(pat))
    if os.path is posixpath:
        return list(itertools.filterfalse(match, names))
    return [name for name in names if match(os.path.normcase(name)) is None]


@functools.lru_cache(maxsize=1 << 15, typed=True)
def _matcher(pat):
    if isinstance(pat, bytes):
        # bytes patterns: translate the Latin-1 text, compile as bytes
        regex = translate(pat.decode('latin-1')).encode('latin-1')
    else:
        regex = translate(pat)
    return re.compile(regex).match


def translate(pat):
    """Return a regular expression (as a string) matching what the shell
    pattern pat matches.  Path separators get no special treatment."""
    parts, stars = _translate(pat, '*', '.')
    return _assemble(parts, stars)


def _assemble(parts, stars):
    """Join the translated pieces; star pieces become '.*' patterns.

    Every star except the last is an atomic lazy group that ends with the
    fixed text up to the next star, which keeps failing matches linear.
    """
    if not stars:
        return fr'(?s:{"".join(parts)})\z'
    out = parts[:stars[0]]
    for here, following in zip(stars, stars[1:]):
        out.append('(?>.*?')
        out.extend(parts[here + 1:following])
        out.append(')')
    out.append('.*')
    out.extend(parts[stars[-1] + 1:])
    return fr'(?s:{"".join(out)})\z'


def _translate(pat, star, question_mark):
    """Translate pat into a list of regex pieces, one per pattern element.

    Runs of '*' give a single piece equal to star; '?' gives question_mark.
    Returns (pieces, indices of the star pieces).
    """
    pieces = []
    stars = []
    i, n = 0, len(pat)
    while i < n:
        c = pat[i]
        i += 1
        if c == '*':
            if not (stars and stars[-1] == len(pieces) - 1):
                stars.append(len(pieces))
                pieces.append(star)
        elif c == '?':
            pieces.append(question_mark)
        elif c == '[':
            end = _bracket_end(pat, i)
            if end < 0:
                pieces.append('\\[')    # no closing bracket: a literal '['
            else:
                pieces.append(_char_class(pat[i:end]))
                i = end + 1
        else:
            pieces.append(re.escape(c))
    return pieces, stars


def _bracket_end(pat, start):
    """Index of the ']' closing a bracket expression whose body starts at
    start, or -1.  A ']' first in the body (after an optional '!') is a
    member, not the end."""
    j = start
    if j < len(pat) and pat[j] == '!':
        j += 1
    if j < len(pat) and pat[j] == ']':
        j += 1
    return pat.find(']', j)


_SET_SPECIAL = {'\\': '\\\\', '-': '\\-', '&': '\\&', '~': '\\~', '|': '\\|'}


def _class_char(c):
    # escape what is special inside a regex set (backslash, hyphen) and
    # what could start a set operation
    return _SET_SPECIAL.get(c, c)


def _char_class(body):
    """Regex for the bracket expression with the given body."""
    negated = body.startswith('!')
    if negated:
        body = body[1:]
    members = []
    k = 0
    while k < len(body):
        if k + 2 < len(body) and body[k + 1] == '-':
            low, high = body[k], body[k + 2]
            k += 3
            if low <= high:             # an empty range matches nothing
                members.append(f'{_class_char(low)}-{_class_char(high)}')
        else:
            members.append(_class_char(body[k]))
            k += 1
    text = ''.join(members)
    if not text:
        return '.' if negated else '(?!)'
    if negated:
        return f'[^{text}]'
    if text[0] in '^[':
        text = '\\' + text              # not negation, not a nested set
    return f'[{text}]'
