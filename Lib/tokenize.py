"""Tokenization of Python source code.

tokenize(readline) takes a callable returning successive lines of *bytes*
(like io.BytesIO(...).readline), detects the source encoding and yields
TokenInfo 5-tuples (type, string, start, end, line); the first token is
always ENCODING.  generate_tokens(readline) does the same for a readline
returning *str* (no ENCODING token).  Positions are (row, column) with rows
starting at 1.  untokenize() converts tokens back into source text.

The tokens themselves come from the interpreter's tokenizer (_tokenize).
"""

from builtins import open as _builtin_open
from codecs import lookup, BOM_UTF8
import collections
import functools
from io import TextIOWrapper
import itertools as _itertools
import re
import sys
from token import *
from token import EXACT_TOKEN_TYPES
import token

cookie_re = re.compile(r'^[ \t\f]*#.*?coding[:=][ \t]*([-\w.]+)', re.ASCII)
blank_re = re.compile(br'^[ \t\f]*(?:[#\r\n]|$)', re.ASCII)

__all__ = token.__all__ + ["tokenize", "generate_tokens", "detect_encoding",
                           "untokenize", "TokenInfo", "open", "TokenError"]
del token


class TokenInfo(collections.namedtuple('TokenInfo', 'type string start end line')):
    def __repr__(self):
        annotated_type = '%d (%s)' % (self.type, tok_name[self.type])
        return ('TokenInfo(type=%s, string=%r, start=%r, end=%r, line=%r)' %
                self._replace(type=annotated_type))

    @property
    def exact_type(self):
        if self.type == OP and self.string in EXACT_TOKEN_TYPES:
            return EXACT_TOKEN_TYPES[self.string]
        return self.type


# ---------------------------------------------------------------------------
# Regular expressions describing Python tokens (kept for compatibility; the
# tokenizer itself does not use them).

def group(*choices): return '(' + '|'.join(choices) + ')'
def any(*choices): return group(*choices) + '*'
def maybe(*choices): return group(*choices) + '?'

Whitespace = r'[ \f\t]*'
Comment = r'#[^\r\n]*'
Ignore = Whitespace + any(r'\\\r?\n' + Whitespace) + maybe(Comment)
Name = r'\w+'

Hexnumber = r'0[xX](?:_?[0-9a-fA-F])+'
Binnumber = r'0[bB](?:_?[01])+'
Octnumber = r'0[oO](?:_?[0-7])+'
Decnumber = r'(?:0(?:_?0)*|[1-9](?:_?[0-9])*)'
Intnumber = group(Hexnumber, Binnumber, Octnumber, Decnumber)
Exponent = r'[eE][-+]?[0-9](?:_?[0-9])*'
Pointfloat = group(r'[0-9](?:_?[0-9])*\.(?:[0-9](?:_?[0-9])*)?',
                   r'\.[0-9](?:_?[0-9])*') + maybe(Exponent)
Expfloat = r'[0-9](?:_?[0-9])*' + Exponent
Floatnumber = group(Pointfloat, Expfloat)
Imagnumber = group(r'[0-9](?:_?[0-9])*[jJ]', Floatnumber + r'[jJ]')
Number = group(Imagnumber, Floatnumber, Intnumber)


def _all_string_prefixes():
    # Every case/order variant of the valid prefix letter combinations,
    # plus the empty prefix.
    combos = ['b', 'r', 'u', 'f', 't', 'br', 'fr', 'tr']
    result = {''}
    for combo in combos:
        for perm in _itertools.permutations(combo):
            for chars in _itertools.product(*[(c, c.upper()) for c in perm]):
                result.add(''.join(chars))
    return result


@functools.lru_cache
def _compile(expr):
    return re.compile(expr, re.UNICODE)


StringPrefix = group(*_all_string_prefixes())

# Tail end of ' string.
Single = r"[^'\\]*(?:\\.[^'\\]*)*'"
# Tail end of " string.
Double = r'[^"\\]*(?:\\.[^"\\]*)*"'
# Tail end of ''' string.
Single3 = r"[^'\\]*(?:(?:\\.|'(?!''))[^'\\]*)*'''"
# Tail end of """ string.
Double3 = r'[^"\\]*(?:(?:\\.|"(?!""))[^"\\]*)*"""'
Triple = group(StringPrefix + "'''", StringPrefix + '"""')
# Single-line ' or " string.
String = group(StringPrefix + r"'[^\n'\\]*(?:\\.[^\n'\\]*)*'",
               StringPrefix + r'"[^\n"\\]*(?:\\.[^\n"\\]*)*"')

