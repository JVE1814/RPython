"""Codec registry, base classes for codecs and stream wrappers.

The codec functions and the registries themselves are implemented in the
built-in `_codecs` module; this module adds the class-based API
(CodecInfo, Codec, incremental coders, StreamReader/StreamWriter and the
wrappers built from them) and convenience lookup functions.
"""

import builtins
import sys

try:
    from _codecs import *
except ImportError as why:
    raise SystemError('Failed to load the builtin codecs: %s' % why)

__all__ = ["register", "lookup", "open", "EncodedFile", "BOM", "BOM_BE",
           "BOM_LE", "BOM32_BE", "BOM32_LE", "BOM64_BE", "BOM64_LE",
           "BOM_UTF8", "BOM_UTF16", "BOM_UTF16_LE", "BOM_UTF16_BE",
           "BOM_UTF32", "BOM_UTF32_LE", "BOM_UTF32_BE",
           "CodecInfo", "Codec", "IncrementalEncoder", "IncrementalDecoder",
           "StreamReader", "StreamWriter",
           "StreamReaderWriter", "StreamRecoder",
           "getencoder", "getdecoder", "getincrementalencoder",
           "getincrementaldecoder", "getreader", "getwriter",
           "encode", "decode", "iterencode", "iterdecode",
           "strict_errors", "ignore_errors", "replace_errors",
           "xmlcharrefreplace_errors",
           "backslashreplace_errors", "namereplace_errors",
           "register_error", "lookup_error"]

# --------------------------------------------------------------------------
# Byte order marks

BOM_UTF8 = b'\xef\xbb\xbf'

BOM_LE = BOM_UTF16_LE = b'\xff\xfe'
BOM_BE = BOM_UTF16_BE = b'\xfe\xff'
BOM_UTF32_LE = b'\xff\xfe\x00\x00'
BOM_UTF32_BE = b'\x00\x00\xfe\xff'

if sys.byteorder == 'little':
    BOM = BOM_UTF16 = BOM_UTF16_LE
    BOM_UTF32 = BOM_UTF32_LE
else:
    BOM = BOM_UTF16 = BOM_UTF16_BE
    BOM_UTF32 = BOM_UTF32_BE

# Old names kept for compatibility.
BOM32_LE = BOM_UTF16_LE
BOM32_BE = BOM_UTF16_BE
BOM64_LE = BOM_UTF32_LE
BOM64_BE = BOM_UTF32_BE


# --------------------------------------------------------------------------
# Codec description and base classes

class CodecInfo(tuple):
    """Description of a codec as returned by lookup().

    The tuple part is (encode, decode, streamreader, streamwriter); the
    other fields are available as attributes.
    """

    # Codecs that do not convert between str and bytes set this to False so
    # that str.encode()/bytes.decode() refuse to use them.
    _is_text_encoding = True

    def __new__(cls, encode, decode, streamreader=None, streamwriter=None,
                incrementalencoder=None, incrementaldecoder=None, name=None,
                *, _is_text_encoding=None):
        self = tuple.__new__(cls, (encode, decode, streamreader, streamwriter))
        self.name = name
        self.encode = encode
        self.decode = decode
        self.incrementalencoder = incrementalencoder
        self.incrementaldecoder = incrementaldecoder
        self.streamwriter = streamwriter
        self.streamreader = streamreader
        if _is_text_encoding is not None:
            self._is_text_encoding = _is_text_encoding
        return self

    def __repr__(self):
        return "<%s.%s object for encoding %s at %#x>" % (
            self.__class__.__module__, self.__class__.__qualname__,
            self.name, id(self))

    def __getnewargs__(self):
        return tuple(self)


class Codec:
    """Interface of stateless encoders/decoders.

    encode(input, errors='strict') and decode(input, errors='strict') return
    a tuple (output, length_consumed).  The errors argument names an error
    handler registered with register_error(): 'strict', 'ignore',
    'replace', 'surrogateescape', 'xmlcharrefreplace', 'backslashreplace',
    'namereplace', ... .
    """

    def encode(self, input, errors='strict'):
        raise NotImplementedError

    def decode(self, input, errors='strict'):
        raise NotImplementedError


