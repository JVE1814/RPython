"""Base16, Base32, Base64 (RFC 4648), Base85 and Ascii85 data encodings.

The heavy lifting is done by the binascii module; this module adds the
RFC 4648 conveniences (alternative alphabets, case folding, the 0/1 mapping
of Base32) and the legacy file-oriented helpers.
"""

import binascii

__all__ = [
    # Legacy interface exports traditional RFC 2045 Base64 encodings
    'encode', 'decode', 'encodebytes', 'decodebytes',
    # Generalized interface for other encodings
    'b64encode', 'b64decode', 'b32encode', 'b32decode',
    'b32hexencode', 'b32hexdecode', 'b16encode', 'b16decode',
    # Base85 and Ascii85 encodings
    'b85encode', 'b85decode', 'a85encode', 'a85decode', 'z85encode', 'z85decode',
    # Standard Base64 encoding
    'standard_b64encode', 'standard_b64decode',
    # URL- and filesystem-safe variants
    'urlsafe_b64encode', 'urlsafe_b64decode',
]

bytes_types = (bytes, bytearray)  # Types acceptable as binary data

_NOT_GIVEN = object()

_B64_STD = binascii.BASE64_ALPHABET


def _bytes_from_decode_data(s):
    if isinstance(s, str):
        try:
            return s.encode('ascii')
        except UnicodeEncodeError:
            raise ValueError('string argument should contain only ASCII characters')
    if isinstance(s, bytes_types):
        return s
    try:
        return memoryview(s).tobytes()
    except TypeError:
        raise TypeError("argument should be a bytes-like object or ASCII "
                        "string, not %r" % s.__class__.__name__) from None


def _ignorechars_arg(ignorechars):
    if isinstance(ignorechars, str) or ignorechars is None:
        raise TypeError("a bytes-like object is required, not '%s'"
                        % type(ignorechars).__name__)
    return memoryview(ignorechars).tobytes()


def _wrapcol_arg(wrapcol):
    import operator
    wrapcol = operator.index(wrapcol)
    if wrapcol < 0:
        raise ValueError("wrapcol must be non-negative")
    if wrapcol >= 1 << 64:
        raise OverflowError("Python int too large to convert to C size_t")
    return wrapcol


