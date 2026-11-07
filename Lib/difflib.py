"""Compare sequences and report the differences.

Classes:

    SequenceMatcher -- finds the matching parts of two sequences of
                       hashable elements and turns them into edit operations
    Differ          -- line-by-line comparison of two lists of text lines,
                       with intraline hints (the format ndiff() produces)
    HtmlDiff        -- side-by-side comparison rendered as an HTML table

Functions:

    get_close_matches() -- the best "good enough" matches for a word
    ndiff()             -- Differ-style delta of two lists of lines
    restore()           -- recover one of the inputs from an ndiff() delta
    unified_diff()      -- delta in the unified diff format
    context_diff()      -- delta in the context diff format
    diff_bytes()        -- run unified_diff()/context_diff() on bytes
    IS_LINE_JUNK(), IS_CHARACTER_JUNK() -- ready-made junk predicates
"""

__all__ = ['get_close_matches', 'ndiff', 'restore', 'SequenceMatcher',
           'Differ', 'IS_CHARACTER_JUNK', 'IS_LINE_JUNK', 'context_diff',
           'unified_diff', 'diff_bytes', 'HtmlDiff', 'Match']

from collections import namedtuple as _namedtuple
from heapq import nlargest as _nlargest
from types import GenericAlias

Match = _namedtuple('Match', 'a b size')


def _similarity(matches, total):
    """2*M/T, the similarity measure of all the *ratio() methods."""
    if total:
        return 2.0 * matches / total
    return 1.0


# ---------------------------------------------------------------------------
# SequenceMatcher

