"""The legacy Header class: header values made of chunks in different
character sets, RFC 2047 encoding and folding of such values.
"""

__all__ = [
    'Header',
    'decode_header',
    'make_header',
]

import re
import binascii

import email.quoprimime
import email.base64mime

from email.errors import HeaderParseError
from email import charset as _charset

Charset = _charset.Charset

NL = '\n'
SPACE = ' '
BSPACE = b' '
SPACE8 = ' ' * 8
EMPTYSTRING = ''
MAXLINELEN = 78
FWS = ' \t'

USASCII = Charset('us-ascii')
UTF8 = Charset('utf-8')

# An RFC 2047 encoded word.
ecre = re.compile(
    r'=\?(?P<charset>[^?]*?)\?(?P<encoding>[qQbB])\?(?P<encoded>.*?)\?=',
    re.MULTILINE)

# A field name at the start of a continuation line means the value would
# smuggle in a header of its own.
fcre = re.compile(r'[\041-\176]+:$')
_embedded_header = re.compile(r'\n[^ \t]+:')


def _decode_word(encoding, encoded, charset):
    if encoding == 'q':
        return email.quoprimime.header_decode(encoded).encode('raw-unicode-escape')
    padding = -len(encoded) % 4
    encoded += '=' * padding
    try:
        return email.base64mime.decode(encoded)
    except binascii.Error:
        raise HeaderParseError('Base64 decoding error')


def decode_header(header):
    """Split a header value into (decoded bytes, charset) pairs.

    Text outside encoded words is returned with charset None.  If the value
    contains no encoded word at all, a single (str, None) pair is returned.
    A Header instance is converted from its chunks.
    """
    if hasattr(header, '_chunks'):
        return [(_charset._encode(string, str(cs)), str(cs))
                for string, cs in header._chunks]
    if not ecre.search(header):
        return [(header, None)]

    # Collect (text, charset, encoding) words; charset None for plain text.
    words = []
    for line in header.splitlines():
        pos = 0
        for m in ecre.finditer(line):
            plain = line[pos:m.start()]
            if pos == 0:
                plain = plain.lstrip()
            if plain:
                words.append((plain, None, None))
            words.append((m.group('encoded'), m.group('charset').lower(),
                          m.group('encoding').lower()))
            pos = m.end()
        tail = line[pos:]
        if pos == 0:
            tail = tail.lstrip()
        if tail:
            words.append((tail, None, None))
    # Whitespace between two encoded words is not part of the value.
    kept = [w for i, w in enumerate(words)
            if not (w[1] is None and w[0].isspace()
                    and 0 < i < len(words) - 1
                    and words[i - 1][1] is not None
                    and words[i + 1][1] is not None)]

    merged = []
    for text, charset, encoding in kept:
        if charset is None:
            text = text.encode('raw-unicode-escape')
        else:
            text = _decode_word(encoding, text, charset)
        if merged and merged[-1][1] == charset:
            if charset is None:
                merged[-1][0] += BSPACE + text
            else:
                merged[-1][0] += text
        else:
            merged.append([text, charset])
    return [tuple(item) for item in merged]


def make_header(decoded_seq, maxlinelen=None, header_name=None,
                continuation_ws=' '):
    """Build a Header from (string, charset) pairs as returned by
    decode_header()."""
    h = Header(maxlinelen=maxlinelen, header_name=header_name,
               continuation_ws=continuation_ws)
    for s, charset in decoded_seq:
        if charset is not None and not isinstance(charset, Charset):
            charset = Charset(charset)
        h.append(s, charset)
    return h


def _is_separator_text(s):
    # Whitespace or comment delimiters need no extra space next to an
    # encoded word.
    return s.isspace() or s in ('(', ')', '\\')


def _plain_charset(cs):
    return cs is None or cs == 'us-ascii'


