"""Text measuring and syntax colouring helpers for the REPL.

Widths follow the usual terminal conventions: East Asian wide/fullwidth
characters take two cells, combining marks and most format characters take
none, everything else one.  ``gen_colors`` classifies the tokens of Python
source so the reader can paint them with the current theme.
"""

from __future__ import annotations

import builtins
import functools
import io
import keyword
import re
import token as T
import tokenize
import unicodedata
from collections import deque
from dataclasses import dataclass
from typing import Iterable, Iterator, NamedTuple

import _colorize

# CSI sequences (SGR colours and cursor controls).
ANSI_ESCAPE_SEQUENCE = re.compile(r"\x1b\[[ -@]*[A-~]")
# readline-style markers around invisible prompt parts.
ZERO_WIDTH_BRACKET = re.compile(r"\x01.*?\x02")
ZERO_WIDTH_TRANS = str.maketrans({"\x01": "", "\x02": ""})

IDENTIFIERS_AFTER = {"def", "class"}
KEYWORD_CONSTANTS = {"True", "False", "None"}
BUILTINS = {name for name in dir(builtins) if not name.startswith("_")}


def THEME(**kwargs):
    """The syntax section of the theme currently in effect."""
    return _colorize.get_theme(**kwargs).syntax


class Span(NamedTuple):
    """An inclusive range of buffer offsets."""
    start: int
    end: int

    @classmethod
    def from_re(cls, m, group):
        a, b = m.span(group)
        return cls(a, b - 1)

    @classmethod
    def from_token(cls, tok, line_starts):
        start = line_starts[tok.start[0] - 1] + tok.start[1]
        end = line_starts[tok.end[0] - 1] + tok.end[1] - 1
        return cls(start, end)


class ColorSpan(NamedTuple):
    span: Span
    tag: str


# --- widths -----------------------------------------------------------------

def _zero_width(c: str) -> bool:
    cat = unicodedata.category(c)
    if cat in ("Mn", "Me"):
        return True
    if cat == "Cf" and c != "­":
        return True
    # conjoining Hangul vowels/finals and the zero width space
    return "ᅠ" <= c <= "ᇿ" or c == "​"


@functools.lru_cache(maxsize=4096)
def str_width(c: str) -> int:
    """Terminal cells taken by the single character *c*."""
    if c < "\x80":
        return 1
    if _zero_width(c):
        return 0
    if unicodedata.east_asian_width(c) in ("W", "F"):
        return 2
    return 1


def wlen(s: str) -> int:
    """Terminal cells taken by *s*, ignoring ANSI escape sequences.

    ^Z is displayed as two cells (it is shown in caret notation)."""
    if len(s) == 1:
        return 2 if s == "\x1a" else str_width(s)
    plain = ANSI_ESCAPE_SEQUENCE.sub("", s)
    return sum(str_width(c) for c in plain) + plain.count("\x1a")


def unbracket(s: str, including_content: bool = False) -> str:
    """Drop the \\x01/\\x02 markers readline uses around invisible text
    (and, with *including_content*, the text between them as well)."""
    if including_content:
        return ZERO_WIDTH_BRACKET.sub("", s)
    return s.translate(ZERO_WIDTH_TRANS)


def disp_char(c: str) -> tuple[str, int]:
    """How the reader shows buffer character *c*: (text, width)."""
    if c < " " or c == "\x7f":
        return "^" + chr(ord(c) ^ 0x40), 2
    if c < "\x80":
        return c, 1
    if unicodedata.category(c).startswith("C"):
        shown = f"\\u{ord(c):04x}" if ord(c) <= 0xFFFF else f"\\U{ord(c):08x}"
        return shown, len(shown)
    return c, str_width(c)