class SequenceMatcher:
    """Find the longest contiguous matching blocks of two sequences.

    The elements must be hashable.  The search is the Ratcliff/Obershelp
    "gestalt" idea: take the longest matching block, then do the same on the
    pieces to its left and to its right.  Elements the optional isjunk
    predicate accepts never start a match (they can only extend one), and
    with autojunk enabled, elements making up more than 1% of a second
    sequence of at least 200 items are treated the same way.  The result is
    meant to look natural to people; it is not a minimal edit script.

    The second sequence is indexed once, so to compare one sequence with
    many others, set it with set_seq2() and vary the first with set_seq1().

    >>> m = SequenceMatcher(None, "tram line", "tramline 9")
    >>> m.get_matching_blocks()
    [Match(a=0, b=0, size=4), Match(a=5, b=4, size=4), Match(a=9, b=10, size=0)]
    >>> for op in m.get_opcodes():
    ...     print(op)
    ('equal', 0, 4, 0, 4)
    ('delete', 4, 5, 4, 4)
    ('equal', 5, 9, 4, 8)
    ('insert', 9, 9, 8, 10)
    >>> round(m.ratio(), 3)
    0.842

    Attributes set by set_seq2() that may be inspected: b2j (each indexed
    element of b mapped to the ascending list of its positions), bjunk (the
    elements isjunk rejected) and bpopular (the elements autojunk dropped).
    """

    def __init__(self, isjunk=None, a='', b='', autojunk=True):
        """isjunk is None or a one-argument predicate telling whether an
        element of b is junk; a and b are the sequences to compare;
        autojunk=False disables the popular-element heuristic."""
        self.isjunk = isjunk
        self.autojunk = autojunk
        self.a = self.b = None
        self.set_seqs(a, b)

    def set_seqs(self, a, b):
        """Set both sequences to compare."""
        self.set_seq1(a)
        self.set_seq2(b)

    def set_seq1(self, a):
        """Set the first sequence (the cheap one to change)."""
        if a is self.a:
            return
        self.a = a
        self._blocks = self._codes = None

    def set_seq2(self, b):
        """Set the second sequence and index it."""
        if b is self.b:
            return
        self.b = b
        self._blocks = self._codes = None
        self._b_counts = None
        self._index_b()

    def _index_b(self):
        b = self.b
        positions = {}
        for index, element in enumerate(b):
            where = positions.get(element)
            if where is None:
                positions[element] = [index]
            else:
                where.append(index)
        junk = set()
        if self.isjunk is not None:
            isjunk = self.isjunk
            junk = {element for element in positions if isjunk(element)}
            for element in junk:
                del positions[element]
        popular = set()
        size = len(b)
        if self.autojunk and size >= 200:
            limit = size // 100 + 1
            popular = {element for element, where in positions.items()
                       if len(where) > limit}
            for element in popular:
                del positions[element]
        self.b2j = positions
        self.bjunk = junk
        self.bpopular = popular

    def find_longest_match(self, alo=0, ahi=None, blo=0, bhi=None):
        """Return the longest matching block in a[alo:ahi] and b[blo:bhi].

        Ties go to the block starting earliest in a, then earliest in b.
        The block is grown on both sides over equal elements that the index
        leaves out (popular ones first, then junk).  Without any match the
        result has size 0.

        >>> SequenceMatcher(None, "a lot of", "lots of").find_longest_match()
        Match(a=2, b=0, size=3)
        """
        a, b, positions, junk = self.a, self.b, self.b2j, self.bjunk
        if ahi is None:
            ahi = len(a)
        if bhi is None:
            bhi = len(b)
        best_i, best_j, best = alo, blo, 0
        # ending[j]: length of the common run that ends at a[i-1] and b[j]
        ending = {}
        nothing = ()
        for i in range(alo, ahi):
            current = {}
            for j in positions.get(a[i], nothing):
                if j < blo:
                    continue
                if j >= bhi:
                    break
                run = current[j] = ending.get(j - 1, 0) + 1
                if run > best:
                    best_i, best_j, best = i - run + 1, j - run + 1, run
            ending = current
        for wanted in (False, True):
            while (best_i > alo and best_j > blo and
                   (b[best_j - 1] in junk) is wanted and
                   a[best_i - 1] == b[best_j - 1]):
                best_i -= 1
                best_j -= 1
                best += 1
            while (best_i + best < ahi and best_j + best < bhi and
                   (b[best_j + best] in junk) is wanted and
                   a[best_i + best] == b[best_j + best]):
                best += 1
        return Match(best_i, best_j, best)

    def get_matching_blocks(self):
        """Return the matching blocks as a list of Match triples (i, j, n),
        meaning a[i:i+n] == b[j:j+n], increasing in i and j.  Adjacent
        blocks are merged; a final Match(len(a), len(b), 0) closes the list.

        >>> SequenceMatcher(None, "abxcd", "abcd").get_matching_blocks()
        [Match(a=0, b=0, size=2), Match(a=3, b=2, size=2), Match(a=5, b=4, size=0)]
        """
        if self._blocks is not None:
            return self._blocks
        len_a, len_b = len(self.a), len(self.b)
        found = []
        pending = [(0, len_a, 0, len_b)]
        while pending:
            alo, ahi, blo, bhi = pending.pop()
            i, j, size = match = self.find_longest_match(alo, ahi, blo, bhi)
            if not size:
                continue
            found.append(match)
            if alo < i and blo < j:
                pending.append((alo, i, blo, j))
            if i + size < ahi and j + size < bhi:
                pending.append((i + size, ahi, j + size, bhi))
        found.sort()
        merged = []
        for i, j, size in found:
            if merged:
                pi, pj, psize = merged[-1]
                if pi + psize == i and pj + psize == j:
                    merged[-1] = (pi, pj, psize + size)
                    continue
            merged.append((i, j, size))
        merged.append((len_a, len_b, 0))
        self._blocks = [Match._make(block) for block in merged]
        return self._blocks

    def get_opcodes(self):
        """Return the edit operations turning a into b, as a list of
        (tag, i1, i2, j1, j2) tuples covering both sequences in order.

        tag is 'replace' (a[i1:i2] becomes b[j1:j2]), 'delete' (a[i1:i2]
        goes, j1 == j2), 'insert' (b[j1:j2] arrives, i1 == i2) or 'equal'.

        >>> SequenceMatcher(None, "spam", "slam").get_opcodes()
        [('equal', 0, 1, 0, 1), ('replace', 1, 2, 1, 2), ('equal', 2, 4, 2, 4)]
        """
        if self._codes is not None:
            return self._codes
        codes = []
        i = j = 0
        for ai, bj, size in self.get_matching_blocks():
            if i < ai:
                codes.append(('replace' if j < bj else 'delete', i, ai, j, bj))
            elif j < bj:
                codes.append(('insert', i, ai, j, bj))
            i, j = ai + size, bj + size
            if size:
                codes.append(('equal', ai, i, bj, j))
        self._codes = codes
        return codes

    def get_grouped_opcodes(self, n=3):
        """Yield the opcodes in groups ("hunks") with at most n elements of
        unchanged context around each change; long unchanged stretches
        separate groups.

        >>> sm = SequenceMatcher(None, list("abcdefghij"), list("abXdefghiY"))
        >>> for group in sm.get_grouped_opcodes(1):
        ...     print(group)
        [('equal', 1, 2, 1, 2), ('replace', 2, 3, 2, 3), ('equal', 3, 4, 3, 4)]
        [('equal', 8, 9, 8, 9), ('replace', 9, 10, 9, 10)]
        """
        codes = list(self.get_opcodes()) or [('equal', 0, 1, 0, 1)]
        tag, i1, i2, j1, j2 = codes[0]
        if tag == 'equal':
            codes[0] = (tag, max(i1, i2 - n), i2, max(j1, j2 - n), j2)
        tag, i1, i2, j1, j2 = codes[-1]
        if tag == 'equal':
            codes[-1] = (tag, i1, min(i2, i1 + n), j1, min(j2, j1 + n))
        group = []
        for tag, i1, i2, j1, j2 in codes:
            if tag == 'equal' and i2 - i1 > 2 * n:
                group.append((tag, i1, min(i2, i1 + n), j1, min(j2, j1 + n)))
                yield group
                group = []
                i1, j1 = max(i1, i2 - n), max(j1, j2 - n)
            group.append((tag, i1, i2, j1, j2))
        if group and not (len(group) == 1 and group[0][0] == 'equal'):
            yield group

    def ratio(self):
        """Similarity in [0, 1]: twice the number of matched elements over
        the total number of elements (1.0 for two empty sequences)."""
        matched = sum(block.size for block in self.get_matching_blocks())
        return _similarity(matched, len(self.a) + len(self.b))

    def quick_ratio(self):
        """An upper bound on ratio(), cheaper to compute: elements are
        matched regardless of their order."""
        counts = self._b_counts
        if counts is None:
            counts = self._b_counts = {}
            for element in self.b:
                counts[element] = counts.get(element, 0) + 1
        left = {}
        matched = 0
        for element in self.a:
            have = left[element] if element in left else counts.get(element, 0)
            left[element] = have - 1
            if have > 0:
                matched += 1
        return _similarity(matched, len(self.a) + len(self.b))

    def real_quick_ratio(self):
        """An even cheaper upper bound on ratio(), from the lengths only."""
        len_a, len_b = len(self.a), len(self.b)
        return _similarity(min(len_a, len_b), len_a + len_b)

    __class_getitem__ = classmethod(GenericAlias)


