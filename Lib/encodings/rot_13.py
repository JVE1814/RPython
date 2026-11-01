"""The 'rot-13' str-to-str codec (Caesar cipher by 13 places).

This is a text transform, not a text encoding: use codecs.encode() /
codecs.decode() with it.
"""

import codecs


class Codec(codecs.Codec):
    def encode(self, input, errors='strict'):
        return (str.translate(input, rot13_map), len(input))

    def decode(self, input, errors='strict'):
        return (str.translate(input, rot13_map), len(input))


class IncrementalEncoder(codecs.IncrementalEncoder):
    def encode(self, input, final=False):
        return str.translate(input, rot13_map)


class IncrementalDecoder(codecs.IncrementalDecoder):
    def decode(self, input, final=False):
        return str.translate(input, rot13_map)


class StreamWriter(Codec, codecs.StreamWriter):
    pass


class StreamReader(Codec, codecs.StreamReader):
    pass


def getregentry():
    return codecs.CodecInfo(
        name='rot-13',
        encode=Codec().encode,
        decode=Codec().decode,
        incrementalencoder=IncrementalEncoder,
        incrementaldecoder=IncrementalDecoder,
        streamwriter=StreamWriter,
        streamreader=StreamReader,
        _is_text_encoding=False,
    )


def _make_map():
    m = {}
    for first, last in (('a', 'z'), ('A', 'Z')):
        base = ord(first)
        for i in range(26):
            m[base + i] = base + (i + 13) % 26
    return m


rot13_map = _make_map()


def rot13(infile, outfile):
    outfile.write(codecs.encode(infile.read(), 'rot-13'))


if __name__ == '__main__':
    import sys
    rot13(sys.stdin, sys.stdout)
