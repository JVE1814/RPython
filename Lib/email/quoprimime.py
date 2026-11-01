"""Quoted-printable (RFC 2045) helpers for headers and bodies.

Bodies use the classic quoted-printable rules (soft line breaks with '=',
trailing whitespace protected); headers use the RFC 2047 "Q" encoding where
a space becomes '_'.  Strings passed to the body functions are expected to
contain only code points < 256 (bytes decoded as latin-1).
"""

__all__ = [
    'body_decode', 'body_encode', 'body_length', 'decode', 'decodestring',
    'header_decode', 'header_encode', 'header_length', 'quote', 'unquote',
]

import re
from string import ascii_letters, digits, hexdigits

CRLF = '\r\n'
NL = '\n'
EMPTYSTRING = ''


def quote(c):
    """Return the '=XX' escape for the single character *c*."""
    return '=%02X' % ord(c)


def unquote(s):
    """Turn an '=XX' escape back into the character."""
    return chr(int(s[1:3], 16))


# Per-octet encodings.
_HEADER_SAFE = frozenset((ascii_letters + digits + '!*+-/').encode('ascii'))
_header_map = [quote(chr(c)) for c in range(256)]
for _c in _HEADER_SAFE:
    _header_map[_c] = chr(_c)
_header_map[ord(' ')] = '_'

_BODY_SAFE = frozenset(c for c in range(0x20, 0x7f) if c != ord('=')) | {ord('\t')}
_body_map = [quote(chr(c)) for c in range(256)]
for _c in _BODY_SAFE:
    _body_map[_c] = chr(_c)
del _c


def header_check(octet):
    """True if the octet needs to be encoded in a header."""
    return octet not in _HEADER_SAFE


def body_check(octet):
    """True if the octet needs to be encoded in a body."""
    return octet not in _BODY_SAFE


def header_length(bytearray):
    """Length of the Q encoding of *bytearray* (without the RFC 2047
    chrome)."""
    return sum(len(_header_map[octet]) for octet in bytearray)


def body_length(bytearray):
    """Length of the body encoding of *bytearray* (without soft breaks)."""
    return sum(len(_body_map[octet]) for octet in bytearray)


def header_encode(header_bytes, charset='iso-8859-1'):
    """Return an RFC 2047 Q encoded word for *header_bytes*; empty input
    gives an empty string."""
    if not header_bytes:
        return ''
    encoded = ''.join([_header_map[b] for b in header_bytes])
    return '=?%s?q?%s?=' % (charset, encoded)


def _encode_line(line, maxlinelen, out):
    # Append the encoded output lines for one input line to *out*.
    tokens = [_body_map[ord(ch)] for ch in line]
    trailing_ws = line[-1:] in (' ', '\t')
    count = len(tokens)
    current = ''
    for index, token in enumerate(tokens):
        last = index == count - 1
        if last and trailing_ws:
            token = quote(line[-1])
            if len(current) + len(token) > maxlinelen:
                if len(current) + 2 <= maxlinelen:
                    # the plain whitespace followed by a soft break fits
                    out.append(current + line[-1] + '=')
                    out.append('')
                    return
                out.append(current + '=')
                current = ''
        else:
            limit = maxlinelen if last else maxlinelen - 1
            if len(current) + len(token) > limit:
                out.append(current + '=')
                current = ''
        current += token
    out.append(current)


def body_encode(body, maxlinelen=76, eol=NL):
    """Encode a body string with quoted-printable.

    Lines are wrapped with soft line breaks so that no encoded line is
    longer than *maxlinelen*; line breaks in *body* (any convention) are
    written as *eol*.
    """
    if maxlinelen < 4:
        raise ValueError("maxlinelen must be at least 4")
    if not body:
        return body
    out = []
    for line in body.splitlines():
        if line:
            _encode_line(line, maxlinelen, out)
        else:
            out.append('')
    if body[-1] in '\r\n':
        out.append('')
    return eol.join(out)


_HEX = frozenset(hexdigits)


def decode(encoded, eol=NL):
    """Decode a quoted-printable body; line breaks become *eol*."""
    if not encoded:
        return encoded
    pieces = []
    for line in encoded.splitlines():
        line = line.rstrip()
        if not line:
            pieces.append(eol)
            continue
        pos = 0
        n = len(line)
        soft = False
        while pos < n:
            eq = line.find('=', pos)
            if eq < 0:
                pieces.append(line[pos:])
                break
            pieces.append(line[pos:eq])
            if eq + 1 == n:
                soft = True
                break
            pair = line[eq + 1:eq + 3]
            if len(pair) == 2 and pair[0] in _HEX and pair[1] in _HEX:
                pieces.append(unquote(line[eq:eq + 3]))
                pos = eq + 3
            else:
                pieces.append('=')
                pos = eq + 1
        if not soft:
            pieces.append(eol)
    decoded = ''.join(pieces)
    if encoded[-1] not in '\r\n' and decoded.endswith(eol):
        decoded = decoded[:-len(eol)]
    return decoded


body_decode = decode
decodestring = decode


_header_escape = re.compile(r'=[a-fA-F0-9]{2}', re.ASCII)


def header_decode(s):
    """Decode a Q encoded string ('_' is a space); returns a str whose
    characters are the decoded octets."""
    s = s.replace('_', ' ')
    return _header_escape.sub(lambda m: unquote(m.group(0)), s)