def get_close_matches(word, possibilities, n=3, cutoff=0.6):
    """Return up to n of the possibilities that resemble word best.

    Only candidates whose SequenceMatcher ratio against word is at least
    cutoff (a float in [0, 1]) are considered; the best come first.

    >>> get_close_matches("pyton", ["python", "pylon", "piston", "cython"])
    ['python', 'pylon', 'piston']
    >>> get_close_matches("lettuce", ["lattice", "letter", "salad"], n=1)
    ['letter']
    >>> get_close_matches("zebra", ["apple", "melon"])
    []
    """
    if not n > 0:
        raise ValueError("n must be > 0: %r" % (n,))
    if not 0.0 <= cutoff <= 1.0:
        raise ValueError("cutoff must be in [0.0, 1.0]: %r" % (cutoff,))
    scored = []
    matcher = SequenceMatcher()
    matcher.set_seq2(word)
    for candidate in possibilities:
        matcher.set_seq1(candidate)
        if (matcher.real_quick_ratio() >= cutoff and
                matcher.quick_ratio() >= cutoff):
            score = matcher.ratio()
            if score >= cutoff:
                scored.append((score, candidate))
    return [candidate for score, candidate in _nlargest(n, scored)]


# ---------------------------------------------------------------------------
# Differ

def _guide_whitespace(line, marks):
    """Copy the tabs and other whitespace of line into the positions of a
    hint line that are unmarked, so the hint stays aligned on screen."""
    return ''.join(char if mark == ' ' and char.isspace() else mark
                   for char, mark in zip(line, marks))


class Differ:
    """Compare two sequences of lines of text and produce a human-readable
    delta.

    Every line of the result starts with a two-letter code:

        '- '  the line is only in the first sequence
        '+ '  the line is only in the second sequence
        '  '  the line is in both
        '? '  a hint line pointing at the changed characters of the line
              above it ('^' changed, '-' removed, '+' added); it is absent
              from both inputs

    Lines are compared with a SequenceMatcher; inside a block of replaced
    lines, pairs that are similar enough are compared character by
    character, which is where the hint lines come from.  The input lines
    should end with newlines (as readlines() returns them); otherwise the
    hint lines are hard to read.

    >>> first = ['one\\n', 'two\\n', 'three\\n']
    >>> second = ['one\\n', 'tree\\n', 'emu\\n']
    >>> print(''.join(Differ().compare(first, second)), end='')
      one
    - two
    - three
    ?  -
    + tree
    + emu
    """

    def __init__(self, linejunk=None, charjunk=None):
        """linejunk and charjunk are optional junk predicates for lines and
        for characters, passed on to the SequenceMatchers used."""
        self.linejunk = linejunk
        self.charjunk = charjunk

    def compare(self, a, b):
        """Yield the delta lines between the line sequences a and b."""
        matcher = SequenceMatcher(self.linejunk, a, b)
        for tag, alo, ahi, blo, bhi in matcher.get_opcodes():
            if tag == 'replace':
                pieces = self._similar_replace(a, alo, ahi, b, blo, bhi)
            elif tag == 'delete':
                pieces = self._prefixed('-', a, alo, ahi)
            elif tag == 'insert':
                pieces = self._prefixed('+', b, blo, bhi)
            elif tag == 'equal':
                pieces = self._prefixed(' ', a, alo, ahi)
            else:
                raise ValueError('unknown tag %r' % (tag,))
            yield from pieces

    def _prefixed(self, code, lines, lo, hi):
        for index in range(lo, hi):
            yield f'{code} {lines[index]}'

    def _block_replace(self, a, alo, ahi, b, blo, bhi):
        # Show the smaller side first: easier to read.
        if bhi - blo < ahi - alo:
            yield from self._prefixed('+', b, blo, bhi)
            yield from self._prefixed('-', a, alo, ahi)
        else:
            yield from self._prefixed('-', a, alo, ahi)
            yield from self._prefixed('+', b, blo, bhi)

    def _leftover(self, a, alo, ahi, b, blo, bhi):
        if alo < ahi:
            if blo < bhi:
                yield from self._block_replace(a, alo, ahi, b, blo, bhi)
            else:
                yield from self._prefixed('-', a, alo, ahi)
        elif blo < bhi:
            yield from self._prefixed('+', b, blo, bhi)

    # Lines must be at least this similar to be shown as a changed pair.
    _PAIR_CUTOFF = 0.74999
    # How far from the "diagonal" a partner line is looked for.
    _PAIR_WINDOW = 10

    def _similar_replace(self, a, alo, ahi, b, blo, bhi):
        """Replace a[alo:ahi] by b[blo:bhi], pairing up similar lines.

        The lines of b are taken in order; for each, the most similar line
        of a not yet used, near the corresponding position, becomes its
        partner if it is similar enough.  Lines between partners are shown
        as plain deletions and insertions.
        """
        chars = SequenceMatcher(self.charjunk)
        cutoff = self._PAIR_CUTOFF
        window = self._PAIR_WINDOW
        next_a, next_b = alo, blo         # first lines not yet shown
        for j in range(blo, bhi):
            chars.set_seq2(b[j])
            middle = alo + (j - blo)
            candidates = range(max(middle - window, next_a),
                               min(middle + window + 1, ahi))
            if not candidates:
                break
            partner = None
            best = cutoff
            for i in candidates:
                chars.set_seq1(a[i])
                if (chars.real_quick_ratio() > best and
                        chars.quick_ratio() > best):
                    score = chars.ratio()
                    if score > best:
                        partner, best = i, score
            if partner is None:
                continue
            yield from self._leftover(a, next_a, partner, b, next_b, j)
            old, new = a[partner], b[j]
            if old == new:
                yield '  ' + old
            else:
                old_marks = []
                new_marks = []
                chars.set_seqs(old, new)
                for tag, i1, i2, j1, j2 in chars.get_opcodes():
                    width_a, width_b = i2 - i1, j2 - j1
                    if tag == 'equal':
                        old_marks.append(' ' * width_a)
                        new_marks.append(' ' * width_b)
                    elif tag == 'replace':
                        old_marks.append('^' * width_a)
                        new_marks.append('^' * width_b)
                    elif tag == 'delete':
                        old_marks.append('-' * width_a)
                    elif tag == 'insert':
                        new_marks.append('+' * width_b)
                    else:
                        raise ValueError('unknown tag %r' % (tag,))
                yield from self._hinted(old, new, ''.join(old_marks),
                                        ''.join(new_marks))
            next_a, next_b = partner + 1, j + 1
        yield from self._leftover(a, next_a, ahi, b, next_b, bhi)

    def _hinted(self, old, new, old_marks, new_marks):
        old_marks = _guide_whitespace(old, old_marks).rstrip()
        new_marks = _guide_whitespace(new, new_marks).rstrip()
        yield '- ' + old
        if old_marks:
            yield f'? {old_marks}\n'
        yield '+ ' + new
        if new_marks:
            yield f'? {new_marks}\n'