class IncrementalEncoder:
    """Encoder that receives its input in pieces and may keep state
    between calls of encode()."""

    def __init__(self, errors='strict'):
        self.errors = errors
        self.buffer = ""

    def encode(self, input, final=False):
        """Encode input and return the bytes produced so far."""
        raise NotImplementedError

    def reset(self):
        """Return the encoder to its initial state."""

    def getstate(self):
        """Return the current state of the encoder (an int)."""
        return 0

    def setstate(self, state):
        """Restore a state returned by getstate()."""


class BufferedIncrementalEncoder(IncrementalEncoder):
    """IncrementalEncoder that keeps the unencoded tail of its input."""

    def __init__(self, errors='strict'):
        IncrementalEncoder.__init__(self, errors)
        self.buffer = ""

    def _buffer_encode(self, input, errors, final):
        # Must return (output, number_of_characters_consumed).
        raise NotImplementedError

    def encode(self, input, final=False):
        data = self.buffer + input
        result, consumed = self._buffer_encode(data, self.errors, final)
        self.buffer = data[consumed:]
        return result

    def reset(self):
        IncrementalEncoder.reset(self)
        self.buffer = ""

    def getstate(self):
        return self.buffer or 0

    def setstate(self, state):
        self.buffer = state or ""


class IncrementalDecoder:
    """Decoder that receives its input in pieces and may keep state
    between calls of decode()."""

    def __init__(self, errors='strict'):
        self.errors = errors

    def decode(self, input, final=False):
        """Decode input and return the text produced so far."""
        raise NotImplementedError

    def reset(self):
        """Return the decoder to its initial state."""

    def getstate(self):
        """Return (buffered_bytes, flag): the pending input and an int
        describing the rest of the state.  (b"", 0) is the initial state."""
        return (b"", 0)

    def setstate(self, state):
        """Restore a state returned by getstate()."""


class BufferedIncrementalDecoder(IncrementalDecoder):
    """IncrementalDecoder that keeps undecoded trailing input."""

    def __init__(self, errors='strict'):
        IncrementalDecoder.__init__(self, errors)
        self.buffer = b""

    def _buffer_decode(self, input, errors, final):
        # Must return (output, number_of_bytes_consumed).
        raise NotImplementedError

    def decode(self, input, final=False):
        data = self.buffer + input
        result, consumed = self._buffer_decode(data, self.errors, final)
        self.buffer = data[consumed:]
        return result

    def reset(self):
        IncrementalDecoder.reset(self)
        self.buffer = b""

    def getstate(self):
        return (self.buffer, 0)

    def setstate(self, state):
        self.buffer = state[0]


# --------------------------------------------------------------------------
# Streams

class StreamWriter(Codec):

    def __init__(self, stream, errors='strict'):
        """Wrap the binary output stream `stream`.

        Text written with write()/writelines() is encoded with
        self.encode(text, self.errors) before being passed on."""
        self.stream = stream
        self.errors = errors

    def write(self, object):
        """Encode object and write it to the stream."""
        data, consumed = self.encode(object, self.errors)
        self.stream.write(data)

    def writelines(self, list):
        """Write the concatenation of the strings in list."""
        self.write(''.join(list))

    def reset(self):
        """Discard the encoder state (output already written is kept)."""

    def seek(self, offset, whence=0):
        self.stream.seek(offset, whence)
        if whence == 0 and offset == 0:
            self.reset()

    def __getattr__(self, name, getattr=getattr):
        """Everything else is looked up on the underlying stream."""
        return getattr(self.stream, name)

    def __enter__(self):
        return self

    def __exit__(self, type, value, tb):
        self.stream.close()

    def __reduce_ex__(self, proto):
        raise TypeError("can't serialize %s" % self.__class__.__name__)


