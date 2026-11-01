"""Binary-to-text encodings: Base16/32/64 (RFC 4648), Base85, Ascii85, Z85.

Encoders take bytes-like objects and return bytes; decoders also accept
ASCII-only str.  The codecs themselves live in binascii; this module layers
the RFC 4648 options (alternative alphabets, case folding, 0/1 mapping) and
the legacy MIME helpers (encode/decode/encodebytes/decodebytes) on top.
"""

import binascii

__all__ = [
    'encode', 'decode', 'encodebytes', 'decodebytes',
    'b64encode', 'b64decode', 'b32encode', 'b32decode',
    'b32hexencode', 'b32hexdecode', 'b16encode', 'b16decode',
    'b85encode', 'b85decode', 'a85encode', 'a85decode', 'z85encode', 'z85decode',
    'standard_b64encode', 'standard_b64decode',
    'urlsafe_b64encode', 'urlsafe_b64decode',
]

bytes_types = (bytes, bytearray)

_MISSING = object()
_STD64 = binascii.BASE64_ALPHABET


def _bytes_from_decode_data(s):
    """Coerce decoder input (ASCII str or bytes-like) to bytes."""
    if isinstance(s, str):
        if not s.isascii():
            raise ValueError('string argument should contain only ASCII characters')
        return s.encode('ascii')
    if isinstance(s, bytes_types):
        return s
    try:
        view = memoryview(s)
    except TypeError:
        raise TypeError("argument should be a bytes-like object or ASCII "
                        f"string, not {type(s).__name__!r}") from None
    return view.tobytes()


def _as_ignorechars(value):
    if value is None or isinstance(value, str):
        raise TypeError("a bytes-like object is required, not "
                        f"'{type(value).__name__}'")
    return memoryview(value).tobytes()


def _checked_wrapcol(wrapcol):
    from operator import index
    n = index(wrapcol)
    if n < 0:
        raise ValueError("wrapcol must be non-negative")
    if n >> 64:
        raise OverflowError("Python int too large to convert to C size_t")
    return n


def _split_lines(data, wrapcol, group):
    """Insert newlines so no line exceeds wrapcol, keeping whole groups."""
    if wrapcol == 0 or len(data) <= wrapcol:
        return data
    step = max(group, wrapcol - wrapcol % group)
    return b'\n'.join([data[i:i + step] for i in range(0, len(data), step)])


def _warn(message, category):
    import warnings
    warnings.warn(message, category, stacklevel=3)


# ---------------------------------------------------------------- Base64

def b64encode(s, altchars=None, *, padded=True, wrapcol=0):
    """Base64-encode s.

    altchars: two bytes replacing '+' and '/'.  padded=False drops the '='
    padding.  A non-zero wrapcol inserts newlines every wrapcol characters
    (rounded down to whole 4-character groups).
    """
    out = binascii.b2a_base64(s, wrapcol=wrapcol, newline=False, padded=padded)
    if altchars is None:
        return out
    if isinstance(altchars, str):
        raise TypeError("a bytes-like object is required, not 'str'")
    alt = memoryview(altchars).tobytes()
    if len(alt) != 2:
        raise ValueError(f'invalid altchars: {alt!r}')
    return out.translate(bytes.maketrans(b'+/', alt))


def b64decode(s, altchars=None, validate=_MISSING, *, padded=True,
              ignorechars=_MISSING):
    """Decode Base64 data s (bytes-like or ASCII str) and return bytes.

    altchars: the two characters used instead of '+' and '/'.
    validate: reject characters outside the alphabet instead of skipping them.
    padded=False: '=' padding is optional.
    ignorechars: bytes to skip; giving it (even empty) turns on strict
    checking of every other character unless validate=False is passed.
    Raises binascii.Error on malformed input.
    """
    s = _bytes_from_decode_data(s)
    if altchars is not None:
        altchars = _bytes_from_decode_data(altchars)
        if len(altchars) != 2:
            raise ValueError(f'invalid altchars: {altchars!r}')

    if ignorechars is not _MISSING:
        ignore = _as_ignorechars(ignorechars)
        strict = True if validate is _MISSING else bool(validate)
        alphabet = _STD64 if altchars is None else _STD64[:62] + altchars
        return binascii.a2b_base64(s, strict_mode=strict, padded=padded,
                                   alphabet=alphabet, ignorechars=ignore)

    strict = validate is not _MISSING and bool(validate)
    if altchars is not None and altchars != b'+/':
        # Standard '+' / '/' are still accepted for now (with a warning).
        for ch in b'+/':
            if ch not in altchars and ch in s:
                setting = f"altchars={altchars!r} and validate={strict}"
                if strict:
                    _warn(f"invalid character {chr(ch)!a} in Base64 data with "
                          f"{setting} will be an error in future Python versions",
                          DeprecationWarning)
                else:
                    _warn(f"invalid character {chr(ch)!a} in Base64 data with "
                          f"{setting} will be discarded in future Python versions",
                          FutureWarning)
        s = s.translate(bytes.maketrans(altchars, b'+/'))
    return binascii.a2b_base64(s, strict_mode=strict, padded=padded)


