"""The 'raw-unicode-escape' codec: Latin-1 plus \\uXXXX and \\UXXXXXXXX escapes."""

import codecs


class Codec(codecs.Codec):
    encode = codecs.raw_unicode_escape_encode
    decode = codecs.raw_unicode_escape_decode


class IncrementalEncoder(codecs.IncrementalEncoder):
    def encode(self, input, final=False):
        return codecs.raw_unicode_escape_encode(input, self.errors)[0]


class IncrementalDecoder(codecs.BufferedIncrementalDecoder):
    def _buffer_decode(self, input, errors, final):
        return codecs.raw_unicode_escape_decode(input, errors, final)


class StreamWriter(Codec, codecs.StreamWriter):
    pass


class StreamReader(Codec, codecs.StreamReader):
    def decode(self, input, errors='strict'):
        return codecs.raw_unicode_escape_decode(input, errors, False)


def getregentry():
    return codecs.CodecInfo(
        name='raw-unicode-escape',
        encode=Codec.encode,
        decode=Codec.decode,
        incrementalencoder=IncrementalEncoder,
        incrementaldecoder=IncrementalDecoder,
        streamwriter=StreamWriter,
        streamreader=StreamReader,
    )