class StreamReader(Codec):

    charbuffertype = str

    def __init__(self, stream, errors='strict'):
        """Wrap the binary input stream `stream`; read data is decoded
        with self.decode(data, self.errors)."""
        self.stream = stream
        self.errors = errors
        self.bytebuffer = b""
        self._empty_charbuffer = self.charbuffertype()
        self.charbuffer = self._empty_charbuffer
        self.linebuffer = None

    def decode(self, input, errors='strict'):
        raise NotImplementedError

    def read(self, size=-1, chars=-1, firstline=False):
        """Decode and return text from the stream.

        chars is the number of characters to return (-1 = as many as are
        available); size is the approximate number of bytes to read from
        the stream per attempt (-1 = everything).  With firstline=True only
        the first line is returned if a decoding error occurs later.
        """
        # Lines pushed back by readline() go back into the char buffer.
        if self.linebuffer:
            self.charbuffer = self._empty_charbuffer.join(self.linebuffer)
            self.linebuffer = None

        if chars < 0:
            # size has the meaning of chars when chars is not given
            chars = size

        while True:
            if chars >= 0 and len(self.charbuffer) >= chars:
                break
            if size < 0:
                newdata = self.stream.read()
            else:
                newdata = self.stream.read(size)
            data = self.bytebuffer + newdata
            if not data:
                break
            try:
                newchars, decodedbytes = self.decode(data, self.errors)
            except UnicodeDecodeError as exc:
                if firstline:
                    newchars, decodedbytes = \
                        self.decode(data[:exc.start], self.errors)
                    lines = newchars.splitlines(keepends=True)
                    if len(lines) <= 1:
                        raise
                else:
                    raise
            self.bytebuffer = data[decodedbytes:]
            self.charbuffer += newchars
            if not newdata:
                break

        if chars < 0:
            result = self.charbuffer
            self.charbuffer = self._empty_charbuffer
        else:
            result = self.charbuffer[:chars]
            self.charbuffer = self.charbuffer[chars:]
        return result

    def readline(self, size=None, keepends=True):
        """Read and return one line of text.

        size is passed on to read(); keepends=False strips the line end."""
        if self.linebuffer:
            line = self.linebuffer[0]
            del self.linebuffer[0]
            if len(self.linebuffer) == 1:
                # the last element is an incomplete line: keep it in the
                # character buffer instead
                self.charbuffer = self.linebuffer[0]
                self.linebuffer = None
            if not keepends:
                line = line.splitlines(keepends=False)[0]
            return line

        readsize = size or 72
        line = self._empty_charbuffer
        while True:
            data = self.read(readsize, firstline=True)
            if data:
                # A trailing "\r" may be the first half of "\r\n": read one
                # more character to find out.
                if (isinstance(data, str) and data.endswith("\r")) or \
                   (isinstance(data, bytes) and data.endswith(b"\r")):
                    data += self.read(size=1, chars=1)

            line += data
            lines = line.splitlines(keepends=True)
            if lines:
                if len(lines) > 1:
                    # More than one line: keep the rest for later calls.
                    line = lines[0]
                    del lines[0]
                    if len(lines) > 1:
                        lines[-1] += self.charbuffer
                        self.linebuffer = lines
                        self.charbuffer = None
                    else:
                        self.charbuffer = lines[0] + self.charbuffer
                    if not keepends:
                        line = line.splitlines(keepends=False)[0]
                    break
                line0withend = lines[0]
                line0withoutend = lines[0].splitlines(keepends=False)[0]
                if line0withend != line0withoutend:
                    # The line is complete.
                    self.charbuffer = self._empty_charbuffer.join(lines[1:]) + \
                                      self.charbuffer
                    if keepends:
                        line = line0withend
                    else:
                        line = line0withoutend
                    break
            if not data or size is not None:
                if line and not keepends:
                    line = line.splitlines(keepends=False)[0]
                break
            if readsize < 8000:
                readsize *= 2
        return line

    def readlines(self, sizehint=None, keepends=True):
        """Read all remaining lines and return them as a list."""
        data = self.read()
        return data.splitlines(keepends)

    def reset(self):
        """Drop all buffered data and the decoder state."""
        self.bytebuffer = b""
        self.charbuffer = self._empty_charbuffer
        self.linebuffer = None

    def seek(self, offset, whence=0):
        """Seek the underlying stream and reset the decoder state."""
        self.stream.seek(offset, whence)
        self.reset()

    def __next__(self):
        """Return the next decoded line."""
        line = self.readline()
        if line:
            return line
        raise StopIteration

    def __iter__(self):
        return self

    def __getattr__(self, name, getattr=getattr):
        return getattr(self.stream, name)

    def __enter__(self):
        return self

    def __exit__(self, type, value, tb):
        self.stream.close()

    def __reduce_ex__(self, proto):
        raise TypeError("can't serialize %s" % self.__class__.__name__)