def IS_LINE_JUNK(line, pat=None):
    """Return True for a line that is blank or holds a single '#' (with
    optional surrounding whitespace).  With pat given, return whether
    pat(line) matched (is not None) instead.

    >>> IS_LINE_JUNK('   \\n'), IS_LINE_JUNK('  # \\n'), IS_LINE_JUNK('x = 1\\n')
    (True, True, False)
    """
    if pat is None:
        return line.strip() in ('', '#')
    return pat(line) is not None


def IS_CHARACTER_JUNK(ch, ws=" \t"):
    """Return True for a blank or a tab (the characters in ws).

    >>> IS_CHARACTER_JUNK('\\t'), IS_CHARACTER_JUNK('\\n'), IS_CHARACTER_JUNK('x')
    (True, False, False)
    """
    return ch in ws


# ---------------------------------------------------------------------------
# unified and context diffs

def _format_range_unified(start, stop):
    """The "line[,count]" range of a unified diff hunk header (1-based;
    an empty range names the line before it)."""
    first = start + 1
    count = stop - start
    if count == 1:
        return str(first)
    if not count:
        first -= 1
    return f'{first},{count}'


def _format_range_context(start, stop):
    """The "first[,last]" range of a context diff hunk header."""
    first = start + 1
    count = stop - start
    if not count:
        first -= 1
    if count <= 1:
        return str(first)
    return f'{first},{first + count - 1}'


def _header_dates(fromfiledate, tofiledate):
    return (f'\t{fromfiledate}' if fromfiledate else '',
            f'\t{tofiledate}' if tofiledate else '')


def unified_diff(a, b, fromfile='', tofile='', fromfiledate='',
                 tofiledate='', n=3, lineterm='\n', *, color=False):
    """Yield the differences between the line lists a and b in the
    unified diff format, with n lines of context around changes.

    The file names and dates go into the "---"/"+++" header lines (a date
    is joined to its name with a tab).  Header and hunk lines end with
    lineterm; the content lines are passed on as they are, so for inputs
    without line endings use lineterm=''.  color=True adds ANSI colors.

    >>> old = ['alpha\\n', 'beta\\n', 'gamma\\n', 'delta\\n']
    >>> new = ['alpha\\n', 'BETA\\n', 'gamma\\n', 'delta\\n', 'epsilon\\n']
    >>> print(''.join(unified_diff(old, new, 'v1', 'v2', n=1)), end='')
    --- v1
    +++ v2
    @@ -1,4 +1,5 @@
     alpha
    -beta
    +BETA
     gamma
     delta
    +epsilon
    """
    _check_types(a, b, fromfile, tofile, fromfiledate, tofiledate, lineterm)
    if color:
        from _colorize import get_theme
        theme = get_theme(force_color=True).difflib
        header, hunk, reset = theme.header, theme.hunk, theme.reset
        kept, removed, added = theme.context, theme.removed, theme.added
    else:
        header = hunk = reset = kept = removed = added = ''
    started = False
    for group in SequenceMatcher(None, a, b).get_grouped_opcodes(n):
        if not started:
            started = True
            fromdate, todate = _header_dates(fromfiledate, tofiledate)
            yield f'{header}--- {fromfile}{fromdate}{reset}{lineterm}'
            yield f'{header}+++ {tofile}{todate}{reset}{lineterm}'
        first, last = group[0], group[-1]
        old_range = _format_range_unified(first[1], last[2])
        new_range = _format_range_unified(first[3], last[4])
        yield f'{hunk}@@ -{old_range} +{new_range} @@{reset}{lineterm}'
        for tag, i1, i2, j1, j2 in group:
            if tag == 'equal':
                for line in a[i1:i2]:
                    yield f'{kept} {line}{reset}'
                continue
            if tag != 'insert':
                for line in a[i1:i2]:
                    yield f'{removed}-{line}{reset}'
            if tag != 'delete':
                for line in b[j1:j2]:
                    yield f'{added}+{line}{reset}'


