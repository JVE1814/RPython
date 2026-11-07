"""Binary search in sorted sequences, and insertion that keeps them sorted.

All functions take optional lo/hi bounds (default: the whole sequence) and
an optional key function applied to the sequence's items (for insort*, also
to the inserted value).
"""


def bisect_left(a, x, lo=0, hi=None, *, key=None):
    """Return the first position in sorted a[lo:hi] where x could be
    inserted, i.e. before any items equal to x."""
    if lo < 0:
        raise ValueError('lo must be non-negative')
    if hi is None:
        hi = len(a)
    # invariant: a[:lo] < x <= a[hi:]
    while lo < hi:
        mid = (lo + hi) // 2
        item = a[mid] if key is None else key(a[mid])
        if item < x:
            lo = mid + 1
        else:
            hi = mid
    return lo


def bisect_right(a, x, lo=0, hi=None, *, key=None):
    """Return the last position in sorted a[lo:hi] where x could be
    inserted, i.e. after any items equal to x."""
    if lo < 0:
        raise ValueError('lo must be non-negative')
    if hi is None:
        hi = len(a)
    # invariant: a[:lo] <= x < a[hi:]
    while lo < hi:
        mid = (lo + hi) // 2
        item = a[mid] if key is None else key(a[mid])
        if x < item:
            hi = mid
        else:
            lo = mid + 1
    return lo


def insort_left(a, x, lo=0, hi=None, *, key=None):
    """Insert x into sorted a, before any items equal to it."""
    i = bisect_left(a, x if key is None else key(x), lo, hi, key=key)
    a.insert(i, x)


def insort_right(a, x, lo=0, hi=None, *, key=None):
    """Insert x into sorted a, after any items equal to it."""
    i = bisect_right(a, x if key is None else key(x), lo, hi, key=key)
    a.insert(i, x)


try:
    from _bisect import *
except ImportError:
    pass

bisect = bisect_right
insort = insort_right
