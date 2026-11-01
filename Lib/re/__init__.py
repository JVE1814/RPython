"""Regular expression operations.

Patterns and the strings they are applied to may be str (Unicode text) or
bytes-like (8-bit data); the two kinds cannot be mixed.

Module functions:
    compile(pattern, flags=0)            compile a pattern to a Pattern object
    prefixmatch(pattern, string, flags)  match at the beginning of string
    match(pattern, string, flags)        alias of prefixmatch
    fullmatch(pattern, string, flags)    match the whole string
    search(pattern, string, flags)       find the first match anywhere
    sub(pattern, repl, string, count=0, flags=0)
                                         replace matches by repl
    subn(...)                            like sub, also returns the count
    split(pattern, string, maxsplit=0, flags=0)
                                         split string at matches
    findall(pattern, string, flags)      list of all matches (or groups)
    finditer(pattern, string, flags)     iterator of Match objects
    purge()                              clear the pattern cache
    escape(pattern)                      backslash-escape special characters

Flags (combine with "|"): A/ASCII, I/IGNORECASE, L/LOCALE (bytes only),
M/MULTILINE, S/DOTALL, X/VERBOSE, U/UNICODE (default for str patterns).

Invalid patterns raise PatternError (also available as 'error').
"""

import enum
from . import _compiler, _parser
import _sre


__all__ = [
    "prefixmatch", "match", "fullmatch", "search", "sub", "subn", "split",
    "findall", "finditer", "compile", "purge", "escape",
    "error", "Pattern", "Match", "A", "I", "L", "M", "S", "X", "U",
    "ASCII", "IGNORECASE", "LOCALE", "MULTILINE", "DOTALL", "VERBOSE",
    "UNICODE", "NOFLAG", "RegexFlag", "PatternError"
]

__version__ = "2.2.1"


@enum.global_enum
class RegexFlag(enum.IntFlag, boundary=enum.KEEP):
    NOFLAG = 0
    ASCII = A = _compiler.SRE_FLAG_ASCII            # ASCII-only \w, \d, ...
    IGNORECASE = I = _compiler.SRE_FLAG_IGNORECASE  # case insensitive
    LOCALE = L = _compiler.SRE_FLAG_LOCALE          # current 8-bit locale
    UNICODE = U = _compiler.SRE_FLAG_UNICODE        # Unicode "locale"
    MULTILINE = M = _compiler.SRE_FLAG_MULTILINE    # anchors at newlines
    DOTALL = S = _compiler.SRE_FLAG_DOTALL          # dot matches newline
    VERBOSE = X = _compiler.SRE_FLAG_VERBOSE        # whitespace/comments
    DEBUG = _compiler.SRE_FLAG_DEBUG                # dump the pattern
    __str__ = object.__str__
    _numeric_repr_ = hex


PatternError = error = _compiler.PatternError

_missing = object()   # marks "argument not given"


def _flags_value(flags):
    if isinstance(flags, RegexFlag):
        return flags.value
    return flags


# --------------------------------------------------------------------
# public interface

def prefixmatch(pattern, string, flags=0):
    """Match pattern at the start of string.

    Return a Match object, or None when the beginning of string does not
    match."""
    return _compile(pattern, flags).prefixmatch(string)


def match(pattern, string, flags=0):
    """Match pattern at the start of string.

    Return a Match object, or None when the beginning of string does not
    match."""
    return _compile(pattern, flags).match(string)


def fullmatch(pattern, string, flags=0):
    """Match pattern against the whole of string.

    Return a Match object, or None when string as a whole does not match."""
    return _compile(pattern, flags).fullmatch(string)


def search(pattern, string, flags=0):
    """Find the first position where pattern matches in string.

    Return a Match object, or None when there is no match anywhere."""
    return _compile(pattern, flags).search(string)


def _legacy_args(fname, argname, args, count, flags):
    # count/maxsplit and flags used to be accepted positionally.
    if args:
        if count is not _missing:
            raise TypeError("%s() got multiple values for argument %r"
                            % (fname, argname))
        count = args[0]
        if len(args) > 1:
            if flags is not _missing:
                raise TypeError("%s() got multiple values for argument 'flags'"
                                % fname)
            flags = args[1]
            if len(args) > 2:
                fixed = 2 if fname == 'split' else 3
                raise TypeError("%s() takes from %d to %d positional arguments "
                                "but %d were given"
                                % (fname, fixed, fixed + 2, fixed + len(args)))
        import warnings
        warnings.warn("%r is passed as positional argument" % argname,
                      DeprecationWarning, stacklevel=3)
    if count is _missing:
        count = 0
    if flags is _missing:
        flags = 0
    return count, flags


def sub(pattern, repl, string, *args, count=_missing, flags=_missing):
    """Replace non-overlapping matches of pattern in string by repl.

    repl is either a string, in which backslash escapes and group
    references are processed, or a callable that receives each Match
    object and returns the replacement string.  At most count
    replacements are made when count is positive."""
    count, flags = _legacy_args('sub', 'count', args, count, flags)
    return _compile(pattern, flags).sub(repl, string, count)