class Header:
    """A header value assembled from chunks in possibly different charsets."""

    def __init__(self, s=None, charset=None,
                 maxlinelen=None, header_name=None,
                 continuation_ws=' ', errors='strict'):
        if charset is None:
            charset = USASCII
        elif not isinstance(charset, Charset):
            charset = Charset(charset)
        self._charset = charset
        self._continuation_ws = continuation_ws
        self._chunks = []
        if s is not None:
            self.append(s, charset, errors)
        if maxlinelen is None:
            maxlinelen = MAXLINELEN
        self._maxlinelen = maxlinelen
        self._headerlen = 0 if header_name is None else len(header_name) + 2

    def __str__(self):
        """The value as text, with encoded chunks decoded."""
        self._normalize()
        out = []
        previous_cs = None
        previous_ends_with_space = None
        for string, cs in self._chunks:
            current_cs = cs
            if current_cs == _charset.UNKNOWN8BIT:
                string = string.encode('ascii', 'surrogateescape').decode(
                    'ascii', 'replace')
            if out:
                starts_with_space = string and _is_separator_text(string[0])
                if not _plain_charset(previous_cs):
                    if _plain_charset(current_cs) and not starts_with_space:
                        out.append(SPACE)
                        current_cs = None
                elif not _plain_charset(current_cs) and not previous_ends_with_space:
                    out.append(SPACE)
            previous_ends_with_space = string and _is_separator_text(string[-1])
            previous_cs = current_cs
            out.append(string)
        return EMPTYSTRING.join(out)

    def __eq__(self, other):
        return other == str(self)

    __hash__ = None

    def append(self, s, charset=None, errors='strict'):
        """Add *s* in *charset* (default: the header's charset).

        Bytes are decoded with the charset's input codec.  Text that cannot
        be represented in us-ascii is stored as utf-8 instead.
        """
        if charset is None:
            charset = self._charset
        elif not isinstance(charset, Charset):
            charset = Charset(charset)
        if not isinstance(s, str):
            codec = charset.input_codec or 'us-ascii'
            if codec == _charset.UNKNOWN8BIT:
                s = s.decode('us-ascii', 'surrogateescape')
            else:
                s = s.decode(codec, errors)
        out_codec = charset.output_codec or 'us-ascii'
        if out_codec != _charset.UNKNOWN8BIT:
            try:
                s.encode(out_codec, errors)
            except UnicodeEncodeError:
                if out_codec != 'us-ascii':
                    raise
                charset = UTF8
        self._chunks.append((s, charset))

    def encode(self, splitchars=';, \t', maxlinelen=None, linesep='\n'):
        """Return the folded, RFC 2047 encoded value.

        Lines are broken preferably after the characters in *splitchars*
        (earlier characters are preferred); *maxlinelen* overrides the
        length given to the constructor (0 means no limit).
        """
        self._normalize()
        if maxlinelen is None:
            maxlinelen = self._maxlinelen
        if maxlinelen == 0:
            maxlinelen = 1000000
        folder = _Folder(self._headerlen, maxlinelen,
                         self._continuation_ws, splitchars)
        previous_cs = None
        previous_ends_with_space = None
        first = True
        for string, cs in self._chunks:
            if not first:
                starts_with_space = string and _is_separator_text(string[0])
                if not _plain_charset(previous_cs):
                    if not starts_with_space or not _plain_charset(cs):
                        folder.add_transition()
                elif not _plain_charset(cs) and not previous_ends_with_space:
                    folder.add_transition()
            first = False
            previous_ends_with_space = string and _is_separator_text(string[-1])
            previous_cs = cs
            lines = string.splitlines()
            folder.feed('', lines[0] if lines else '', cs)
            for line in lines[1:]:
                folder.newline()
                stripped = line.lstrip()
                if cs.header_encoding is not None:
                    folder.feed(self._continuation_ws, ' ' + stripped, cs)
                else:
                    folder.feed(line[:len(line) - len(stripped)], stripped, cs)
            if len(lines) > 1:
                folder.newline()
        if self._chunks:
            folder.add_transition()
        value = folder.result(linesep)
        if _embedded_header.search(value):
            raise HeaderParseError("header value appears to contain "
                                   "an embedded header: {!r}".format(value))
        return value

    def _normalize(self):
        # Merge runs of chunks that share a charset.
        merged = []
        for string, cs in self._chunks:
            if merged and merged[-1][1] == cs:
                merged[-1][0].append(string)
            else:
                merged.append(([string], cs))
        self._chunks = [(SPACE.join(strings), cs) for strings, cs in merged]


