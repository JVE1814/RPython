"""The 'utf-16' codec: UTF-16 with a byte order mark.

Encoding writes a BOM followed by native byte order data; decoding uses the
BOM at the start of the data to choose the byte order.
"""

import codecs
import sys

encode = codecs.utf_16_encode


def decode(input, errors='strict'):
    return codecs.utf_16_decode(input, errors, True)


if sys.byteorder == 'little':
    _native_encode = codecs.utf_16_le_encode
    _native_decode = codecs.utf_16_le_decode
    _foreign_decode = codecs.utf_16_be_decode
else:
    _native_encode = codecs.utf_16_be_encode
    _native_decode = codecs.utf_16_be_decode
    _foreign_decode = codecs.utf_16_le_decode


def _no_bom(input):
    return UnicodeDecodeError('utf-16', bytes(input[:2]), 0, 2,
                              'UTF-16 stream does not start with BOM')


class IncrementalEncoder(codecs.IncrementalEncoder):
    def __init__(self, errors='strict'):
        codecs.IncrementalEncoder.__init__(self, errors)
        self.encoder = None

    def encode(self, input, final=False):
        if self.encoder is None:
            result = codecs.utf_16_encode(input, self.errors)[0]
            self.encoder = _native_encode
            return result
        return self.encoder(input, self.errors)[0]

    def reset(self):
        codecs.IncrementalEncoder.reset(self)
        self.encoder = None

    def getstate(self):
        # 2: the BOM still has to be written; 0: it has been written.
        return (2 if self.encoder is None else 0)

    def setstate(self, state):
        if state:
            self.encoder = None
        else:
            self.encoder = _native_encode


class IncrementalDecoder(codecs.BufferedIncrementalDecoder):
    def __init__(self, errors='strict'):
        codecs.BufferedIncrementalDecoder.__init__(self, errors)
        self.decoder = None

    def _buffer_decode(self, input, errors, final):
        if self.decoder is None:
            (output, consumed, byteorder) = \
                codecs.utf_16_ex_decode(input, errors, 0, final)
            if byteorder == -1:
                self.decoder = codecs.utf_16_le_decode
            elif byteorder == 1:
                self.decoder = codecs.utf_16_be_decode
            elif consumed >= 2:
                raise _no_bom(input)
            return (output, consumed)
        return self.decoder(input, self.errors, final)

    def reset(self):
        codecs.BufferedIncrementalDecoder.reset(self)
        self.decoder = None

    def getstate(self):
        buffered = codecs.BufferedIncrementalDecoder.getstate(self)[0]
        # Extra state: 0 = native byte order, 1 = foreign byte order,
        # 2 = byte order not known yet.
        if self.decoder is None:
            return (buffered, 2)
        return (buffered, int(self.decoder is not _native_decode))

    def setstate(self, state):
        codecs.BufferedIncrementalDecoder.setstate(self, state)
        flag = state[1]
        if flag == 0:
            self.decoder = _native_decode
        elif flag == 1:
            self.decoder = _foreign_decode
        else:
            self.decoder = None


class StreamWriter(codecs.StreamWriter):
    def __init__(self, stream, errors='strict'):
        codecs.StreamWriter.__init__(self, stream, errors)
        self.encoder = None

    def reset(self):
        codecs.StreamWriter.reset(self)
        self.encoder = None

    def encode(self, input, errors='strict'):
        if self.encoder is None:
            result = codecs.utf_16_encode(input, errors)
            self.encoder = _native_encode
            return result
        return self.encoder(input, errors)


class StreamReader(codecs.StreamReader):

    def reset(self):
        codecs.StreamReader.reset(self)
        try:
            del self.decode
        except AttributeError:
            pass

    def decode(self, input, errors='strict'):
        (output, consumed, byteorder) = \
            codecs.utf_16_ex_decode(input, errors, 0, False)
        if byteorder == -1:
            self.decode = codecs.utf_16_le_decode
        elif byteorder == 1:
            self.decode = codecs.utf_16_be_decode
        elif consumed >= 2:
            raise _no_bom(input)
        return (output, consumed)


def getregentry():
    return codecs.CodecInfo(
        name='utf-16',
        encode=encode,
        decode=decode,
        incrementalencoder=IncrementalEncoder,
        incrementaldecoder=IncrementalDecoder,
        streamreader=StreamReader,
        streamwriter=StreamWriter,
    )