# Operators, longest first so that the regex prefers them.
Special = group(*map(re.escape, sorted(EXACT_TOKEN_TYPES, reverse=True)))
Funny = group(r'\r?\n', Special)

PlainToken = group(Number, Funny, String, Name)
Token = Ignore + PlainToken

# First (or only) line of ' or " string.
ContStr = group(StringPrefix + r"'[^\n'\\]*(?:\\.[^\n'\\]*)*" +
                group("'", r'\\\r?\n'),
                StringPrefix + r'"[^\n"\\]*(?:\\.[^\n"\\]*)*' +
                group('"', r'\\\r?\n'))
PseudoExtras = group(r'\\\r?\n|\z', Comment, Triple)
PseudoToken = Whitespace + group(PseudoExtras, Number, Funny, ContStr, Name)

# Map each prefixed opening quote to the pattern matching the rest of the
# string.
endpats = {}
for _prefix in _all_string_prefixes():
    endpats[_prefix + "'"] = Single
    endpats[_prefix + '"'] = Double
    endpats[_prefix + "'''"] = Single3
    endpats[_prefix + '"""'] = Double3
del _prefix

single_quoted = set()
triple_quoted = set()
for t in _all_string_prefixes():
    for u in (t + '"', t + "'"):
        single_quoted.add(u)
    for u in (t + '"""', t + "'''"):
        triple_quoted.add(u)
del t, u

tabsize = 8


class TokenError(Exception): pass


class StopTokenizing(Exception): pass


# ---------------------------------------------------------------------------
# untokenize

class Untokenizer:

    def __init__(self):
        self.tokens = []
        self.prev_row = 1
        self.prev_col = 0
        self.prev_type = None
        self.prev_line = ""
        self.encoding = None

    def add_whitespace(self, start):
        row, col = start
        if row < self.prev_row or row == self.prev_row and col < self.prev_col:
            raise ValueError("start ({},{}) precedes previous end ({},{})"
                             .format(row, col, self.prev_row, self.prev_col))
        self.add_backslash_continuation(start)
        col_offset = col - self.prev_col
        if col_offset:
            self.tokens.append(" " * col_offset)

    def add_backslash_continuation(self, start):
        """Emit backslash-newlines for rows skipped without a NEWLINE/NL
        token, keeping the whitespace that preceded the backslash."""
        row = start[0]
        skipped = row - self.prev_row
        if skipped == 0:
            return
        newline = '\r\n' if self.prev_line.endswith('\r\n') else '\n'
        line = self.prev_line.rstrip('\\\r\n')
        trailing = len(line) - len(line.rstrip())
        ws = line[len(line) - trailing:] if trailing else ''
        self.tokens.append(ws + ("\\" + newline) * skipped)
        self.prev_col = 0

    def escape_brackets(self, token):
        """Double the braces of an f/t-string middle part, except the
        braces of \\N{...} escapes."""
        out = []
        in_named_escape = False
        for ch in token:
            if ch == "}":
                if in_named_escape:
                    in_named_escape = False
                else:
                    out.append(ch)
            elif ch == "{":
                # Count the backslashes before a preceding 'N'.
                backslashes = 0
                k = len(out) - 2
                while k >= 0 and out[k] == "\\":
                    backslashes += 1
                    k -= 1
                if out and out[-1] == "N" and backslashes % 2 == 1:
                    in_named_escape = True
                else:
                    out.append(ch)
            out.append(ch)
        return "".join(out)

    def untokenize(self, iterable):
        it = iter(iterable)
        indents = []
        startline = False
        for t in it:
            if len(t) == 2:
                self.compat(t, it)
                break
            tok_type, token, start, end, line = t
            if tok_type == ENCODING:
                self.encoding = token
                continue
            if tok_type == ENDMARKER:
                break
            if tok_type == INDENT:
                indents.append(token)
                continue
            elif tok_type == DEDENT:
                indents.pop()
                self.prev_row, self.prev_col = end
                continue
            elif tok_type in (NEWLINE, NL):
                startline = True
            elif startline and indents:
                indent = indents[-1]
                if start[1] >= len(indent):
                    self.tokens.append(indent)
                    self.prev_col = len(indent)
                startline = False
            elif tok_type in (FSTRING_MIDDLE, TSTRING_MIDDLE):
                if '{' in token or '}' in token:
                    token = self.escape_brackets(token)
                    last_line = token.splitlines()[-1]
                    end_line, end_col = end
                    extra = last_line.count("{{") + last_line.count("}}")
                    end = (end_line, end_col + extra)

            self.add_whitespace(start)
            self.tokens.append(token)
            self.prev_row, self.prev_col = end
            if tok_type in (NEWLINE, NL):
                self.prev_row += 1
                self.prev_col = 0
            self.prev_type = tok_type
            self.prev_line = line
        return "".join(self.tokens)

    def compat(self, token, iterable):
        """Untokenize 2-tuples (type, string): positions are unknown, so
        whitespace is synthesized."""
        indents = []
        append = self.tokens.append
        startline = token[0] in (NEWLINE, NL)
        prevstring = False
        nesting = 0

        for tok in _itertools.chain([token], iterable):
            toknum, tokval = tok[:2]
            if toknum == ENCODING:
                self.encoding = tokval
                continue

            if toknum in (NAME, NUMBER):
                tokval += ' '

            # Adjacent string literals need a separating space.
            if toknum == STRING:
                if prevstring:
                    tokval = ' ' + tokval
                prevstring = True
            else:
                prevstring = False

            if toknum in (FSTRING_START, TSTRING_START):
                nesting += 1
            elif toknum in (FSTRING_END, TSTRING_END):
                nesting -= 1
            if toknum == INDENT:
                indents.append(tokval)
                continue
            elif toknum == DEDENT:
                indents.pop()
                continue
            elif toknum in (NEWLINE, NL):
                startline = True
            elif startline and indents:
                append(indents[-1])
                startline = False
            elif toknum in (FSTRING_MIDDLE, TSTRING_MIDDLE):
                tokval = self.escape_brackets(tokval)

            # '{{' / '}}' inside an f-string would be read as an escape.
            if (tokval in ("{", "}") and nesting and self.tokens
                    and self.tokens[-1] == tokval):
                tokval = ' ' + tokval

            # Two f/t-strings in a row need a separating space.
            if (toknum in (FSTRING_START, TSTRING_START)
                    and self.prev_type in (FSTRING_END, TSTRING_END)):
                append(" ")

            append(tokval)
            self.prev_type = toknum


