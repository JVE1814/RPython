"""Constants and the exception class shared by the regular expression package.

The pattern parser and compiler of this implementation live in the `_sre`
extension module; this module only keeps the public constants that code
outside the package relies on.
"""

from _sre import MAXREPEAT, MAXGROUPS, MAGIC

__all__ = ['MAXREPEAT', 'MAXGROUPS', 'MAGIC', 'PatternError', 'error',
           'SRE_FLAG_IGNORECASE', 'SRE_FLAG_LOCALE', 'SRE_FLAG_MULTILINE',
           'SRE_FLAG_DOTALL', 'SRE_FLAG_UNICODE', 'SRE_FLAG_VERBOSE',
           'SRE_FLAG_DEBUG', 'SRE_FLAG_ASCII', 'SRE_INFO_PREFIX',
           'SRE_INFO_LITERAL', 'SRE_INFO_CHARSET']


class PatternError(Exception):
    """Exception raised for invalid regular expressions.

    Attributes:

        msg: The unformatted error message
        pattern: The regular expression pattern
        pos: The index in the pattern where compilation failed (may be None)
        lineno: The line corresponding to pos (may be None)
        colno: The column corresponding to pos (may be None)
    """

    __module__ = 're'

    def __init__(self, msg, pattern=None, pos=None):
        self.msg = msg
        self.pattern = pattern
        self.pos = pos
        if pattern is not None and pos is not None:
            text = '%s at position %d' % (msg, pos)
            nl = '\n' if isinstance(pattern, str) else b'\n'
            self.lineno = pattern.count(nl, 0, pos) + 1
            self.colno = pos - pattern.rfind(nl, 0, pos)
            if nl in pattern:
                text = '%s (line %d, column %d)' % (text, self.lineno, self.colno)
        else:
            text = msg
            self.lineno = None
            self.colno = None
        super().__init__(text)


# Backwards compatible alias.
error = PatternError


class _NamedIntConstant(int):
    def __new__(cls, value, name):
        self = super().__new__(cls, value)
        self.name = name
        return self

    def __repr__(self):
        return self.name

    __reduce__ = None


MAXREPEAT = _NamedIntConstant(MAXREPEAT, 'MAXREPEAT')

SRE_FLAG_IGNORECASE = 2
SRE_FLAG_LOCALE = 4
SRE_FLAG_MULTILINE = 8
SRE_FLAG_DOTALL = 16
SRE_FLAG_UNICODE = 32
SRE_FLAG_VERBOSE = 64
SRE_FLAG_DEBUG = 128
SRE_FLAG_ASCII = 256

SRE_INFO_PREFIX = 1
SRE_INFO_LITERAL = 2
SRE_INFO_CHARSET = 4