def _wrap(data, wrapcol, unit):
    """Split *data* into lines of at most *wrapcol* characters (a multiple
    of *unit*, at least *unit*)."""
    if not wrapcol or len(data) <= wrapcol:
        return data
    width = max(wrapcol // unit * unit, unit)
    return b'\n'.join(data[i:i + width] for i in range(0, len(data), width))


# Base64 encoding/decoding uses binascii

def b64encode(s, altchars=None, *, padded=True, wrapcol=0):
    """Encode the bytes-like object s using Base64 and return a bytes object.

    Optional altchars should be a byte string of length 2 which specifies an
    alternative alphabet for the '+' and '/' characters.  This allows an
    application to e.g. generate url or filesystem safe Base64 strings.

    If padded is false, omit padding in the output.

    If wrapcol is non-zero, insert a newline (b'\\n') character after at most
    every wrapcol characters.
    """
    encoded = binascii.b2a_base64(s, wrapcol=wrapcol, newline=False,
                                  padded=padded)
    if altchars is not None:
        if isinstance(altchars, str):
            raise TypeError("a bytes-like object is required, not 'str'")
        altchars = memoryview(altchars).tobytes()
        if len(altchars) != 2:
            raise ValueError(f'invalid altchars: {altchars!r}')
        return encoded.translate(bytes.maketrans(b'+/', altchars))
    return encoded


def b64decode(s, altchars=None, validate=_NOT_GIVEN, *, padded=True,
              ignorechars=_NOT_GIVEN):
    """Decode the Base64 encoded bytes-like object or ASCII string s.

    Optional altchars must be a bytes-like object or ASCII string of length 2
    which specifies the alternative alphabet used instead of the '+' and '/'
    characters.

    If padded is false, padding in input is not required.

    The result is returned as a bytes object.  A binascii.Error is raised if
    s is incorrectly padded.

    If ignorechars is specified, it should be a bytes-like object containing
    characters to ignore from the input.  The default is to discard
    characters that are neither in the alphabet nor the padding character
    unless validate is true; specifying ignorechars (even empty) enables
    strict validation of all other characters.
    """
    s = _bytes_from_decode_data(s)
    if altchars is not None:
        altchars = _bytes_from_decode_data(altchars)
        if len(altchars) != 2:
            raise ValueError(f'invalid altchars: {altchars!r}')
    if ignorechars is not _NOT_GIVEN:
        ignorechars = _ignorechars_arg(ignorechars)
        strict = True if validate is _NOT_GIVEN else bool(validate)
        alphabet = _B64_STD
        if altchars is not None:
            alphabet = _B64_STD[:-2] + altchars
        return binascii.a2b_base64(s, strict_mode=strict, padded=padded,
                                   alphabet=alphabet, ignorechars=ignorechars)

    validate = False if validate is _NOT_GIVEN else bool(validate)
    if altchars is not None and altchars != b'+/':
        for c in b'+/':
            if c in altchars or c not in s:
                continue
            if validate:
                import warnings
                warnings.warn(
                    f"invalid character {chr(c)!a} in Base64 data with "
                    f"altchars={altchars!r} and validate=True "
                    f"will be an error in future Python versions",
                    DeprecationWarning, stacklevel=2)
            else:
                import warnings
                warnings.warn(
                    f"invalid character {chr(c)!a} in Base64 data with "
                    f"altchars={altchars!r} and validate=False "
                    f"will be discarded in future Python versions",
                    FutureWarning, stacklevel=2)
        s = s.translate(bytes.maketrans(altchars, b'+/'))
    return binascii.a2b_base64(s, strict_mode=validate, padded=padded)


def standard_b64encode(s):
    """Encode bytes-like object s using the standard Base64 alphabet.

    The result is returned as a bytes object.
    """
    return b64encode(s)


def standard_b64decode(s):
    """Decode bytes encoded with the standard Base64 alphabet.

    Argument s is a bytes-like object or ASCII string to decode.  The result
    is returned as a bytes object.  A binascii.Error is raised if the input
    is incorrectly padded.  Characters that are not in the standard alphabet
    are discarded prior to the padding check.
    """
    return b64decode(s)


_urlsafe_encode_translation = bytes.maketrans(b'+/', b'-_')
_urlsafe_decode_translation = bytes.maketrans(b'-_', b'+/')


def urlsafe_b64encode(s, *, padded=True):
    """Encode bytes using the URL- and filesystem-safe Base64 alphabet.

    Argument s is a bytes-like object to encode.  The result is returned as a
    bytes object.  The alphabet uses '-' instead of '+' and '_' instead of
    '/'.

    If padded is false, omit padding in the output.
    """
    return binascii.b2a_base64(s, newline=False, padded=padded).translate(
        _urlsafe_encode_translation)


def urlsafe_b64decode(s, *, padded=False):
    """Decode bytes using the URL- and filesystem-safe Base64 alphabet.

    Argument s is a bytes-like object or ASCII string to decode.  The result
    is returned as a bytes object.  A binascii.Error is raised if the input
    is incorrectly padded.  Characters that are not in the URL-safe base-64
    alphabet, and are not a plus '+' or slash '/', are discarded prior to the
    padding check.

    If padded is false, padding in input is not required.

    The alphabet uses '-' instead of '+' and '_' instead of '/'.
    """
    s = _bytes_from_decode_data(s)
    for c in b'+/':
        if c in s:
            import warnings
            warnings.warn(f"invalid character {chr(c)!a} in URL-safe Base64 "
                          f"data will be discarded in future Python versions",
                          FutureWarning, stacklevel=2)
    s = s.translate(_urlsafe_decode_translation)
    return binascii.a2b_base64(s, strict_mode=False, padded=padded)


# Base32 encoding/decoding

_B32_ENCODE_DOCSTRING = '''
Encode the bytes-like objects using {encoding} and return a bytes object.

If padded is false, omit padding in the output.

If wrapcol is non-zero, insert a newline (b'\\n') character after at most
every wrapcol characters.
'''
_B32_DECODE_DOCSTRING = '''
Decode the {encoding} encoded bytes-like object or ASCII string s.

Optional casefold is a flag specifying whether a lowercase alphabet is
acceptable as input.  For security purposes, the default is False.
{extra_args}
If padded is false, padding in input is not required.

If ignorechars is specified, it should be a bytes-like object containing
characters to ignore from the input.

The result is returned as a bytes object.  A binascii.Error is raised if
the input is incorrectly padded or if there are non-alphabet
characters present in the input.
'''
_B32_DECODE_MAP01_DOCSTRING = '''
RFC 3548 allows for optional mapping of the digit 0 (zero) to the
letter O (oh), and for optional mapping of the digit 1 (one) to
either the letter I (eye) or letter L (el).  The optional argument
map01 when not None, specifies which letter the digit 1 should be
mapped to (when map01 is not None, the digit 0 is always mapped to
the letter O).  For security purposes the default is None, so that
0 and 1 are not allowed in the input.
'''


def b32encode(s, *, padded=True, wrapcol=0):
    return binascii.b2a_base32(s, padded=padded, wrapcol=wrapcol)
b32encode.__doc__ = _B32_ENCODE_DOCSTRING.format(encoding='base32')


def b32decode(s, casefold=False, map01=None, *, padded=True, ignorechars=b''):
    s = _bytes_from_decode_data(s)
    # Optional mapping of the digits 0 -> O and 1 -> map01.
    if map01 is not None:
        map01 = _bytes_from_decode_data(map01)
        if len(map01) != 1:
            raise ValueError(f'invalid map01: {map01!r}')
        s = s.translate(bytes.maketrans(b'01', b'O' + map01))
    if casefold:
        s = s.upper()
    return binascii.a2b_base32(s, padded=padded,
                               ignorechars=_ignorechars_arg(ignorechars))
b32decode.__doc__ = _B32_DECODE_DOCSTRING.format(
    encoding='base32', extra_args=_B32_DECODE_MAP01_DOCSTRING)


def b32hexencode(s, *, padded=True, wrapcol=0):
    return binascii.b2a_base32(s, padded=padded, wrapcol=wrapcol,
                               alphabet=binascii.BASE32HEX_ALPHABET)
b32hexencode.__doc__ = _B32_ENCODE_DOCSTRING.format(encoding='base32hex')


def b32hexdecode(s, casefold=False, *, padded=True, ignorechars=b''):
    s = _bytes_from_decode_data(s)
    if casefold:
        s = s.upper()
    return binascii.a2b_base32(s, padded=padded,
                               alphabet=binascii.BASE32HEX_ALPHABET,
                               ignorechars=_ignorechars_arg(ignorechars))
b32hexdecode.__doc__ = _B32_DECODE_DOCSTRING.format(encoding='base32hex',
                                                    extra_args='')


# Base16 output is upper case (RFC 4648); decoding is case sensitive unless
# casefold is requested.
def b16encode(s, *, wrapcol=0):
    """Encode the bytes-like object s using Base16 and return a bytes object.

    If wrapcol is non-zero, insert a newline (b'\\n') character after at most
    every wrapcol characters.
    """
    wrapcol = _wrapcol_arg(wrapcol)
    return _wrap(binascii.hexlify(s).upper(), wrapcol, 2)


def b16decode(s, casefold=False, *, ignorechars=b''):
    """Decode the Base16 encoded bytes-like object or ASCII string s.

    Optional casefold is a flag specifying whether a lowercase alphabet is
    acceptable as input.  For security purposes, the default is False.

    If ignorechars is specified, it should be a bytes-like object containing
    characters to ignore from the input.

    The result is returned as a bytes object.  A binascii.Error is raised if
    s is incorrectly padded or if there are non-alphabet characters present
    in the input.
    """
    s = _bytes_from_decode_data(s)
    if casefold:
        s = s.upper()
    ignorechars = _ignorechars_arg(ignorechars)
    if ignorechars:
        s = s.translate(None, ignorechars)
    if s.translate(None, b'0123456789ABCDEF'):
        raise binascii.Error('Non-base16 digit found')
    return binascii.unhexlify(s)


#
# Ascii85 encoding/decoding
#

def a85encode(b, *, foldspaces=False, wrapcol=0, pad=False, adobe=False):
    """Encode bytes-like object b using Ascii85 and return a bytes object.

    foldspaces is an optional flag that uses the special short sequence 'y'
    instead of 4 consecutive spaces (ASCII 0x20) as supported by 'btoa'. This
    feature is not supported by the "standard" Adobe encoding.

    If wrapcol is non-zero, insert a newline (b'\\n') character after at most
    every wrapcol characters.

    pad controls whether the input is padded to a multiple of 4 before
    encoding. Note that the btoa implementation always pads.

    adobe controls whether the encoded byte sequence is framed with <~ and ~>,
    which is used by the Adobe implementation.
    """
    return binascii.b2a_ascii85(b, foldspaces=foldspaces, wrapcol=wrapcol,
                                pad=pad, adobe=adobe)


def a85decode(b, *, foldspaces=False, adobe=False, ignorechars=b' \t\n\r\v'):
    """Decode the Ascii85 encoded bytes-like object or ASCII string b.

    foldspaces is a flag that specifies whether the 'y' short sequence should
    be accepted as shorthand for 4 consecutive spaces (ASCII 0x20). This
    feature is not supported by the "standard" Adobe encoding.

    adobe controls whether the input sequence is in Adobe Ascii85 format
    (i.e. is framed with <~ and ~>).

    ignorechars should be a bytes-like object or ASCII string containing
    characters to ignore from the input. This should only contain whitespace
    characters, and by default contains all whitespace characters in ASCII.

    The result is returned as a bytes object.
    """
    if isinstance(ignorechars, str):
        ignorechars = ignorechars.encode('ascii')
    return binascii.a2b_ascii85(b, foldspaces=foldspaces, adobe=adobe,
                                ignorechars=ignorechars)


def b85encode(b, pad=False, *, wrapcol=0):
    """Encode bytes-like object b in base85 format and return a bytes object.

    If wrapcol is non-zero, insert a newline (b'\\n') character after at most
    every wrapcol characters.

    If pad is true, the input is padded with b'\\0' so its length is a
    multiple of 4 bytes before encoding.
    """
    return binascii.b2a_base85(b, pad=pad, wrapcol=wrapcol)


def b85decode(b, *, ignorechars=b''):
    """Decode the base85-encoded bytes-like object or ASCII string b

    If ignorechars is specified, it should be a bytes-like object containing
    characters to ignore from the input.

    The result is returned as a bytes object.
    """
    return binascii.a2b_base85(b, ignorechars=ignorechars)


def z85encode(s, pad=False, *, wrapcol=0):
    """Encode bytes-like object b in z85 format and return a bytes object.

    If wrapcol is non-zero, insert a newline (b'\\n') character after at most
    every wrapcol characters.

    If pad is true, the input is padded with b'\\0' so its length is a
    multiple of 4 bytes before encoding.
    """
    return binascii.b2a_base85(s, pad=pad, wrapcol=wrapcol,
                               alphabet=binascii.Z85_ALPHABET)


def z85decode(s, *, ignorechars=b''):
    """Decode the z85-encoded bytes-like object or ASCII string b

    If ignorechars is specified, it should be a bytes-like object containing
    characters to ignore from the input.

    The result is returned as a bytes object.
    """
    return binascii.a2b_base85(s, alphabet=binascii.Z85_ALPHABET,
                               ignorechars=ignorechars)


# Legacy RFC 2045 helpers (MIME line length, binary file objects).

MAXLINESIZE = 76  # Excluding the CRLF
MAXBINSIZE = (MAXLINESIZE // 4) * 3


def encode(input, output):
    """Encode a file; input and output are binary files."""
    while s := input.read(MAXBINSIZE):
        while len(s) < MAXBINSIZE and (ns := input.read(MAXBINSIZE - len(s))):
            s += ns
        output.write(binascii.b2a_base64(s))


def decode(input, output):
    """Decode a file; input and output are binary files."""
    while line := input.readline():
        output.write(binascii.a2b_base64(line))


def _input_type_check(s):
    try:
        m = memoryview(s)
    except TypeError as err:
        msg = "expected bytes-like object, not %s" % s.__class__.__name__
        raise TypeError(msg) from err
    if m.format not in ('c', 'b', 'B'):
        msg = ("expected single byte elements, not %r from %s" %
               (m.format, s.__class__.__name__))
        raise TypeError(msg)
    if m.ndim != 1:
        msg = ("expected 1-D data, not %d-D data from %s" %
               (m.ndim, s.__class__.__name__))
        raise TypeError(msg)


def encodebytes(s):
    """Encode a bytestring into a bytes object containing multiple lines
    of base-64 data."""
    _input_type_check(s)
    return binascii.b2a_base64(s, wrapcol=MAXLINESIZE)


def decodebytes(s):
    """Decode a bytestring of base-64 data into a bytes object."""
    _input_type_check(s)
    return binascii.a2b_base64(s)


# Command line interface
def main():
    """Encode or decode stdin or a file to stdout."""
    import sys, getopt
    usage = f"""usage: {sys.argv[0]} [-h|-d|-e|-u] [file|-]
        -h: print this help message and exit
        -d, -u: decode
        -e: encode (default)"""
    try:
        opts, args = getopt.getopt(sys.argv[1:], 'hdeu')
    except getopt.error as msg:
        sys.stdout = sys.stderr
        print(msg)
        print(usage)
        sys.exit(2)
    func = encode
    for o, a in opts:
        if o == '-e': func = encode
        if o == '-d': func = decode
        if o == '-u': func = decode
        if o == '-h': print(usage); return
    if args and args[0] != '-':
        with open(args[0], 'rb') as f:
            func(f, sys.stdout.buffer)
    else:
        if sys.stdin.isatty():
            # gh-138775: read terminal input data all at once to detect EOF
            import io
            data = sys.stdin.buffer.read()
            buffer = io.BytesIO(data)
        else:
            buffer = sys.stdin.buffer
        func(buffer, sys.stdout.buffer)


if __name__ == '__main__':
    main()