def standard_b64encode(s):
    """Base64-encode s with the standard alphabet."""
    return b64encode(s)


def standard_b64decode(s):
    """Decode standard-alphabet Base64 data (non-alphabet bytes are skipped)."""
    return b64decode(s)


_TO_URLSAFE = bytes.maketrans(b'+/', b'-_')
_FROM_URLSAFE = bytes.maketrans(b'-_', b'+/')


def urlsafe_b64encode(s, *, padded=True):
    """Base64-encode s using '-' and '_' in place of '+' and '/'."""
    return binascii.b2a_base64(s, newline=False, padded=padded).translate(_TO_URLSAFE)


def urlsafe_b64decode(s, *, padded=False):
    """Decode URL-safe Base64 data; '=' padding is optional unless padded=True."""
    s = _bytes_from_decode_data(s)
    for ch in b'+/':
        if ch in s:
            _warn(f"invalid character {chr(ch)!a} in URL-safe Base64 data "
                  "will be discarded in future Python versions", FutureWarning)
    return binascii.a2b_base64(s.translate(_FROM_URLSAFE), strict_mode=False,
                               padded=padded)


# ---------------------------------------------------------------- Base32

def b32encode(s, *, padded=True, wrapcol=0):
    """Base32-encode s (RFC 4648 alphabet); see b64encode for the options."""
    return binascii.b2a_base32(s, padded=padded, wrapcol=wrapcol)


def b32decode(s, casefold=False, map01=None, *, padded=True, ignorechars=b''):
    """Decode Base32 data and return bytes.

    casefold accepts lower-case input.  map01, if given, is the letter
    ('I' or 'L') that the digit 1 stands for; the digit 0 then means 'O'.
    Bytes in ignorechars are skipped; anything else outside the alphabet
    raises binascii.Error.
    """
    s = _bytes_from_decode_data(s)
    if map01 is not None:
        map01 = _bytes_from_decode_data(map01)
        if len(map01) != 1:
            raise ValueError(f'invalid map01: {map01!r}')
        s = s.translate(bytes.maketrans(b'01', b'O' + map01))
    if casefold:
        s = s.upper()
    return binascii.a2b_base32(s, padded=padded,
                               ignorechars=_as_ignorechars(ignorechars))


def b32hexencode(s, *, padded=True, wrapcol=0):
    """Base32-encode s with the "extended hex" alphabet (0-9A-V)."""
    return binascii.b2a_base32(s, padded=padded, wrapcol=wrapcol,
                               alphabet=binascii.BASE32HEX_ALPHABET)


def b32hexdecode(s, casefold=False, *, padded=True, ignorechars=b''):
    """Decode "extended hex" Base32 data; see b32decode."""
    s = _bytes_from_decode_data(s)
    if casefold:
        s = s.upper()
    return binascii.a2b_base32(s, padded=padded,
                               alphabet=binascii.BASE32HEX_ALPHABET,
                               ignorechars=_as_ignorechars(ignorechars))


# ---------------------------------------------------------------- Base16

def b16encode(s, *, wrapcol=0):
    """Hex-encode s with upper-case digits (optionally wrapped)."""
    wrapcol = _checked_wrapcol(wrapcol)
    return _split_lines(binascii.hexlify(s).upper(), wrapcol, 2)


def b16decode(s, casefold=False, *, ignorechars=b''):
    """Decode upper-case hex data (any case with casefold=True)."""
    s = _bytes_from_decode_data(s)
    if casefold:
        s = s.upper()
    ignore = _as_ignorechars(ignorechars)
    if ignore:
        s = s.translate(None, ignore)
    if s.translate(None, b'0123456789ABCDEF'):
        raise binascii.Error('Non-base16 digit found')
    return binascii.unhexlify(s)