_CONTEXT_MARK = {'insert': '+ ', 'delete': '- ', 'replace': '! ',
                 'equal': '  '}


def context_diff(a, b, fromfile='', tofile='', fromfiledate='',
                 tofiledate='', n=3, lineterm='\n'):
    """Yield the differences between the line lists a and b in the
    context diff format, with n lines of context around changes.

    Each hunk shows the old lines, then the new lines, marked '! '
    (changed), '- ' (removed), '+ ' (added) or '  ' (context); a side
    without changes is left out.  Names, dates and lineterm work as for
    unified_diff().

    >>> old = ['red\\n', 'green\\n', 'blue\\n']
    >>> new = ['red\\n', 'teal\\n', 'blue\\n', 'pink\\n']
    >>> print(''.join(context_diff(old, new, 'before', 'after')), end='')
    *** before
    --- after
    ***************
    *** 1,3 ****
      red
    ! green
      blue
    --- 1,4 ----
      red
    ! teal
      blue
    + pink
    """
    _check_types(a, b, fromfile, tofile, fromfiledate, tofiledate, lineterm)
    started = False
    for group in SequenceMatcher(None, a, b).get_grouped_opcodes(n):
        if not started:
            started = True
            fromdate, todate = _header_dates(fromfiledate, tofiledate)
            yield f'*** {fromfile}{fromdate}{lineterm}'
            yield f'--- {tofile}{todate}{lineterm}'
        first, last = group[0], group[-1]
        yield '***************' + lineterm
        yield f'*** {_format_range_context(first[1], last[2])} ****{lineterm}'
        if any(code[0] in ('replace', 'delete') for code in group):
            for tag, i1, i2, _, _ in group:
                if tag != 'insert':
                    mark = _CONTEXT_MARK[tag]
                    for line in a[i1:i2]:
                        yield mark + line
        yield f'--- {_format_range_context(first[3], last[4])} ----{lineterm}'
        if any(code[0] in ('replace', 'insert') for code in group):
            for tag, _, _, j1, j2 in group:
                if tag != 'delete':
                    mark = _CONTEXT_MARK[tag]
                    for line in b[j1:j2]:
                        yield mark + line


def _check_types(a, b, *args):
    # Mixing str and bytes would produce a garbled diff (or fail deep down);
    # refuse it up front with a precise message.
    for lines in (a, b):
        if lines and not isinstance(lines[0], str):
            raise TypeError('lines to compare must be str, not %s (%r)'
                            % (type(lines[0]).__name__, lines[0]))
    for lines in (a, b):
        if isinstance(lines, str):
            raise TypeError('input must be a sequence of strings, not %s'
                            % type(lines).__name__)
    for arg in args:
        if not isinstance(arg, str):
            raise TypeError('all arguments must be str, not: %r' % (arg,))


def diff_bytes(dfunc, a, b, fromfile=b'', tofile=b'', fromfiledate=b'',
               tofiledate=b'', n=3, lineterm=b'\n'):
    """Run dfunc (unified_diff or context_diff) on bytes.

    All inputs (lines, names, dates, lineterm) must be bytes; they are
    passed through losslessly (decoded as ASCII with surrogateescape and
    encoded back), so the output is bytes in the inputs' encodings.
    """
    def as_text(value):
        try:
            return value.decode('ascii', 'surrogateescape')
        except AttributeError as exc:
            raise TypeError('all arguments must be bytes, not %s (%r)'
                            % (type(value).__name__, value)) from exc

    a = [as_text(line) for line in a]
    b = [as_text(line) for line in b]
    lines = dfunc(a, b, as_text(fromfile), as_text(tofile),
                  as_text(fromfiledate), as_text(tofiledate), n,
                  as_text(lineterm))
    for line in lines:
        yield line.encode('ascii', 'surrogateescape')


def ndiff(a, b, linejunk=None, charjunk=IS_CHARACTER_JUNK):
    """Return a Differ-style delta of the line lists a and b (a generator).

    linejunk and charjunk are the junk predicates for lines and characters;
    by default no line is junk and blanks and tabs are character junk.

    >>> delta = ndiff(['one\\n', 'two\\n'], ['one\\n', 'too\\n'])
    >>> print(''.join(delta), end='')
      one
    - two
    ?  ^
    + too
    ?  ^
    """
    return Differ(linejunk, charjunk).compare(a, b)


def restore(delta, which):
    """Recover one of the two line sequences an ndiff()/Differ delta was
    made from: which=1 for the first, which=2 for the second.

    >>> delta = list(ndiff(['ham\\n', 'eggs\\n'], ['ham\\n', 'spam\\n']))
    >>> print(''.join(restore(delta, 1)), end='')
    ham
    eggs
    >>> print(''.join(restore(delta, 2)), end='')
    ham
    spam
    """
    codes = {1: '- ', 2: '+ '}
    try:
        own = codes[int(which)]
    except (KeyError, ValueError, TypeError):
        raise ValueError('unknown delta choice (must be 1 or 2): %r'
                         % (which,)) from None
    for line in delta:
        if line[:2] in ('  ', own):
            yield line[2:]


