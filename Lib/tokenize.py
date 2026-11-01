"""Split Python source code into tokens.

``tokenize(readline)`` reads *bytes* lines, determines the source encoding
(BOM / PEP 263 declaration) and yields `TokenInfo` tuples
``(type, string, start, end, line)``, starting with an ENCODING token.
``generate_tokens(readline)`` does the same for *str* lines.  Rows count
from 1, columns from 0.  ``untokenize()`` turns tokens back into source.

The scanning itself is done by the interpreter's tokenizer (``_tokenize``).
"""

from builtins import open as _builtin_open
from codecs import lookup, BOM_UTF8
import collections
import itertools as _itertools
import sys
from io import TextIOWrapper
from token import *
from token import EXACT_TOKEN_TYPES
import token as _token

__all__ = _token.__all__ + ["tokenize", "generate_tokens", "detect_encoding",
                            "untokenize", "TokenInfo", "open", "TokenError"]


class TokenInfo(collections.namedtuple('TokenInfo', 'type string start end line')):
    """One token: its type, text, (row, col) start and end, and source line."""

    def __repr__(self):
        kind = f'{self.type} ({tok_name[self.type]})'
        return (f'TokenInfo(type={kind}, string={self.string!r}, '
                f'start={self.start!r}, end={self.end!r}, line={self.line!r})')

    @property
    def exact_type(self):
        """For OP tokens the specific operator type, else `type`."""
        if self.type == OP:
            return EXACT_TOKEN_TYPES.get(self.string, OP)
        return self.type


class TokenError(Exception):
    """Raised for input that ends inside a construct (e.g. a bracket)."""


class StopTokenizing(Exception):
    pass


# ---------------------------------------------------------------------------
# Regular expression fragments for Python tokens.  The tokenizer does not use
# them; they are provided for code that builds its own scanners.

def _alternatives(*items):
    return '(' + '|'.join(items) + ')'


def _string_prefixes():
    """All spellings (any order, any case) of the legal string prefixes,
    including the empty one."""
    found = {''}
    for letters in ('b', 'r', 'u', 'f', 't', 'br', 'fr', 'tr'):
        for order in _itertools.permutations(letters):
            for cased in _itertools.product(*((c.lower(), c.upper()) for c in order)):
                found.add(''.join(cased))
    return found


Whitespace = r'[ \f\t]*'
Comment = r'#[^\r\n]*'
Name = r'\w+'
_DIGITS = r'[0-9](?:_?[0-9])*'
Hexnumber = r'0[xX](?:_?[0-9a-fA-F])+'
Binnumber = r'0[bB](?:_?[01])+'
Octnumber = r'0[oO](?:_?[0-7])+'
Decnumber = r'(?:0(?:_?0)*|[1-9](?:_?[0-9])*)'
Intnumber = _alternatives(Hexnumber, Binnumber, Octnumber, Decnumber)
Exponent = r'[eE][-+]?' + _DIGITS
Pointfloat = (_alternatives(_DIGITS + r'\.(?:' + _DIGITS + r')?', r'\.' + _DIGITS)
              + '(?:' + Exponent + ')?')
Expfloat = _DIGITS + Exponent
Floatnumber = _alternatives(Pointfloat, Expfloat)
Imagnumber = _alternatives(_DIGITS + '[jJ]', Floatnumber + '[jJ]')
Number = _alternatives(Imagnumber, Floatnumber, Intnumber)

StringPrefix = _alternatives(*sorted(_string_prefixes()))
String = _alternatives(StringPrefix + r"'[^\n'\\]*(?:\\.[^\n'\\]*)*'",
                       StringPrefix + r'"[^\n"\\]*(?:\\.[^\n"\\]*)*"')
single_quoted = {p + q for p in _string_prefixes() for q in ("'", '"')}
triple_quoted = {p + q for p in _string_prefixes() for q in ("'''", '"""')}
tabsize = 8


# ---------------------------------------------------------------------------
# Untokenizing