class StreamReaderWriter:
    """Combination of a StreamReader and a StreamWriter on one stream."""

    encoding = 'unknown'

    def __init__(self, stream, Reader, Writer, errors='strict'):
        self.stream = stream
        self.reader = Reader(stream, errors)
        self.writer = Writer(stream, errors)
        self.errors = errors

    def read(self, size=-1):
        return self.reader.read(size)

    def readline(self, size=None, keepends=True):
        return self.reader.readline(size, keepends)

    def readlines(self, sizehint=None, keepends=True):
        return self.reader.readlines(sizehint, keepends)

    def __next__(self):
        return next(self.reader)

    def __iter__(self):
        return self

    def write(self, data):
        return self.writer.write(data)

    def writelines(self, list):
        return self.writer.writelines(list)

    def reset(self):
        self.reader.reset()
        self.writer.reset()

    def seek(self, offset, whence=0):
        self.stream.seek(offset, whence)
        self.reader.reset()
        if whence == 0 and offset == 0:
            self.writer.reset()

    def __getattr__(self, name, getattr=getattr):
        return getattr(self.stream, name)

    def __enter__(self):
        return self

    def __exit__(self, type, value, tb):
        self.stream.close()

    def __reduce_ex__(self, proto):
        raise TypeError("can't serialize %s" % self.__class__.__name__)


class StreamRecoder:
    """Stream wrapper that converts data between two encodings.

    Data read from the wrapped stream is decoded with Reader and the result
    encoded with `encode`; data written is decoded with `decode` and then
    written through Writer.
    """

    data_encoding = 'unknown'
    file_encoding = 'unknown'

    def __init__(self, stream, encode, decode, Reader, Writer,
                 errors='strict'):
        self.stream = stream
        self.encode = encode
        self.decode = decode
        self.reader = Reader(stream, errors)
        self.writer = Writer(stream, errors)
        self.errors = errors

    def read(self, size=-1):
        data = self.reader.read(size)
        data, bytesencoded = self.encode(data, self.errors)
        return data

    def readline(self, size=None):
        if size is None:
            data = self.reader.readline()
        else:
            data = self.reader.readline(size)
        data, bytesencoded = self.encode(data, self.errors)
        return data

    def readlines(self, sizehint=None):
        data = self.reader.read()
        data, bytesencoded = self.encode(data, self.errors)
        return data.splitlines(keepends=True)

    def __next__(self):
        data = next(self.reader)
        data, bytesencoded = self.encode(data, self.errors)
        return data

    def __iter__(self):
        return self

    def write(self, data):
        data, bytesdecoded = self.decode(data, self.errors)
        return self.writer.write(data)

    def writelines(self, list):
        data = b''.join(list)
        data, bytesdecoded = self.decode(data, self.errors)
        return self.writer.write(data)

    def reset(self):
        self.reader.reset()
        self.writer.reset()

    def seek(self, offset, whence=0):
        # Recoding streams can only be repositioned to the very start.
        self.reader.seek(offset, whence)
        self.writer.seek(offset, whence)

    def __getattr__(self, name, getattr=getattr):
        return getattr(self.stream, name)

    def __enter__(self):
        return self

    def __exit__(self, type, value, tb):
        self.stream.close()

    def __reduce_ex__(self, proto):
        raise TypeError("can't serialize %s" % self.__class__.__name__)


# --------------------------------------------------------------------------
# Shortcuts

