"""RFC 2047 encoded words ("=?charset?q?text?=") for header values.

decode(ew) -> (text, charset, lang, defects)
encode(text, charset='utf-8', encoding=None, lang='') -> encoded word

The q and b transfer encodings are available separately as
decode_q/encode_q/len_q and decode_b/encode_b/len_b; they work on bytes.
"""

import base64
import binascii
from email import errors

__all__ = ['decode_q', 'encode_q', 'decode_b', 'encode_b', 'len_q', 'len_b',
           'decode', 'encode']


# -- q encoding ----------------------------------------------------------------

_HEXDIGITS = b'0123456789abcdefABCDEF'


def decode_q(encoded):
    """Decode the q encoding (underscore means space).  Never reports
    defects; malformed escapes are kept literally."""
    data = encoded.replace(b'_', b' ')
    if b'=' not in data:
        return data, []
    out = bytearray()
    pos = 0
    n = len(data)
    while pos < n:
        eq = data.find(b'=', pos)
        if eq < 0:
            out += data[pos:]
            break
        out += data[pos:eq]
        pair = data[eq + 1:eq + 3]
        if len(pair) == 2 and pair[0] in _HEXDIGITS and pair[1] in _HEXDIGITS:
            out.append(int(pair, 16))
            pos = eq + 3
        else:
            out.append(0x3d)
            pos = eq + 1
    return bytes(out), []


# Bytes that may appear unencoded in a q-encoded word inside a header.
_Q_LITERAL = frozenset(
    b'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-!*+/')
_Q_TABLE = ['=%02X' % b for b in range(256)]
for _b in _Q_LITERAL:
    _Q_TABLE[_b] = chr(_b)
_Q_TABLE[0x20] = '_'
del _b


def encode_q(bstring):
    return ''.join([_Q_TABLE[b] for b in bstring])


def len_q(bstring):
    return sum(len(_Q_TABLE[b]) for b in bstring)


# -- b encoding ----------------------------------------------------------------

_B64_ALPHABET = frozenset(
    b'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/')


def decode_b(encoded):
    """Decode base64, tolerating junk characters and missing padding (each
    reported as a defect)."""
    defects = []
    kept = bytes(b for b in encoded if b in _B64_ALPHABET or b == 0x3d)
    if len(kept) != len(encoded):
        defects.append(errors.InvalidBase64CharactersDefect())
    stripped = kept.rstrip(b'=')
    present = len(kept) - len(stripped)
    data = stripped.replace(b'=', b'')
    remainder = len(data) % 4
    if remainder == 1:
        # cannot be decoded at all; hand back the original
        return encoded, [errors.InvalidBase64LengthDefect()]
    needed = (4 - remainder) % 4
    if present < needed:
        defects.append(errors.InvalidBase64PaddingDefect())
    try:
        return binascii.a2b_base64(data + b'=' * needed), defects
    except binascii.Error:
        return encoded, defects + [errors.InvalidBase64LengthDefect()]


def encode_b(bstring):
    return base64.b64encode(bstring).decode('ascii')


def len_b(bstring):
    full, rest = divmod(len(bstring), 3)
    return full * 4 + (4 if rest else 0)


_DECODERS = {'q': decode_q, 'b': decode_b}
_ENCODERS = {'q': encode_q, 'b': encode_b}
_LENGTHS = {'q': len_q, 'b': len_b}


def decode(ew):
    """Decode one encoded word.

    Returns (text, charset, lang, defects).  Bytes that cannot be decoded
    with the charset are kept as surrogate escapes.  A malformed word raises
    ValueError; an unknown transfer encoding raises KeyError.
    """
    _, charset, cte, cte_string, _ = ew.split('?')
    charset, _, lang = charset.partition('*')
    raw = cte_string.encode('ascii', 'surrogateescape')
    raw, defects = _DECODERS[cte.lower()](raw)
    try:
        text = raw.decode(charset)
    except UnicodeDecodeError:
        defects.append(errors.UndecodableBytesDefect(
            "Encoded word contains bytes not decodable using "
            "{!r} charset".format(charset)))
        text = raw.decode(charset, 'surrogateescape')
    except (LookupError, UnicodeEncodeError):
        text = raw.decode('ascii', 'surrogateescape')
        if charset.lower() != 'unknown-8bit':
            defects.append(errors.CharsetError(
                "Unknown charset {!r} in encoded word; decoded as "
                "unknown bytes".format(charset)))
    return text, charset, lang, defects


def encode(string, charset='utf-8', encoding=None, lang=''):
    """Return *string* as an RFC 2047 encoded word.

    *encoding* is 'q' or 'b'; if None, the shorter is chosen (q is used
    unless it is at least 5 characters longer).  For the 'unknown-8bit'
    charset, surrogate escapes in *string* become the original bytes.
    """
    if charset == 'unknown-8bit':
        raw = string.encode('ascii', 'surrogateescape')
    else:
        raw = string.encode(charset)
    if encoding is None:
        encoding = 'q' if len_q(raw) - len_b(raw) < 5 else 'b'
    text = _ENCODERS[encoding](raw)
    if lang:
        lang = '*' + lang
    return "=?{}{}?{}?{}?=".format(charset, lang, encoding, text)