def untokenize(iterable):
    """Convert tokens back into Python source code.

    With full 5-tuples the result reproduces the original spacing (except
    for backslash continuations in some positions); with 2-tuples (type,
    string) the result tokenizes to the same tokens.  If the first token is
    ENCODING, the result is bytes in that encoding, otherwise str.
    """
    ut = Untokenizer()
    out = ut.untokenize(iterable)
    if ut.encoding is not None:
        out = out.encode(ut.encoding)
    return out


# ---------------------------------------------------------------------------
# Encoding detection

def _get_normal_name(orig_enc):
    """Normalize the spellings of utf-8 and latin-1 used by the tokenizer."""
    enc = orig_enc[:12].lower().replace("_", "-")
    if enc == "utf-8" or enc.startswith("utf-8-"):
        return "utf-8"
    if enc in ("latin-1", "iso-8859-1", "iso-latin-1") or \
       enc.startswith(("latin-1-", "iso-8859-1-", "iso-latin-1-")):
        return "iso-8859-1"
    return orig_enc


def detect_encoding(readline):
    """Detect the encoding of a Python source file read with readline.

    Reads at most two lines; returns (encoding, lines_read).  The encoding
    comes from a UTF-8 BOM ('utf-8-sig') or a PEP 263 coding cookie on one
    of the first two lines, else it is 'utf-8'.  SyntaxError is raised for
    an unknown or inconsistent declaration and for undecodable lines.
    """
    try:
        filename = readline.__self__.name
    except AttributeError:
        filename = None
    bom_found = False
    default = 'utf-8'

    def read_or_stop():
        try:
            return readline()
        except StopIteration:
            return b''

    def fail(msg):
        if filename is not None:
            msg = '{} for {!r}'.format(msg, filename)
        raise SyntaxError(msg)

    def check_nul(line):
        if b'\0' in line:
            raise SyntaxError("source code cannot contain null bytes")

    def check_decodes(lines, encoding):
        for line in lines:
            try:
                line.decode(encoding)
            except UnicodeDecodeError:
                fail("invalid or missing encoding declaration")

    def find_cookie(line):
        # The cookie itself is ASCII; the rest of the line may use the
        # declared encoding, so match on a Latin-1 view of the bytes.
        match = cookie_re.match(line.decode('latin-1'))
        if not match:
            return None
        encoding = _get_normal_name(match.group(1))
        try:
            lookup(encoding)
        except LookupError:
            if filename is None:
                msg = "unknown encoding: " + encoding
            else:
                msg = "unknown encoding for {!r}: {}".format(filename, encoding)
            raise SyntaxError(msg)
        if bom_found:
            if encoding != 'utf-8':
                fail('encoding problem: utf-8')
            encoding += '-sig'
        return encoding

    first = read_or_stop()
    if first.startswith(BOM_UTF8):
        bom_found = True
        first = first[3:]
        default = 'utf-8-sig'
    if not first:
        return default, []
    check_nul(first)

    encoding = find_cookie(first)
    if encoding:
        check_decodes([first], encoding)
        return encoding, [first]
    if not blank_re.match(first):
        check_decodes([first], default)
        return default, [first]

    second = read_or_stop()
    if not second:
        check_decodes([first], default)
        return default, [first]
    check_nul(second)

    encoding = find_cookie(second)
    if encoding:
        check_decodes([first, second], encoding)
        return encoding, [first, second]
    check_decodes([first, second], default)
    return default, [first, second]


