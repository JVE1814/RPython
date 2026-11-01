"""The 'punycode' codec (RFC 3492 bootstring encoding of Unicode)."""

import codecs

# Bootstring parameters for Punycode (RFC 3492, section 5).
_BASE = 36
_TMIN = 1
_TMAX = 26
_SKEW = 38
_DAMP = 700
_INITIAL_BIAS = 72
_INITIAL_N = 0x80

_DIGITS = b"abcdefghijklmnopqrstuvwxyz0123456789"


def _threshold(j, bias):
    t = _BASE * (j + 1) - bias
    if t < _TMIN:
        return _TMIN
    if t > _TMAX:
        return _TMAX
    return t


def _adapt(delta, first, numpoints):
    delta = delta // (_DAMP if first else 2)
    delta += delta // numpoints
    k = 0
    while delta > ((_BASE - _TMIN) * _TMAX) // 2:
        delta = delta // (_BASE - _TMIN)
        k += _BASE
    return k + ((_BASE - _TMIN + 1) * delta) // (delta + _SKEW)


# ---------------------------------------------------------------- encoding

def _encode_integer(n, bias, out):
    j = 0
    while True:
        t = _threshold(j, bias)
        if n < t:
            out.append(_DIGITS[n])
            return
        out.append(_DIGITS[t + (n - t) % (_BASE - t)])
        n = (n - t) // (_BASE - t)
        j += 1


def punycode_encode(text):
    basic = bytearray()
    others = set()
    for c in text:
        if ord(c) < 0x80:
            basic.append(ord(c))
        else:
            others.add(ord(c))

    out = bytearray()
    n = _INITIAL_N
    delta = 0
    bias = _INITIAL_BIAS
    handled = len(basic)
    first = True
    codes = [ord(c) for c in text]
    for m in sorted(others):
        # Skip the states for code points below m (handled + 1 insertion
        # positions per code point value).
        delta += (m - n) * (handled + 1)
        n = m
        for c in codes:
            if c < n:
                delta += 1
            elif c == n:
                _encode_integer(delta, bias, out)
                handled += 1
                bias = _adapt(delta, first, handled)
                first = False
                delta = 0
        delta += 1
        n += 1
    if basic:
        return bytes(basic) + b"-" + bytes(out)
    return bytes(out)


# ---------------------------------------------------------------- decoding

def _digit_value(c):
    if 0x41 <= c <= 0x5A:     # A-Z
        return c - 0x41
    if 0x61 <= c <= 0x7A:     # a-z
        return c - 0x61
    if 0x30 <= c <= 0x39:     # 0-9
        return c - 22
    return -1


def punycode_decode(data, errors):
    if isinstance(data, str):
        data = data.encode("ascii")
    data = bytes(data)
    sep = data.rfind(b"-")
    if sep == -1:
        basic_bytes = b""
        ext_start = 0
    else:
        basic_bytes = data[:sep]
        ext_start = sep + 1

    for i, b in enumerate(basic_bytes):
        if b >= 0x80:
            if errors == "strict":
                raise UnicodeDecodeError("ascii", data, i, i + 1,
                                         "ordinal not in range(128)")
            break
    output = list(str(basic_bytes, "ascii", errors))

    n = _INITIAL_N
    i = 0
    bias = _INITIAL_BIAS
    pos = ext_start
    end = len(data)
    first = True
    while pos < end:
        old_i = i
        w = 1
        j = 0
        while True:
            if pos >= end:
                if errors == "strict":
                    raise UnicodeDecodeError("punycode", data, pos - 1, pos,
                                             "incomplete punycode string")
                return "".join(output)
            digit = _digit_value(data[pos])
            if digit < 0:
                if errors == "strict":
                    raise UnicodeDecodeError(
                        "punycode", data, pos, pos + 1,
                        "Invalid extended code point '%s'" % chr(data[pos]))
                return "".join(output)
            pos += 1
            i += digit * w
            t = _threshold(j, bias)
            if digit < t:
                break
            w *= _BASE - t
            j += 1
        count = len(output) + 1
        bias = _adapt(i - old_i, first, count)
        first = False
        n += i // count
        i %= count
        if n > 0x10FFFF:
            if errors == "strict":
                raise UnicodeDecodeError("punycode", data, ext_start, end,
                                         "Invalid character U+%x" % n)
            n = ord('?')
        output.insert(i, chr(n))
        i += 1
    return "".join(output)


# ---------------------------------------------------------------- codec API

class Codec(codecs.Codec):

    def encode(self, input, errors='strict'):
        res = punycode_encode(input)
        return res, len(input)

    def decode(self, input, errors='strict'):
        if errors not in ('strict', 'replace', 'ignore'):
            raise UnicodeError(f"Unsupported error handling: {errors}")
        res = punycode_decode(input, errors)
        return res, len(input)


class IncrementalEncoder(codecs.IncrementalEncoder):
    def encode(self, input, final=False):
        return punycode_encode(input)


class IncrementalDecoder(codecs.IncrementalDecoder):
    def decode(self, input, final=False):
        if self.errors not in ('strict', 'replace', 'ignore'):
            raise UnicodeError(f"Unsupported error handling: {self.errors}")
        return punycode_decode(input, self.errors)


class StreamWriter(Codec, codecs.StreamWriter):
    pass


class StreamReader(Codec, codecs.StreamReader):
    pass


def getregentry():
    return codecs.CodecInfo(
        name='punycode',
        encode=Codec().encode,
        decode=Codec().decode,
        incrementalencoder=IncrementalEncoder,
        incrementaldecoder=IncrementalDecoder,
        streamwriter=StreamWriter,
        streamreader=StreamReader,
    )