# ---------------------------------------------------------------------------
# Side-by-side rows (used by HtmlDiff)
#
# A row is ((number, text), (number, text), changed).  Inside the texts,
# "\0+", "\0-" and "\0^" open an added/removed/changed span and "\1" closes
# it.  A filler cell is ('', '\n').  With context, rows far from changes
# are dropped and (None, None, None) marks each gap.

def _marked(text, code):
    return '\0' + code + (text or ' ') + '\1'


def _apply_hints(text, hints):
    # Wrap each run of '+', '-' or '^' in the hint line around the same
    # columns of text (the two-character prefixes line up).
    spans = []
    index, size = 0, len(hints)
    while index < size:
        code = hints[index]
        if code in '+-^':
            end = index + 1
            while end < size and hints[end] == code:
                end += 1
            spans.append((index, end, code))
            index = end
        else:
            index += 1
    for start, end, code in reversed(spans):
        text = (text[:start] + '\0' + code + text[start:end] + '\1' +
                text[end:])
    return text[2:]


class _RowBuilder:
    """Turn an ndiff delta into side-by-side rows.

    The delta is read with a lookahead of four lines, classified by their
    two-letter codes.  Old and new cells are queued separately and paired
    up in order; filler cells are queued to realign both sides at the
    points where a run of changes meets unchanged text.
    """

    _FILLER = ('', '\n')

    # (code prefix of the lookahead, action), tried in order.
    #   pair:     a changed pair, (old, new) cell styles; '?' consumes a
    #             hint line, None is plain text
    #   old/new:  a lone removed/added line; "sync" first realigns the
    #             sides (counting this line), otherwise the imbalance is
    #             only recorded
    _RULES = (
        ('-?+?', ('pair', '?', '?')),
        ('--++', ('old', False)),
        ('--?+', ('old', True)),
        ('--+', ('old', True)),
        ('- ', ('old', True)),
        ('-+?', ('pair', None, '?')),
        ('-?+', ('pair', '?', None)),
        ('-', ('old', False)),
        ('+--', ('new', False)),
        ('+ ', ('new', True)),
        ('+-', ('new', True)),
        ('+', ('new', False)),
        (' ', ('same',)),
    )

    def __init__(self, delta):
        self._delta = iter(delta)
        self._ahead = []
        self._numbers = [0, 0]
        self._old = []
        self._new = []
        self._balance = 0      # new cells minus old cells since last sync

    def _cell(self, side, style):
        self._numbers[side] += 1
        line = self._ahead.pop(0)
        if style is None:
            text = line[2:]
        elif style == '?':
            text = _apply_hints(line, self._ahead.pop(0))
        else:
            text = _marked(line[2:], style)
        return (self._numbers[side], text)

    def _realign(self, extra):
        missing = self._balance + extra
        if missing < 0:
            self._new.extend([(self._FILLER, True)] * -missing)
        elif missing > 0:
            self._old.extend([(self._FILLER, True)] * missing)
        self._balance = 0

    def rows(self):
        ahead = self._ahead
        old, new = self._old, self._new
        while True:
            while len(ahead) < 4:
                ahead.append(next(self._delta, 'X'))
            codes = ''.join(line[0] for line in ahead)
            if codes[0] == 'X':
                self._realign(0)
            else:
                for prefix, action in self._RULES:
                    if codes.startswith(prefix):
                        break
                else:
                    raise ValueError('unexpected delta line %r' % (ahead[0],))
                kind = action[0]
                if kind == 'pair':
                    old.append((self._cell(0, action[1]), True))
                    new.append((self._cell(1, action[2]), True))
                elif kind == 'same':
                    # one delta line provides both cells
                    ahead.insert(0, ahead[0])
                    old.append((self._cell(0, None), False))
                    new.append((self._cell(1, None), False))
                elif kind == 'old':
                    if action[1]:
                        self._realign(-1)
                    else:
                        self._balance -= 1
                    old.append((self._cell(0, '-'), True))
                else:
                    if action[1]:
                        self._realign(1)
                    else:
                        self._balance += 1
                    new.append((self._cell(1, '+'), True))
            while old and new:
                (left, left_changed) = old.pop(0)
                (right, right_changed) = new.pop(0)
                yield left, right, left_changed or right_changed
            if codes[0] == 'X':
                return


def _context_rows(rows, context):
    # Keep the changed rows plus up to `context` rows before and after
    # each; a (None, None, None) row stands for every gap.
    rows = iter(rows)
    before = []
    skipped = False
    for row in rows:
        if not row[2]:
            before.append(row)
            if len(before) > context:
                del before[0]
                skipped = True
            continue
        if skipped:
            yield None, None, None
        yield from before
        yield row
        before = []
        skipped = False
        remaining = context
        while remaining:
            row = next(rows, None)
            if row is None:
                return
            yield row
            remaining = context if row[2] else remaining - 1


def _mdiff(fromlines, tolines, context=None, linejunk=None,
           charjunk=IS_CHARACTER_JUNK):
    """Yield side-by-side rows for HtmlDiff (see the format above).

    context is None for all rows, or the number of unchanged rows to keep
    around each change.
    """
    rows = _RowBuilder(ndiff(fromlines, tolines, linejunk, charjunk)).rows()
    if context is None:
        yield from rows
    else:
        yield from _context_rows(rows, context)


