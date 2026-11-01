"""Base64 helpers for MIME bodies and RFC 2047 "B" encoded headers."""

__all__ = [
    'body_decode', 'body_encode', 'decode', 'decodestring',
    'header_encode', 'header_length',
]

from base64 import b64encode
from binascii import a2b_base64, b2a_base64

CRLF = '\r\n'
NL = '\n'
EMPTYSTRING = ''

# Size of the RFC 2047 chrome around an encoded word: =?charset?b??=
MISC_LEN = 7


def header_length(bytearray):
    """Length of the base64 encoding of *bytearray*."""
    full, rest = divmod(len(bytearray), 3)
    return full * 4 + (4 if rest else 0)


def header_encode(header_bytes, charset='iso-8859-1'):
    """Return an RFC 2047 B encoded word for *header_bytes* (a str is first
    encoded with *charset*)."""
    if not header_bytes:
        return ''
    if isinstance(header_bytes, str):
        header_bytes = header_bytes.encode(charset)
    encoded = b64encode(header_bytes).decode('ascii')
    return '=?%s?b?%s?=' % (charset, encoded)


def body_encode(s, maxlinelen=76, eol=NL):
    """Base64 encode the bytes *s* in lines of at most *maxlinelen*
    characters, each ended by *eol*."""
    if not s:
        return ""
    chunk = maxlinelen * 3 // 4
    lines = []
    for start in range(0, len(s), chunk):
        line = b2a_base64(s[start:start + chunk], newline=False).decode('ascii')
        lines.append(line + eol)
    return EMPTYSTRING.join(lines)


def decode(string):
    """Decode base64 text (str or bytes) to bytes."""
    if not string:
        return bytes()
    if isinstance(string, str):
        return a2b_base64(string.encode('raw-unicode-escape'))
    return a2b_base64(string)


body_decode = decode
decodestring = decode