class _Line:
    """The pieces of the output line being built: (whitespace, text) pairs
    plus the room taken up by a field name on the first line."""

    def __init__(self, initial=0):
        self.initial = initial
        self.pieces = []

    def __len__(self):
        return self.initial + sum(len(ws) + len(text) for ws, text in self.pieces)

    def __str__(self):
        return EMPTYSTRING.join(ws + text for ws, text in self.pieces)

    def only_whitespace(self):
        return self.initial == 0 and (not self.pieces or str(self).isspace())


class _Folder:
    """Accumulates header text and breaks it into lines."""

    def __init__(self, headerlen, maxlen, continuation_ws, splitchars):
        self.maxlen = maxlen
        self.continuation_ws = continuation_ws
        self.continuation_len = len(continuation_ws.replace('\t', SPACE8))
        self.splitchars = splitchars
        self.lines = []
        self.line = _Line(headerlen)

    def result(self, linesep):
        self.newline()
        return linesep.join(self.lines)

    def newline(self):
        pieces = self.line.pieces
        if pieces and pieces[-1] == (' ', ''):
            pieces.pop()
        if len(self.line) > 0:
            if self.line.only_whitespace() and self.lines:
                self.lines[-1] += str(self.line)
            else:
                self.lines.append(str(self.line))
        self.line = _Line()

    def add_transition(self):
        self.line.pieces.append((' ', ''))

    def feed(self, fws, string, cs):
        if cs.header_encoding is None:
            self._feed_plain(fws, string)
            return
        words = cs.header_encode_lines(string, self._room())
        if not words:
            return
        first = words.pop(0)
        if first is not None:
            self._add(fws, first)
        if not words:
            return
        last = words.pop()
        self.newline()
        self.line.pieces.append((self.continuation_ws, last))
        for word in words:
            self.lines.append(self.continuation_ws + word)

    def _room(self):
        yield self.maxlen - len(self.line)
        while True:
            yield self.maxlen - self.continuation_len

    def _feed_plain(self, fws, string):
        parts = re.split('([' + FWS + ']+)', fws + string)
        if parts[0]:
            parts.insert(0, '')
        else:
            del parts[0]
        for i in range(0, len(parts) - 1, 2):
            self._add(parts[i], parts[i + 1])

    def _break_index(self):
        # Find where to break the current line: the last place after one of
        # the split characters, trying the characters in order.
        pieces = self.line.pieces
        for ch in self.splitchars:
            for i in range(len(pieces) - 1, 0, -1):
                if ch.isspace():
                    ws = pieces[i][0]
                    if ws and ws[0] == ch:
                        return i
                before = pieces[i - 1][1]
                if before and before[-1] == ch:
                    return i
        return None

    def _add(self, ws, text):
        line = self.line
        line.pieces.append((ws, text))
        if len(line) <= self.maxlen:
            return
        index = self._break_index()
        if index is None:
            # Nothing to break at.  If a field name precedes this text, move
            # the text to a line of its own.
            ws, text = line.pieces.pop()
            if line.initial > 0:
                self.newline()
                if not ws:
                    ws = ' '
            self.line.pieces.append((ws, text))
            return
        rest = line.pieces[index:]
        del line.pieces[index:]
        self.lines.append(str(line))
        self.line = _Line()
        self.line.pieces = rest