def open(filename, mode='r', encoding=None, errors='strict', buffering=-1):
    """Open a file and wrap it in a StreamReaderWriter for `encoding`.

    The file is always opened in binary mode.  Deprecated: use the
    built-in open() with an encoding argument instead.
    """
    import warnings
    warnings.warn("codecs.open() is deprecated. Use open() instead.",
                  DeprecationWarning, stacklevel=2)

    if encoding is not None and 'b' not in mode:
        mode = mode + 'b'
    file = builtins.open(filename, mode, buffering)
    if encoding is None:
        return file

    try:
        info = lookup(encoding)
        srw = StreamReaderWriter(file, info.streamreader, info.streamwriter, errors)
        srw.encoding = encoding
        return srw
    except:
        file.close()
        raise


def EncodedFile(file, data_encoding, file_encoding=None, errors='strict'):
    """Wrap `file` so that text in data_encoding is transparently recoded
    to file_encoding (default: same as data_encoding) when writing, and
    back when reading."""
    if file_encoding is None:
        file_encoding = data_encoding
    data_info = lookup(data_encoding)
    file_info = lookup(file_encoding)
    sr = StreamRecoder(file, data_info.encode, data_info.decode,
                       file_info.streamreader, file_info.streamwriter, errors)
    sr.data_encoding = data_encoding
    sr.file_encoding = file_encoding
    return sr


def getencoder(encoding):
    """Return the stateless encoder function of the codec `encoding`."""
    return lookup(encoding).encode


def getdecoder(encoding):
    """Return the stateless decoder function of the codec `encoding`."""
    return lookup(encoding).decode


def getincrementalencoder(encoding):
    """Return the IncrementalEncoder class of the codec `encoding`."""
    encoder = lookup(encoding).incrementalencoder
    if encoder is None:
        raise LookupError(encoding)
    return encoder


def getincrementaldecoder(encoding):
    """Return the IncrementalDecoder class of the codec `encoding`."""
    decoder = lookup(encoding).incrementaldecoder
    if decoder is None:
        raise LookupError(encoding)
    return decoder


def getreader(encoding):
    """Return the StreamReader class of the codec `encoding`."""
    return lookup(encoding).streamreader


def getwriter(encoding):
    """Return the StreamWriter class of the codec `encoding`."""
    return lookup(encoding).streamwriter


def iterencode(iterator, encoding, errors='strict', **kwargs):
    """Encode the strings produced by `iterator` incrementally."""
    encoder = getincrementalencoder(encoding)(errors, **kwargs)
    for input in iterator:
        output = encoder.encode(input)
        if output:
            yield output
    output = encoder.encode("", True)
    if output:
        yield output


def iterdecode(iterator, encoding, errors='strict', **kwargs):
    """Decode the bytes objects produced by `iterator` incrementally."""
    decoder = getincrementaldecoder(encoding)(errors, **kwargs)
    for input in iterator:
        output = decoder.decode(input)
        if output:
            yield output
    output = decoder.decode(b"", True)
    if output:
        yield output


# --------------------------------------------------------------------------
# Helpers for charmap codecs

def make_identity_dict(rng):
    """Return a dict mapping every element of rng to itself."""
    return {i: i for i in rng}


def make_encoding_map(decoding_map):
    """Invert a decoding map.  Targets that occur more than once map to
    None (undefined) so that they cannot be encoded ambiguously."""
    m = {}
    for k, v in decoding_map.items():
        if v not in m:
            m[v] = k
        else:
            m[v] = None
    return m


# --------------------------------------------------------------------------
# The built-in error handlers

try:
    strict_errors = lookup_error("strict")
    ignore_errors = lookup_error("ignore")
    replace_errors = lookup_error("replace")
    xmlcharrefreplace_errors = lookup_error("xmlcharrefreplace")
    backslashreplace_errors = lookup_error("backslashreplace")
    namereplace_errors = lookup_error("namereplace")
except LookupError:
    # Without the built-in handlers (e.g. an incomplete bootstrap) these
    # names are simply unavailable.
    strict_errors = None
    ignore_errors = None
    replace_errors = None
    xmlcharrefreplace_errors = None
    backslashreplace_errors = None
    namereplace_errors = None

# Make sure the encodings package (the standard search function) is loaded.
_false = 0
if _false:
    import encodings