_LINE_ENDS = (NEWLINE, NL)
_MIDDLES = (FSTRING_MIDDLE, TSTRING_MIDDLE)
_STARTS = (FSTRING_START, TSTRING_START)
_ENDS = (FSTRING_END, TSTRING_END)


def _double_braces(text):
    """Escape the braces of literal f/t-string text, leaving the braces of
    ``\\N{NAME}`` escapes alone."""
    out = []
    i = 0
    n = len(text)
    while i < n:
        ch = text[i]
        if ch == '\\':
            # a backslash run: an odd run followed by N{ starts a named escape
            j = i
            while j < n and text[j] == '\\':
                j += 1
            out.append(text[i:j])
            if (j - i) % 2 and text[j:j + 2] == 'N{':
                close = text.find('}', j)
                if close < 0:
                    close = n - 1
                out.append(text[j:close + 1])
                i = close + 1
            else:
                i = j
            continue
        if ch in '{}':
            out.append(ch)
        out.append(ch)
        i += 1
    return ''.join(out)


class Untokenizer:
    """Rebuilds source text from tokens (see `untokenize`)."""

    def __init__(self):
        self.tokens = []
        self.prev_row = 1
        self.prev_col = 0
        self.prev_type = None
        self.prev_line = ""
        self.encoding = None

    def add_whitespace(self, start):
        """Emit what is needed to move from the previous token's end to
        *start*: backslash continuations for skipped rows, then spaces."""
        row, col = start
        if (row, col) < (self.prev_row, self.prev_col):
            raise ValueError("start ({},{}) precedes previous end ({},{})"
                             .format(row, col, self.prev_row, self.prev_col))
        self.add_backslash_continuation(start)
        if col > self.prev_col:
            self.tokens.append(' ' * (col - self.prev_col))

    def add_backslash_continuation(self, start):
        """Rows skipped without a NEWLINE/NL token were joined with a
        backslash; re-create those continuations (keeping the whitespace
        that stood before the backslash on the previous line)."""
        skipped = start[0] - self.prev_row
        if skipped <= 0:
            return
        eol = '\r\n' if self.prev_line.endswith('\r\n') else '\n'
        body = self.prev_line.rstrip('\\\r\n')
        gap = body[len(body.rstrip()):]
        self.tokens.append(gap + ('\\' + eol) * skipped)
        self.prev_col = 0

    def escape_brackets(self, token):
        return _double_braces(token)

    def untokenize(self, iterable):
        tokens = iter(iterable)
        indents = []
        at_line_start = False
        for tok in tokens:
            if len(tok) == 2:
                self.compat(tok, tokens)
                break
            kind, text, start, end, line = tok
            if kind == ENCODING:
                self.encoding = text
                continue
            if kind == ENDMARKER:
                break
            if kind == INDENT:
                indents.append(text)
                continue
            if kind == DEDENT:
                indents.pop()
                self.prev_row, self.prev_col = end
                continue
            if kind in _LINE_ENDS:
                at_line_start = True
            elif at_line_start and indents:
                indent = indents[-1]
                if start[1] >= len(indent):
                    self.tokens.append(indent)
                    self.prev_col = len(indent)
                at_line_start = False
            elif kind in _MIDDLES and ('{' in text or '}' in text):
                text = self.escape_brackets(text)
                last = text.splitlines()[-1] if text else ''
                end = (end[0], end[1] + last.count('{{') + last.count('}}'))
            self.add_whitespace(start)
            self.tokens.append(text)
            self.prev_row, self.prev_col = end
            if kind in _LINE_ENDS:
                self.prev_row += 1
                self.prev_col = 0
            self.prev_type = kind
            self.prev_line = line
        return ''.join(self.tokens)

    def compat(self, token, iterable):
        """Untokenize (type, string) pairs: positions are unknown, so only
        the whitespace needed to separate tokens is produced."""
        out = self.tokens
        indents = []
        at_line_start = token[0] in _LINE_ENDS
        after_string = False
        depth = 0
        for tok in _itertools.chain([token], iterable):
            kind, text = tok[:2]
            if kind == ENCODING:
                self.encoding = text
                continue
            if kind in (NAME, NUMBER):
                text += ' '
            if kind == STRING:
                if after_string:
                    text = ' ' + text
                after_string = True
            else:
                after_string = False
            if kind in _STARTS:
                depth += 1
            elif kind in _ENDS:
                depth -= 1
            if kind == INDENT:
                indents.append(text)
                continue
            if kind == DEDENT:
                indents.pop()
                continue
            if kind in _LINE_ENDS:
                at_line_start = True
            elif at_line_start and indents:
                out.append(indents[-1])
                at_line_start = False
            elif kind in _MIDDLES:
                text = self.escape_brackets(text)
            # inside an f-string, `{{` / `}}` would read as escaped braces
            if depth and text in ('{', '}') and out and out[-1] == text:
                text = ' ' + text
            if kind in _STARTS and self.prev_type in _ENDS:
                out.append(' ')
            out.append(text)
            self.prev_type = kind


