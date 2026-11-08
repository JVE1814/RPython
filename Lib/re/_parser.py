"""Compatibility shim for `re._parser`.

The regular expression parser is implemented in the `_sre` module.  Only the
character class constants that third-party code occasionally imports are
provided here.
"""

from ._constants import *

SPECIAL_CHARS = ".\\[{()*+?^$|"
REPEAT_CHARS = "*+?{"

DIGITS = frozenset("0123456789")
OCTDIGITS = frozenset("01234567")
HEXDIGITS = frozenset("0123456789abcdefABCDEF")
ASCIILETTERS = frozenset("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ")
WHITESPACE = frozenset(" \t\n\r\v\f")

FLAGS = {
    # standard flags
    "i": SRE_FLAG_IGNORECASE,
    "L": SRE_FLAG_LOCALE,
    "m": SRE_FLAG_MULTILINE,
    "s": SRE_FLAG_DOTALL,
    "x": SRE_FLAG_VERBOSE,
    # extensions
    "a": SRE_FLAG_ASCII,
    "u": SRE_FLAG_UNICODE,
}

TYPE_FLAGS = SRE_FLAG_ASCII | SRE_FLAG_LOCALE | SRE_FLAG_UNICODE
GLOBAL_FLAGS = SRE_FLAG_DEBUG


class Verbose(Exception):
    pass


# The parse-tree API (re._parse_tree), loaded on first use: `re` itself never
# needs it.
from . import _constants as _c
globals().update({_k: _v for _k, _v in vars(_c).items()
                  if _k.isupper() and not _k.startswith("_")})
del _c


def parse(str, flags=0, state=None):
    """Parse tree of the pattern `str` (see re._parse_tree)."""
    from ._parse_tree import parse as _parse
    return _parse(str, flags, state)


def __getattr__(name):
    if name in ("SubPattern", "State"):
        from . import _parse_tree
        return getattr(_parse_tree, name)
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")
