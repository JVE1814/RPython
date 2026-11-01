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
