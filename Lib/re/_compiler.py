"""Regular expression compilation entry point.

Parsing and code generation happen inside the `_sre` module; this module
keeps the helper names other code imports from `re._compiler`.
"""

import _sre
from ._constants import *
from ._constants import MAXREPEAT, MAXGROUPS, PatternError

assert _sre.MAGIC == MAGIC, "SRE module mismatch"


def isstring(obj):
    return isinstance(obj, (str, bytes))


def compile(p, flags=0):
    """Compile the pattern `p` (str or bytes) with `flags` to a Pattern object."""
    if not isstring(p):
        raise TypeError("first argument must be string or compiled pattern")
    return _sre.compile(p, flags, [], 0, {}, ())


def _generate_overlap_table(prefix):
    """Return the KMP failure table of `prefix`.

    Entry i is the length of the longest proper suffix of prefix[:i+1]
    that is also a prefix of `prefix`.
    """
    n = len(prefix)
    table = [0] * n
    k = 0
    for i in range(1, n):
        while k and prefix[i] != prefix[k]:
            k = table[k - 1]
        if prefix[i] == prefix[k]:
            k += 1
        table[i] = k
    return table