def untokenize(iterable):
    """Turn tokens back into source text.

    Full 5-tuples reproduce the original layout as far as possible; with
    (type, string) pairs the result re-tokenizes to the same tokens.  If
    the token stream starts with ENCODING the result is encoded bytes.
    """
    builder = Untokenizer()
    text = builder.untokenize(iterable)
    if builder.encoding is not None:
        return text.encode(builder.encoding)
    return text


# ---------------------------------------------------------------------------
# Source encoding

def _get_normal_name(orig_enc):
    """Canonical spelling for the utf-8 and latin-1 aliases."""
    name = orig_enc[:12].lower().replace('_', '-')
    for canonical, aliases in (('utf-8', ('utf-8',)),
                               ('iso-8859-1', ('latin-1', 'iso-8859-1', 'iso-latin-1'))):
        for alias in aliases:
            if name == alias or name.startswith(alias + '-'):
                return canonical
    return orig_enc


def _cookie(line):
    """Encoding name declared by a PEP 263 comment line, or None."""
    text = line.lstrip(' \t\f')
    if not text.startswith('#'):
        return None
    pos = text.find('coding')
    while pos >= 0:
        rest = text[pos + 6:]
        if rest[:1] in (':', '='):
            rest = rest[1:].lstrip(' \t')
            size = 0
            while size < len(rest) and (rest[size].isalnum() or rest[size] in '-_.')\
                    and rest[size].isascii():
                size += 1
            if size:
                return rest[:size]
        pos = text.find('coding', pos + 1)
    return None


def _is_blank_or_comment(line):
    text = line.lstrip(b' \t\f')
    return not text or text[:1] in (b'#', b'\r', b'\n')


def detect_encoding(readline):
    """Find the encoding of the source that *readline* returns as bytes.

    At most two lines are read.  Returns ``(encoding, lines_read)``; the
    encoding is 'utf-8-sig' after a UTF-8 BOM, the declared one (PEP 263)
    or 'utf-8'.  Invalid declarations and undecodable or NUL-containing
    lines raise SyntaxError.
    """
    try:
        filename = readline.__self__.name
    except AttributeError:
        filename = None

    def problem(message):
        if filename is not None:
            message = f'{message} for {filename!r}'
        raise SyntaxError(message)

    def next_line():
        try:
            line = readline()
        except StopIteration:
            return b''
        if b'\0' in line:
            raise SyntaxError("source code cannot contain null bytes")
        return line

    def declared(line, bom):
        name = _cookie(line.decode('latin-1'))
        if name is None:
            return None
        name = _get_normal_name(name)
        try:
            lookup(name)
        except LookupError:
            if filename is None:
                raise SyntaxError("unknown encoding: " + name)
            raise SyntaxError(f"unknown encoding for {filename!r}: {name}")
        if bom:
            if name != 'utf-8':
                problem('encoding problem: utf-8')
            name = 'utf-8-sig'
        return name

    def verified(encoding, lines):
        for line in lines:
            try:
                line.decode(encoding)
            except UnicodeDecodeError:
                problem("invalid or missing encoding declaration")
        return encoding, lines

    first = next_line()
    bom = first.startswith(BOM_UTF8)
    if bom:
        first = first[len(BOM_UTF8):]
    default = 'utf-8-sig' if bom else 'utf-8'
    if not first:
        return default, []
    found = declared(first, bom)
    if found:
        return verified(found, [first])
    if not _is_blank_or_comment(first):
        return verified(default, [first])
    second = next_line()
    if not second:
        return verified(default, [first])
    found = declared(second, bom)
    return verified(found or default, [first, second])


