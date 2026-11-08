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


# Node and argument codes of the parse trees `re._parser.parse()` returns.
# Third-party code (hypothesis' from_regex, regex generators) walks those trees
# and compares against these names; this implementation's matcher does not
# use them, so only the names and their distinctness matter.

def _named_codes(names, start=0):
    codes = []
    for i, name in enumerate(names.split()):
        code = _NamedIntConstant(start + i, name)
        globals()[name] = code
        codes.append(code)
    return codes


OPCODES = _named_codes("""
    FAILURE SUCCESS ANY ANY_ALL ASSERT ASSERT_NOT AT BRANCH CATEGORY CHARSET
    BIGCHARSET GROUPREF GROUPREF_EXISTS IN INFO JUMP LITERAL MARK MAX_UNTIL
    MIN_UNTIL NOT_LITERAL NEGATE RANGE REPEAT REPEAT_ONE SUBPATTERN
    MIN_REPEAT_ONE ATOMIC_GROUP POSSESSIVE_REPEAT POSSESSIVE_REPEAT_ONE
    GROUPREF_IGNORE IN_IGNORE LITERAL_IGNORE NOT_LITERAL_IGNORE
    GROUPREF_LOC_IGNORE IN_LOC_IGNORE LITERAL_LOC_IGNORE NOT_LITERAL_LOC_IGNORE
    GROUPREF_UNI_IGNORE IN_UNI_IGNORE LITERAL_UNI_IGNORE NOT_LITERAL_UNI_IGNORE
    RANGE_UNI_IGNORE
""")
# repeats as the parser reports them (compiled into REPEAT/REPEAT_ONE forms)
MIN_REPEAT, MAX_REPEAT = _named_codes("MIN_REPEAT MAX_REPEAT", len(OPCODES))

ATCODES = _named_codes("""
    AT_BEGINNING AT_BEGINNING_LINE AT_BEGINNING_STRING AT_BOUNDARY
    AT_NON_BOUNDARY AT_END AT_END_LINE AT_END_STRING
    AT_LOC_BOUNDARY AT_LOC_NON_BOUNDARY AT_UNI_BOUNDARY AT_UNI_NON_BOUNDARY
""")

CHCODES = _named_codes("""
    CATEGORY_DIGIT CATEGORY_NOT_DIGIT CATEGORY_SPACE CATEGORY_NOT_SPACE
    CATEGORY_WORD CATEGORY_NOT_WORD CATEGORY_LINEBREAK CATEGORY_NOT_LINEBREAK
    CATEGORY_LOC_WORD CATEGORY_LOC_NOT_WORD CATEGORY_UNI_DIGIT
    CATEGORY_UNI_NOT_DIGIT CATEGORY_UNI_SPACE CATEGORY_UNI_NOT_SPACE
    CATEGORY_UNI_WORD CATEGORY_UNI_NOT_WORD CATEGORY_UNI_LINEBREAK
    CATEGORY_UNI_NOT_LINEBREAK
""")

del _named_codes