def disp_str(buffer: str, colors: list[ColorSpan] | None = None,
             start_index: int = 0, theme=None) -> tuple[list[str], list[int]]:
    """Displayed pieces and widths for each character of *buffer*.

    With *colors*, each piece is wrapped in the SGR sequence of its tag."""
    chars: list[str] = []
    widths: list[int] = []
    styles = style_tags(buffer, colors, start_index) if colors else None
    if styles is not None and theme is None:
        theme = THEME()
    for i, c in enumerate(buffer):
        text, w = disp_char(c)
        if styles is not None and styles[i] is not None:
            text = f"{theme[styles[i]]}{text}{theme.reset}"
        chars.append(text)
        widths.append(w)
    return chars, widths


def style_tags(buffer: str, colors: list[ColorSpan] | None,
               start_index: int = 0) -> list[str | None]:
    """The colour tag of each character of *buffer* (which starts at
    offset *start_index* of the text the spans refer to)."""
    tags: list[str | None] = [None] * len(buffer)
    if not colors:
        return tags
    end_index = start_index + len(buffer)
    for span, tag in colors:
        lo = max(span.start, start_index)
        hi = min(span.end + 1, end_index)
        for i in range(lo, hi):
            tags[i - start_index] = tag
    return tags


# --- iteration helper ---------------------------------------------------------

def prev_next_window(iterable: Iterable) -> Iterator[tuple]:
    """Yield (previous, current, next) for every item; the missing
    neighbours at both ends are None.  Exceptions from the underlying
    iterator propagate once the items before them have been produced."""
    it = iter(iterable)
    before = None
    try:
        current = next(it)
    except StopIteration:
        return
    while True:
        try:
            after = next(it)
        except StopIteration:
            yield (before, current, None)
            return
        except BaseException:
            yield (before, current, None)
            raise
        yield (before, current, after)
        before, current = current, after


# --- syntax colouring ---------------------------------------------------------

_STRING_TOKENS = {T.STRING, T.FSTRING_START, T.FSTRING_MIDDLE, T.FSTRING_END}
for _name in ("TSTRING_START", "TSTRING_MIDDLE", "TSTRING_END"):
    if hasattr(T, _name):
        _STRING_TOKENS.add(getattr(T, _name))
_MIDDLE_TOKENS = {T.FSTRING_MIDDLE}
if hasattr(T, "TSTRING_MIDDLE"):
    _MIDDLE_TOKENS.add(T.TSTRING_MIDDLE)
_LAYOUT_TOKENS = {T.NL, T.COMMENT, T.INDENT, T.DEDENT, T.ENDMARKER}
_STATEMENT_ENDERS = {T.NEWLINE, T.INDENT, T.DEDENT}

# tokens that may follow `match`/`case` when they start a pattern/subject
_SUBJECT_OPS = {"(", "[", "{", "-", "+", "~", "*"}


def _line_starts(text: str) -> list[int]:
    starts = [0]
    for i, c in enumerate(text):
        if c == "\n":
            starts.append(i + 1)
    return starts


def _middle_span(tok, text: str, line_starts: list[int]) -> Span:
    """Span of an f-string middle: doubled braces appear once in the
    token's string, so walk the source to find the real extent."""
    pos = line_starts[tok.start[0] - 1] + tok.start[1]
    start = pos
    for c in tok.string:
        if c in "{}" and text[pos:pos + 2] == c * 2:
            pos += 2
        else:
            pos += 1
    return Span(start, pos - 1)


def _significant(tokens):
    return [t for t in tokens if t.type not in _LAYOUT_TOKENS]


def gen_colors(buffer: str) -> Iterator[ColorSpan]:
    """Yield ColorSpans for the tokens of *buffer*, in order."""
    line_starts = _line_starts(buffer)
    tokens = []
    error = None
    try:
        for tok in tokenize.generate_tokens(io.StringIO(buffer).readline):
            tokens.append(tok)
    except (tokenize.TokenError, SyntaxError) as exc:
        error = exc
    yield from _classify(tokens, buffer, line_starts)
    if error is not None:
        span = _unterminated_string_span(error, tokens, buffer, line_starts)
        if span is not None:
            yield ColorSpan(span, "string")


