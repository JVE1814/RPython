"""Small helpers shared by the unittest modules."""

from collections import namedtuple, Counter
from os.path import commonprefix

__unittest = True

# Limits used when shortening reprs in failure messages.
_MAX_LENGTH = 80
_PLACEHOLDER_LEN = 12
_MIN_BEGIN_LEN = 5
_MIN_END_LEN = 5
_MIN_COMMON_LEN = 5
_MIN_DIFF_LEN = _MAX_LENGTH - (_MIN_BEGIN_LEN + _PLACEHOLDER_LEN +
                               _MIN_COMMON_LEN + _PLACEHOLDER_LEN +
                               _MIN_END_LEN)
assert _MIN_DIFF_LEN >= 0


def _shorten(s, prefixlen, suffixlen):
    """Replace the middle of *s* by '[N chars]' if that saves enough."""
    hidden = len(s) - prefixlen - suffixlen
    if hidden <= _PLACEHOLDER_LEN:
        return s
    return '%s[%d chars]%s' % (s[:prefixlen], hidden, s[len(s) - suffixlen:])


def _common_shorten_repr(*args):
    """Reprs of *args*, shortened so that the differing parts stay
    visible: a long common prefix is abbreviated first, then (if still too
    long) the tails."""
    reprs = tuple(safe_repr(arg) for arg in args)
    longest = max(len(r) for r in reprs)
    if longest <= _MAX_LENGTH:
        return reprs

    shared = commonprefix(reprs)
    shared_len = len(shared)
    # room left for the shared prefix if the tails are shown in full
    keep = _MAX_LENGTH - (longest - shared_len + _MIN_BEGIN_LEN +
                          _PLACEHOLDER_LEN)
    if keep > _MIN_COMMON_LEN:
        head = _shorten(shared, _MIN_BEGIN_LEN, keep)
        return tuple(head + r[shared_len:] for r in reprs)

    head = _shorten(shared, _MIN_BEGIN_LEN, _MIN_COMMON_LEN)
    return tuple(head + _shorten(r[shared_len:], _MIN_DIFF_LEN, _MIN_END_LEN)
                 for r in reprs)


def safe_repr(obj, short=False):
    """repr() that never raises, optionally truncated to _MAX_LENGTH."""
    try:
        text = repr(obj)
    except Exception:
        text = object.__repr__(obj)
    if short and len(text) >= _MAX_LENGTH:
        return text[:_MAX_LENGTH] + ' [truncated]...'
    return text


def strclass(cls):
    return '%s.%s' % (cls.__module__, cls.__qualname__)


def _unique_sorted(items):
    """Drop consecutive duplicates of an already sorted sequence."""
    result = []
    for item in items:
        if not result or not (result[-1] == item):
            result.append(item)
    return result


def sorted_list_difference(expected, actual):
    """Compare two sorted lists.

    Returns (missing, unexpected): the distinct items only in *expected* and
    the distinct items only in *actual*.
    """
    want = _unique_sorted(expected)
    have = _unique_sorted(actual)
    missing = []
    unexpected = []
    i = j = 0
    while i < len(want) and j < len(have):
        w, h = want[i], have[j]
        if w < h:
            missing.append(w)
            i += 1
        elif h < w:
            unexpected.append(h)
            j += 1
        else:
            i += 1
            j += 1
    missing.extend(want[i:])
    unexpected.extend(have[j:])
    return missing, unexpected


def unorderable_list_difference(expected, actual):
    """Like sorted_list_difference() for unsortable items (quadratic).

    Consumes *expected*; returns (missing, the rest of *actual*).
    """
    missing = []
    while expected:
        item = expected.pop()
        try:
            actual.remove(item)
        except ValueError:
            missing.append(item)
    return missing, actual


def three_way_cmp(x, y):
    """Return -1 if x < y, 0 if x == y and 1 if x > y."""
    return (x > y) - (x < y)


_Mismatch = namedtuple('Mismatch', 'actual expected value')


def _count_diff_all_purpose(actual, expected):
    """Elements whose counts differ, as (count in actual, count in expected,
    element) triples; works for unhashable elements."""
    first = list(actual)
    second = list(expected)
    n, m = len(first), len(second)
    used = object()
    diffs = []
    for i, elem in enumerate(first):
        if elem is used:
            continue
        count_first = 0
        for k in range(i, n):
            if first[k] == elem:
                count_first += 1
                first[k] = used
        count_second = 0
        for k in range(m):
            if second[k] is not used and second[k] == elem:
                count_second += 1
                second[k] = used
        if count_first != count_second:
            diffs.append(_Mismatch(count_first, count_second, elem))
    for i, elem in enumerate(second):
        if elem is used:
            continue
        count_second = 0
        for k in range(i, m):
            if second[k] == elem:
                count_second += 1
                second[k] = used
        diffs.append(_Mismatch(0, count_second, elem))
    return diffs


def _count_diff_hashable(actual, expected):
    """Like _count_diff_all_purpose() for hashable elements (fast)."""
    first = Counter(actual)
    second = Counter(expected)
    diffs = []
    for elem, count in first.items():
        other = second.get(elem, 0)
        if count != other:
            diffs.append(_Mismatch(count, other, elem))
    for elem, count in second.items():
        if elem not in first:
            diffs.append(_Mismatch(0, count, elem))
    return diffs
