"""The 'uu' bytes-to-bytes codec (uuencode with begin/end lines)."""

import codecs
import binascii
from io import BytesIO


def uu_encode(input, errors='strict', filename='<data>', mode=0o666):
    assert errors == 'strict'
    infile = BytesIO(bytes(input))
    outfile = BytesIO()
    write = outfile.write
    # Newlines and carriage returns would end the header line early.
    filename = filename.replace('\n', '\\n').replace('\r', '\\r')
    write(('begin %o %s\n' % (mode & 0o777, filename)).encode('ascii'))
    chunk = infile.read(45)
    while chunk:
        write(binascii.b2a_uu(chunk))
        chunk = infile.read(45)
    write(b' \nend\n')
    return (outfile.getvalue(), len(input))


def uu_decode(input, errors='strict'):
    assert errors == 'strict'
    infile = BytesIO(bytes(input))
    outfile = BytesIO()
    readline = infile.readline
    write = outfile.write

    # Find the begin line.
    while True:
        s = readline()
        if not s:
            raise ValueError('Missing "begin" line in input data')
        if s[:5] == b'begin':
            break

    while True:
        s = readline()
        if not s or s == b'end\n':
            break
        try:
            data = binascii.a2b_uu(s)
        except binascii.Error:
            # Some encoders pad lines with garbage: decode only the number
            # of characters announced by the length byte.
            nbytes = (((s[0] - 32) & 63) * 4 + 5) // 3
            data = binascii.a2b_uu(s[:nbytes])
        write(data)
    if not s:
        raise ValueError('Truncated input data')

    return (outfile.getvalue(), len(input))


class Codec(codecs.Codec):
    def encode(self, input, errors='strict'):
        return uu_encode(input, errors)

    def decode(self, input, errors='strict'):
        return uu_decode(input, errors)


class IncrementalEncoder(codecs.IncrementalEncoder):
    def encode(self, input, final=False):
        return uu_encode(input, self.errors)[0]


class IncrementalDecoder(codecs.IncrementalDecoder):
    def decode(self, input, final=False):
        return uu_decode(input, self.errors)[0]


class StreamWriter(Codec, codecs.StreamWriter):
    charbuffertype = bytes


class StreamReader(Codec, codecs.StreamReader):
    charbuffertype = bytes


def getregentry():
    return codecs.CodecInfo(
        name='uu',
        encode=uu_encode,
        decode=uu_decode,
        incrementalencoder=IncrementalEncoder,
        incrementaldecoder=IncrementalDecoder,
        streamreader=StreamReader,
        streamwriter=StreamWriter,
        _is_text_encoding=False,
    )