# ---------------------------------------------------------------- Base85 family

def a85encode(b, *, foldspaces=False, wrapcol=0, pad=False, adobe=False):
    """Ascii85-encode b.

    foldspaces writes 'y' for four spaces (btoa extension); pad pads the
    input to a multiple of 4 bytes; adobe adds the <~ ~> framing; wrapcol
    limits the line length.
    """
    return binascii.b2a_ascii85(b, foldspaces=foldspaces, wrapcol=wrapcol,
                                pad=pad, adobe=adobe)


def a85decode(b, *, foldspaces=False, adobe=False, ignorechars=b' \t\n\r\v'):
    """Decode Ascii85 data; whitespace in ignorechars is skipped."""
    return binascii.a2b_ascii85(b, foldspaces=foldspaces, adobe=adobe,
                                ignorechars=_as_ignorechars(ignorechars))


def b85encode(b, pad=False, *, wrapcol=0):
    """Encode b with the Base85 alphabet used by git and Mercurial."""
    return binascii.b2a_base85(b, pad=pad, wrapcol=wrapcol)


def b85decode(b, *, ignorechars=b''):
    """Decode Base85 data (git/Mercurial alphabet)."""
    return binascii.a2b_base85(b, ignorechars=ignorechars)


def z85encode(s, pad=False, *, wrapcol=0):
    """Encode s with the ZeroMQ Z85 alphabet."""
    return binascii.b2a_base85(s, pad=pad, wrapcol=wrapcol,
                               alphabet=binascii.Z85_ALPHABET)


def z85decode(s, *, ignorechars=b''):
    """Decode ZeroMQ Z85 data."""
    return binascii.a2b_base85(s, alphabet=binascii.Z85_ALPHABET,
                               ignorechars=ignorechars)


# ---------------------------------------------------------------- legacy MIME API

MAXLINESIZE = 76
MAXBINSIZE = (MAXLINESIZE // 4) * 3


def encode(input, output):
    """Base64-encode binary file input into binary file output (MIME lines)."""
    while True:
        chunk = input.read(MAXBINSIZE)
        if not chunk:
            return
        while len(chunk) < MAXBINSIZE:
            more = input.read(MAXBINSIZE - len(chunk))
            if not more:
                break
            chunk += more
        output.write(binascii.b2a_base64(chunk))


def decode(input, output):
    """Base64-decode binary file input line by line into output."""
    while True:
        line = input.readline()
        if not line:
            return
        output.write(binascii.a2b_base64(line))


def _input_type_check(s):
    try:
        view = memoryview(s)
    except TypeError as exc:
        raise TypeError("expected bytes-like object, not "
                        f"{type(s).__name__}") from exc
    if view.format not in ('c', 'b', 'B'):
        raise TypeError(f"expected single byte elements, not {view.format!r} "
                        f"from {type(s).__name__}")
    if view.ndim != 1:
        raise TypeError(f"expected 1-D data, not {view.ndim}-D data "
                        f"from {type(s).__name__}")


def encodebytes(s):
    """Base64-encode s as MIME lines (76 characters, each ending in newline)."""
    _input_type_check(s)
    if not len(memoryview(s)):
        return b''
    return binascii.b2a_base64(s, wrapcol=MAXLINESIZE)


def decodebytes(s):
    """Decode Base64 data produced by encodebytes()."""
    _input_type_check(s)
    return binascii.a2b_base64(s)


def main():
    """Command line: encode (default) or decode a file or stdin to stdout."""
    import sys
    import getopt
    usage = (f"usage: {sys.argv[0]} [-h|-d|-e|-u] [file|-]\n"
             "        -h: print this help message and exit\n"
             "        -d, -u: decode\n"
             "        -e: encode (default)")
    try:
        opts, args = getopt.getopt(sys.argv[1:], 'hdeu')
    except getopt.error as exc:
        sys.stdout = sys.stderr
        print(exc)
        print(usage)
        sys.exit(2)
    action = encode
    for opt, _ in opts:
        if opt == '-h':
            print(usage)
            return
        action = encode if opt == '-e' else decode
    if args and args[0] != '-':
        with open(args[0], 'rb') as f:
            action(f, sys.stdout.buffer)
        return
    source = sys.stdin.buffer
    if sys.stdin.isatty():
        import io
        source = io.BytesIO(source.read())
    action(source, sys.stdout.buffer)


if __name__ == '__main__':
    main()