# ---------------------------------------------------------------------------
# HtmlDiff
#
# The page, table and legend markup below is the documented output format.

_PAGE = """
<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="%(charset)s">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>Diff comparison</title>
    <style>%(styles)s
    </style>
</head>

<body>
    %(table)s%(legend)s
</body>

</html>"""

_CSS = """
        :root {color-scheme: light dark}
        table.diff {
            font-family: Menlo, Consolas, Monaco, Liberation Mono, Lucida Console, monospace;
            border: medium;
        }
        .diff_header {
            background-color: #e0e0e0;
            font-weight: bold;
        }
        td.diff_header {
            text-align: right;
            padding: 0 8px;
        }
        .diff_next {
            background-color: #c0c0c0;
            padding: 4px 0;
        }
        .diff_add {background-color:palegreen}
        .diff_chg {background-color:#ffff77}
        .diff_sub {background-color:#ffaaaa}
        table.diff[summary="Legends"] {
            margin-top: 20px;
            border: 1px solid #ccc;
        }
        table.diff[summary="Legends"] th {
            background-color: #e0e0e0;
            padding: 4px 8px;
        }
        table.diff[summary="Legends"] td {
            padding: 4px 8px;
        }

        @media (prefers-color-scheme: dark) {
            .diff_header {background-color:#666}
            .diff_next {background-color:#393939}
            .diff_add {background-color:darkgreen}
            .diff_chg {background-color:#847415}
            .diff_sub {background-color:darkred}
            table.diff[summary="Legends"] {border-color:#555}
            table.diff[summary="Legends"] th{background-color:#666}
        }"""

_TABLE = """
    <table class="diff" id="difflib_chg_%(prefix)s_top"
           cellspacing="0" cellpadding="0" rules="groups" >
        <colgroup></colgroup> <colgroup></colgroup> <colgroup></colgroup>
        <colgroup></colgroup> <colgroup></colgroup> <colgroup></colgroup>
        %(header_row)s
        <tbody>
%(data_rows)s        </tbody>
    </table>"""

_LEGEND = """
    <table class="diff" summary="Legends">
        <tr> <th colspan="2"> Legends </th> </tr>
        <tr> <td> <table border="" summary="Colors">
                      <tr><th> Colors </th> </tr>
                      <tr><td class="diff_add">&nbsp;Added&nbsp;</td></tr>
                      <tr><td class="diff_chg">Changed</td> </tr>
                      <tr><td class="diff_sub">Deleted</td> </tr>
                  </table></td>
             <td> <table border="" summary="Links">
                      <tr><th colspan="2"> Links </th> </tr>
                      <tr><td>(f)irst change</td> </tr>
                      <tr><td>(n)ext change</td> </tr>
                      <tr><td>(t)op</td> </tr>
                  </table></td> </tr>
    </table>"""

_ROW = ('            <tr><td class="diff_next"%s>%s</td>%s'
        '<td class="diff_next">%s</td>%s</tr>\n')
_GAP = '        </tbody>        \n        <tbody>\n'
_SPANS = (('\0+', '<span class="diff_add">'),
          ('\0-', '<span class="diff_sub">'),
          ('\0^', '<span class="diff_chg">'),
          ('\1', '</span>'),
          ('\t', '&nbsp;'))


def _wrap_point(text, limit):
    """Return (index, open_code): where to cut text so that at most limit
    visible characters come before the cut (the span markers take no
    room), and the code of the span still open there ('' if none)."""
    index = shown = 0
    size = len(text)
    open_code = ''
    while shown < limit and index < size:
        char = text[index]
        if char == '\0':
            open_code = text[index + 1]
            index += 2
        elif char == '\1':
            open_code = ''
            index += 1
        else:
            index += 1
            shown += 1
    return index, open_code


