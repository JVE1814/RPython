"""ASCII character names and classification helpers.

Every predicate accepts either a one-character string or an integer code and
only answers True for codes inside the 7-bit ASCII range (except ismeta(),
which tests for the high bit).  ascii(), ctrl() and alt() return the same kind
of value they were given; unctrl() always returns a string.
"""

_NAMES = ("NUL SOH STX ETX EOT ENQ ACK BEL BS HT LF VT FF CR SO SI "
          "DLE DC1 DC2 DC3 DC4 NAK SYN ETB CAN EM SUB ESC FS GS RS US SP").split()

controlnames = list(_NAMES)

for _code, _name in enumerate(_NAMES):
    globals()[_name] = _code
del _code, _name

TAB = HT
NL = LF
DEL = 0x7F


def _code(c):
    return ord(c) if isinstance(c, str) else c


def _in(c, lo, hi):
    n = _code(c)
    return lo <= n <= hi


def isascii(c):
    return _in(c, 0, 127)


def isdigit(c):
    return _in(c, 48, 57)


def isupper(c):
    return _in(c, 65, 90)


def islower(c):
    return _in(c, 97, 122)


def isalpha(c):
    return isupper(c) or islower(c)


def isalnum(c):
    return isalpha(c) or isdigit(c)


def isxdigit(c):
    return isdigit(c) or _in(c, 65, 70) or _in(c, 97, 102)


def isblank(c):
    return _code(c) in (9, 32)


def isspace(c):
    n = _code(c)
    return n == 32 or 9 <= n <= 13


def isctrl(c):
    return _in(c, 0, 31)


def iscntrl(c):
    return isctrl(c) or _code(c) == 127


def isprint(c):
    return _in(c, 32, 126)


def isgraph(c):
    return _in(c, 33, 126)


def ispunct(c):
    return isgraph(c) and not isalnum(c)


def ismeta(c):
    return _code(c) > 127


def _same_kind(c, n):
    return chr(n) if isinstance(c, str) else n


def ascii(c):
    """Strip the high bit."""
    return _same_kind(c, _code(c) & 0x7F)


def ctrl(c):
    """The control character corresponding to *c*."""
    return _same_kind(c, _code(c) & 0x1F)


def alt(c):
    """Set the high (meta) bit."""
    return _same_kind(c, _code(c) | 0x80)


def unctrl(c):
    """Printable representation: '^X' for controls, '!' prefix for meta."""
    n = _code(c)
    low = n & 0x7F
    if low == 127:
        text = "^?"
    elif low < 32:
        text = "^" + chr(low + 64)
    else:
        text = chr(low)
    return "!" + text if n & 0x80 else text