sub.__text_signature__ = '(pattern, repl, string, count=0, flags=0)'


def subn(pattern, repl, string, *args, count=_missing, flags=_missing):
    """Like sub(), but return a tuple (new_string, number_of_replacements)."""
    count, flags = _legacy_args('subn', 'count', args, count, flags)
    return _compile(pattern, flags).subn(repl, string, count)

subn.__text_signature__ = '(pattern, repl, string, count=0, flags=0)'


def split(pattern, string, *args, maxsplit=_missing, flags=_missing):
    """Split string at the matches of pattern and return the pieces as a list.

    The text of capturing groups is included in the result between the
    pieces.  When maxsplit is positive, at most maxsplit splits are done and
    the rest of the string is the last element."""
    maxsplit, flags = _legacy_args('split', 'maxsplit', args, maxsplit, flags)
    return _compile(pattern, flags).split(string, maxsplit)

split.__text_signature__ = '(pattern, string, maxsplit=0, flags=0)'


def findall(pattern, string, flags=0):
    """Return a list of all non-overlapping matches of pattern in string.

    Without groups the list holds the matched strings; with exactly one
    group it holds that group's text; with several groups it holds tuples.
    Empty matches are included."""
    return _compile(pattern, flags).findall(string)


def finditer(pattern, string, flags=0):
    """Return an iterator of Match objects for all non-overlapping matches
    of pattern in string (empty matches included)."""
    return _compile(pattern, flags).finditer(string)


def compile(pattern, flags=0):
    "Compile a regular expression pattern and return a Pattern object."
    return _compile(pattern, flags)


def purge():
    "Clear the cache of compiled patterns."
    _cache.clear()


# Characters that have a meaning somewhere in the pattern syntax: operators,
# brackets, set operators, and '#' and whitespace (verbose mode).
_escape_map = {c: '\\' + chr(c) for c in b'()[]{}?*+-|^$\\.&~# \t\n\r\v\f'}


def escape(pattern):
    """Backslash-escape every character of pattern that is special in a
    regular expression."""
    if isinstance(pattern, str):
        return pattern.translate(_escape_map)
    text = str(pattern, 'latin1')
    return text.translate(_escape_map).encode('latin1')


Pattern = type(_compiler.compile('', 0))
Match = type(_compiler.compile('', 0).match(''))

# --------------------------------------------------------------------
# internals

# LRU cache of compiled patterns, keyed by (type, pattern, flags); the most
# recently used entry is the last one of the dict.
_cache = {}
_MAXCACHE = 512


def _compile(pattern, flags):
    flags = _flags_value(flags)
    try:
        key = (type(pattern), pattern, flags)
        p = _cache.pop(key)
    except KeyError:
        pass
    except TypeError:
        key = None
    else:
        _cache[key] = p
        return p

    if isinstance(pattern, Pattern):
        if flags:
            raise ValueError(
                "cannot process flags argument with a compiled pattern")
        return pattern
    if not _compiler.isstring(pattern):
        raise TypeError("first argument must be string or compiled pattern")
    p = _compiler.compile(pattern, flags)
    if key is None or flags & DEBUG:
        return p
    if len(_cache) >= _MAXCACHE:
        try:
            del _cache[next(iter(_cache))]
        except (StopIteration, RuntimeError, KeyError):
            pass
    _cache[key] = p
    return p


# Patterns pickle as a call to _compile(pattern, flags).
import copyreg


def _pickle(p):
    return _compile, (p.pattern, p.flags)


copyreg.pickle(Pattern, _pickle, _compile)


# --------------------------------------------------------------------
# simple lexical scanner


class Scanner:
    """Tokenizer driven by a list of (regex, action) pairs.

    scan(string) matches the alternatives at the current position; for
    each match the action (if callable) is called as action(scanner, text)
    and a non-None result is collected.  Returns (results, unmatched_rest).
    """

    def __init__(self, lexicon, flags=0):
        flags = _flags_value(flags)
        self.lexicon = lexicon
        parts = []
        for phrase, action in lexicon:
            if _compiler.compile(phrase, flags).groups:
                raise ValueError("Cannot use capturing groups in re.Scanner")
            parts.append(phrase)
        if parts and isinstance(parts[0], bytes):
            combined = b'|'.join([b'(' + p + b')' for p in parts])
        else:
            combined = '|'.join(['(' + p + ')' for p in parts])
        self.scanner = _compiler.compile(combined, flags)

    def scan(self, string):
        result = []
        match = self.scanner.scanner(string).match
        pos = 0
        while True:
            m = match()
            if not m:
                break
            end = m.end()
            if end == pos:
                break
            action = self.lexicon[m.lastindex - 1][1]
            if callable(action):
                self.match = m
                action = action(self, m.group())
            if action is not None:
                result.append(action)
            pos = end
        return result, string[pos:]