class HtmlDiff:
    """Render a side-by-side comparison of two lists of lines as HTML.

    make_table() returns a table, make_file() a complete page that includes
    the table and a legend.  Changed lines are highlighted with intraline
    markup; each table carries anchors and links to jump between changes.

    The class attributes _file_template, _styles, _table_template and
    _legend hold the markup and may be replaced in a subclass.
    """

    _file_template = _PAGE
    _styles = _CSS
    _table_template = _TABLE
    _legend = _LEGEND
    # Counter that makes the anchors of every table unique.
    _default_prefix = 0

    def __init__(self, tabsize=8, wrapcolumn=None, linejunk=None,
                 charjunk=IS_CHARACTER_JUNK):
        """tabsize: columns per tab stop for tab expansion;
        wrapcolumn: wrap lines longer than this (None: no wrapping);
        linejunk, charjunk: junk predicates passed on to ndiff()."""
        self._tabsize = tabsize
        self._wrapcolumn = wrapcolumn
        self._linejunk = linejunk
        self._charjunk = charjunk

    def make_file(self, fromlines, tolines, fromdesc='', todesc='',
                  context=False, numlines=5, *, charset='utf-8'):
        """Return a complete HTML page comparing fromlines with tolines.

        fromdesc/todesc are the column headings; with context=True only
        changes and numlines lines around them are shown (numlines also
        places the targets of the "next change" links); characters the
        charset cannot encode become character references.
        """
        page = self._file_template % dict(
            styles=self._styles, legend=self._legend, charset=charset,
            table=self.make_table(fromlines, tolines, fromdesc, todesc,
                                  context=context, numlines=numlines))
        return page.encode(charset, 'xmlcharrefreplace').decode(charset)

    def make_table(self, fromlines, tolines, fromdesc='', todesc='',
                   context=False, numlines=5):
        """Return an HTML table comparing fromlines with tolines (see
        make_file() for the arguments)."""
        self._next_prefix()
        old = [self._visible_tabs(line) for line in fromlines]
        new = [self._visible_tabs(line) for line in tolines]
        rows = _mdiff(old, new, numlines if context else None,
                      linejunk=self._linejunk, charjunk=self._charjunk)
        if self._wrapcolumn:
            rows = self._wrapped(rows)
        cells_old, cells_new, flags = self._cells(rows)
        ids, links, cells_old, cells_new, flags = self._navigation(
            cells_old, cells_new, flags, context, numlines)
        body = []
        for index, flag in enumerate(flags):
            if flag is None:
                if index:
                    body.append(_GAP)
            else:
                body.append(_ROW % (ids[index], links[index],
                                    cells_old[index], links[index],
                                    cells_new[index]))
        if fromdesc or todesc:
            header_row = (
                '<thead><tr><th class="diff_next"><br /></th>'
                f'<th colspan="2" class="diff_header">{fromdesc}</th>'
                '<th class="diff_next"><br /></th>'
                f'<th colspan="2" class="diff_header">{todesc}</th>'
                '</tr></thead>')
        else:
            header_row = ''
        table = self._table_template % dict(
            data_rows=''.join(body), header_row=header_row,
            prefix=self._prefix[1])
        for marker, markup in _SPANS:
            table = table.replace(marker, markup)
        return table

    def _next_prefix(self):
        number = HtmlDiff._default_prefix
        HtmlDiff._default_prefix += 1
        self._prefix = ['from%d_' % number, 'to%d_' % number]

    def _visible_tabs(self, line):
        # Expand tabs, keeping the expansion distinguishable from real
        # blanks (as '\t', rendered as &nbsp; at the very end) so that a
        # tab/space change shows up; drop trailing newlines.
        line = line.replace(' ', '\0').expandtabs(self._tabsize)
        return line.replace(' ', '\t').replace('\0', ' ').rstrip('\n')

    def _wrapped(self, rows):
        filler = ('', ' ')
        for old, new, flag in rows:
            if flag is None:
                yield old, new, flag
                continue
            old_parts = self._split(*old)
            new_parts = self._split(*new)
            for index in range(max(len(old_parts), len(new_parts))):
                yield (old_parts[index] if index < len(old_parts) else filler,
                       new_parts[index] if index < len(new_parts) else filler,
                       flag)

    def _split(self, number, text):
        limit = self._wrapcolumn
        if not number:
            return [(number, text)]
        parts = []
        while (len(text) > limit and
               len(text) - 3 * text.count('\0') > limit):
            cut, open_code = _wrap_point(text, limit)
            head, text = text[:cut], text[cut:]
            if open_code:
                head += '\1'
                text = '\0' + open_code + text
            parts.append((number, head))
            number = '>'
        parts.append((number, text))
        return parts

    def _cells(self, rows):
        cells_old, cells_new, flags = [], [], []
        for old, new, flag in rows:
            if flag is None:
                cells_old.append(None)
                cells_new.append(None)
            else:
                cells_old.append(self._cell(0, flag, *old))
                cells_new.append(self._cell(1, flag, *new))
            flags.append(flag)
        return cells_old, cells_new, flags

    def _cell(self, side, changed, number, text):
        if isinstance(number, int):
            number = '%d' % number
            anchor = f' id="{self._prefix[side]}{number}"'
        else:
            anchor = ''
        text = text.replace('&', '&amp;').replace('>', '&gt;')
        text = text.replace('<', '&lt;').replace(' ', '&nbsp;').rstrip()
        style = ' class="diff_changed"' if changed else ''
        return (f'<td class="diff_header"{anchor}>{number}</td>'
                f'<td{style} nowrap="nowrap">{text}</td>')

    def _navigation(self, cells_old, cells_new, flags, context, numlines):
        # Each block of changed rows gets an anchor numlines rows above it,
        # and its first row links to the next block ("n"); the first row
        # links to the first block ("f") unless a block starts there, and
        # the last block links back to the top of the table ("t").
        target = self._prefix[1]
        ids = [''] * len(flags)
        links = [''] * len(flags)
        blocks = 0
        last_start = 0
        inside = False
        for index, flag in enumerate(flags):
            if not flag:
                inside = False
                continue
            if inside:
                continue
            inside = True
            last_start = index
            ids[max(0, index - numlines)] = (
                f' id="difflib_chg_{target}_{blocks}"')
            blocks += 1
            links[index] = f'<a href="#difflib_chg_{target}_{blocks}">n</a>'
        if not flags:
            note = 'No Differences Found' if context else 'Empty File'
            cells_old = cells_new = [f'<td></td><td>&nbsp;{note}&nbsp;</td>']
            flags = [False]
            ids = ['']
            links = ['']
            last_start = 0
        if not flags[0]:
            links[0] = f'<a href="#difflib_chg_{target}_0">f</a>'
        links[last_start] = f'<a href="#difflib_chg_{target}_top">t</a>'
        return ids, links, cells_old, cells_new, flags