def _unterminated_string_span(error, tokens, text, line_starts):
    msg = str(error.args[0]) if error.args else ""
    if not ("unterminated" in msg or "EOF in multi-line string" in msg):
        return None
    if isinstance(error, SyntaxError):
        lineno, col = error.lineno or 1, (error.offset or 1)
    else:
        try:
            lineno, col = error.args[1]
        except (IndexError, TypeError, ValueError):
            return None
    if lineno - 1 >= len(line_starts):
        return None
    start = line_starts[lineno - 1] + max(col - 1, 0)
    if tokens:
        last = tokens[-1]
        if last.end[0] - 1 < len(line_starts):
            last_end = line_starts[last.end[0] - 1] + last.end[1]
            start = max(start, last_end)
    multi_line = "triple-quoted" in msg or "multi-line" in msg
    if multi_line:
        end = len(text)
    else:
        nl = text.find("\n", start)
        end = len(text) if nl < 0 else nl
    if end <= start:
        return None
    return Span(start, end - 1)


def _classify(tokens, text, line_starts) -> Iterator[ColorSpan]:
    sig = _significant(tokens)
    depth = 0
    prev = None           # previous significant token
    prev_tag = None       # tag given to it (for `case _`)
    at_statement_start = True
    for i, tok in enumerate(sig):
        nxt = sig[i + 1] if i + 1 < len(sig) else None
        tag = None
        kind = tok.type
        if kind in _STRING_TOKENS:
            tag = "string"
        elif kind == T.NUMBER:
            tag = "number"
        elif kind == T.OP:
            tag = "op"
        elif kind == T.NAME:
            tag = _name_tag(tok.string, prev, prev_tag, nxt,
                            at_statement_start and depth == 0)
        if tag is not None:
            if kind in _MIDDLE_TOKENS:
                span = _middle_span(tok, text, line_starts)
            else:
                span = Span.from_token(tok, line_starts)
            if span.end >= span.start:
                yield ColorSpan(span, tag)
        # bookkeeping for the next token
        if kind == T.OP:
            if tok.string in "([{":
                depth += 1
            elif tok.string in ")]}":
                depth = max(depth - 1, 0)
        if kind in _STATEMENT_ENDERS:
            at_statement_start = True
            depth = 0
        elif kind == T.OP and tok.string in (";", ":") and depth == 0:
            at_statement_start = True
        else:
            at_statement_start = False
        prev, prev_tag = tok, tag
    # comments are not part of the significant stream
    for tok in tokens:
        if tok.type == T.COMMENT:
            yield ColorSpan(Span.from_token(tok, line_starts), "comment")


def _name_tag(name, prev, prev_tag, nxt, statement_start):
    after_dot = prev is not None and prev.type == T.OP and prev.string == "."
    if after_dot:
        return None
    if keyword.iskeyword(name):
        return "keyword"
    if prev is not None and prev.type == T.NAME and prev.string in IDENTIFIERS_AFTER:
        return "definition"
    if keyword.issoftkeyword(name):
        soft = _soft_keyword(name, prev, prev_tag, nxt, statement_start)
        if soft:
            return "soft_keyword"
    if name in BUILTINS:
        return "builtin"
    return None


def _starts_operand(tok) -> bool:
    if tok is None:
        return False
    if tok.type in (T.NAME, T.NUMBER, T.STRING, T.FSTRING_START):
        return True
    if getattr(T, "TSTRING_START", None) == tok.type:
        return True
    return tok.type == T.OP and tok.string in _SUBJECT_OPS


def _soft_keyword(name, prev, prev_tag, nxt, statement_start) -> bool:
    if name == "_":
        return (prev is not None and prev_tag == "soft_keyword"
                and prev.string == "case")
    if not statement_start or nxt is None:
        return False
    if name in ("match", "case"):
        if nxt.type == T.NAME and keyword.iskeyword(nxt.string) and nxt.string not in KEYWORD_CONSTANTS and nxt.string != "not":
            return False
        return _starts_operand(nxt)
    if name == "type":
        return nxt.type == T.NAME
    if name == "lazy":
        return nxt.type == T.NAME and nxt.string in ("import", "from")
    return False