def open(filename):
    """Open a source file for reading as text in its detected encoding."""
    raw = _builtin_open(filename, 'rb')
    try:
        encoding, _ = detect_encoding(raw.readline)
        raw.seek(0)
        text = TextIOWrapper(raw, encoding, line_buffering=True)
        text.mode = 'r'
        return text
    except BaseException:
        raw.close()
        raise


# ---------------------------------------------------------------------------
# Tokenizing

def tokenize(readline):
    """Tokenize source that *readline* returns as bytes (see module doc)."""
    encoding, consumed = detect_encoding(readline)
    lines = _itertools.chain(consumed, iter(readline, b""))
    if encoding == 'utf-8-sig':
        encoding = 'utf-8'   # the BOM itself has already been dropped
    yield TokenInfo(ENCODING, encoding, (0, 0), (0, 0), '')
    yield from _generate_tokens_from_c_tokenizer(lines.__next__, encoding,
                                                 extra_tokens=True)


def generate_tokens(readline):
    """Tokenize source that *readline* returns as str (no ENCODING token)."""
    return _generate_tokens_from_c_tokenizer(readline, extra_tokens=True)


def _generate_tokens_from_c_tokenizer(source, encoding=None, extra_tokens=False):
    """Run the interpreter's tokenizer over a readline-style *source*."""
    import _tokenize
    kwargs = {'extra_tokens': extra_tokens}
    if encoding is not None:
        kwargs['encoding'] = encoding
    try:
        for item in _tokenize.TokenizerIter(source, **kwargs):
            yield TokenInfo._make(item)
    except SyntaxError as exc:
        if type(exc) is not SyntaxError:
            raise exc from None
        message = exc.msg
        if 'unterminated triple-quoted string literal' in message:
            message = 'EOF in multi-line string'
        raise TokenError(message, (exc.lineno, exc.offset)) from None


# ---------------------------------------------------------------------------
# Command line

def _main(args=None):
    import argparse

    def fail(message, filename=None, location=None):
        if location:
            text = '%s:%d:%d: error: %s' % ((filename,) + tuple(location) + (message,))
        elif filename:
            text = f'{filename}: error: {message}'
        else:
            text = f'error: {message}'
        sys.stderr.write(text + '\n')
        sys.exit(1)

    cli = argparse.ArgumentParser(prog='python -m tokenize')
    cli.add_argument(dest='filename', nargs='?', metavar='filename.py',
                     help='file to tokenize (default: standard input)')
    cli.add_argument('-e', '--exact', dest='exact', action='store_true',
                     help='show the exact type of operator tokens')
    opts = cli.parse_args(args)

    filename = opts.filename or '<stdin>'
    try:
        if opts.filename:
            with _builtin_open(opts.filename, 'rb') as f:
                tokens = list(tokenize(f.readline))
        else:
            tokens = _generate_tokens_from_c_tokenizer(sys.stdin.readline,
                                                       extra_tokens=True)
        for tok in tokens:
            kind = tok.exact_type if opts.exact else tok.type
            where = '%d,%d-%d,%d:' % (tok.start + tok.end)
            print('%-20s%-15s%-15r' % (where, tok_name[kind], tok.string))
    except IndentationError as exc:
        fail(exc.args[0], filename, exc.args[1][1:3])
    except TokenError as exc:
        fail(exc.args[0], filename, exc.args[1])
    except SyntaxError as exc:
        fail(exc, filename)
    except OSError as exc:
        fail(exc)
    except KeyboardInterrupt:
        print("interrupted\n")
    except Exception as exc:
        sys.stderr.write(f"unexpected error: {exc}\n")
        raise


if __name__ == '__main__':
    _main()