def open(filename):
    """Open a Python source file in text mode, decoding it with the
    encoding found by detect_encoding()."""
    buffer = _builtin_open(filename, 'rb')
    try:
        encoding, lines = detect_encoding(buffer.readline)
        buffer.seek(0)
        text = TextIOWrapper(buffer, encoding, line_buffering=True)
        text.mode = 'r'
        return text
    except:
        buffer.close()
        raise


# ---------------------------------------------------------------------------
# Tokenizing

def tokenize(readline):
    """Tokenize source read as bytes with readline (see module docstring).

    The first token is ENCODING; the remaining lines are decoded with the
    detected encoding.
    """
    encoding, consumed = detect_encoding(readline)
    lines = _itertools.chain(consumed, iter(readline, b""))
    if encoding is not None:
        if encoding == "utf-8-sig":
            # The BOM was removed by detect_encoding().
            encoding = "utf-8"
        yield TokenInfo(ENCODING, encoding, (0, 0), (0, 0), '')
    yield from _generate_tokens_from_c_tokenizer(lines.__next__, encoding,
                                                 extra_tokens=True)


def generate_tokens(readline):
    """Tokenize source read as str with readline (no ENCODING token)."""
    return _generate_tokens_from_c_tokenizer(readline, extra_tokens=True)


def _transform_msg(msg):
    """Adapt tokenizer error messages to the historical tokenize ones."""
    if "unterminated triple-quoted string literal" in msg:
        return "EOF in multi-line string"
    return msg


def _generate_tokens_from_c_tokenizer(source, encoding=None, extra_tokens=False):
    """Tokenize with the interpreter's tokenizer (readline-style source)."""
    import _tokenize
    if encoding is None:
        it = _tokenize.TokenizerIter(source, extra_tokens=extra_tokens)
    else:
        it = _tokenize.TokenizerIter(source, encoding=encoding,
                                     extra_tokens=extra_tokens)
    try:
        for info in it:
            yield TokenInfo._make(info)
    except SyntaxError as e:
        if type(e) != SyntaxError:
            raise e from None
        msg = _transform_msg(e.msg)
        raise TokenError(msg, (e.lineno, e.offset)) from None


# ---------------------------------------------------------------------------
# Command line interface

def _main(args=None):
    import argparse

    def perror(message):
        sys.stderr.write(message)
        sys.stderr.write('\n')

    def error(message, filename=None, location=None):
        if location:
            args = (filename,) + location + (message,)
            perror("%s:%d:%d: error: %s" % args)
        elif filename:
            perror("%s: error: %s" % (filename, message))
        else:
            perror("error: %s" % message)
        sys.exit(1)

    parser = argparse.ArgumentParser(color=True)
    parser.add_argument(dest='filename', nargs='?',
                        metavar='filename.py',
                        help='the file to tokenize; defaults to stdin')
    parser.add_argument('-e', '--exact', dest='exact', action='store_true',
                        help='display token names using the exact type')
    args = parser.parse_args(args)

    try:
        if args.filename:
            filename = args.filename
            with _builtin_open(filename, 'rb') as f:
                tokens = list(tokenize(f.readline))
        else:
            filename = "<stdin>"
            tokens = _generate_tokens_from_c_tokenizer(
                sys.stdin.readline, extra_tokens=True)

        for token in tokens:
            token_type = token.type
            if args.exact:
                token_type = token.exact_type
            token_range = "%d,%d-%d,%d:" % (token.start + token.end)
            print("%-20s%-15s%-15r" %
                  (token_range, tok_name[token_type], token.string))
    except IndentationError as err:
        line, column = err.args[1][1:3]
        error(err.args[0], filename, (line, column))
    except TokenError as err:
        line, column = err.args[1]
        error(err.args[0], filename, (line, column))
    except SyntaxError as err:
        error(err, filename)
    except OSError as err:
        error(err)
    except KeyboardInterrupt:
        print("interrupted\n")
    except Exception as err:
        perror("unexpected error: %s" % err)
        raise


if __name__ == "__main__":
    _main()
