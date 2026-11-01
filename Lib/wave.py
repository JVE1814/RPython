"""Read and write WAV (RIFF WAVE) sound files.

open(file, mode) returns a Wave_read for mode 'r'/'rb' and a Wave_write
for 'w'/'wb'.  Uncompressed integer PCM (1 to 4 bytes per sample), IEEE
floating point (4 or 8 bytes) and, for reading, WAVE_FORMAT_EXTENSIBLE
files holding PCM data are supported.  Frames are exchanged as raw bytes
in native byte order (8-bit samples are unsigned).
"""

import builtins
import os
import struct
import sys
from collections import namedtuple

__all__ = ["open", "Error", "Wave_read", "Wave_write",
           "WAVE_FORMAT_PCM", "WAVE_FORMAT_IEEE_FLOAT", "WAVE_FORMAT_EXTENSIBLE"]


class Error(Exception):
    pass


WAVE_FORMAT_PCM = 0x0001
WAVE_FORMAT_IEEE_FLOAT = 0x0003
WAVE_FORMAT_EXTENSIBLE = 0xFFFE
# GUID of integer PCM data inside a WAVE_FORMAT_EXTENSIBLE header
KSDATAFORMAT_SUBTYPE_PCM = bytes.fromhex("0100000000001000800000aa00389b71")
_KSDATAFORMAT_SUBTYPE_IEEE_FLOAT = bytes.fromhex("0300000000001000800000aa00389b71")

_wave_params = namedtuple('_wave_params',
                          'nchannels sampwidth framerate nframes comptype compname')


def _byteswap(data, width):
    """Reverse the byte order of every `width`-byte group of `data`."""
    swapped = bytearray(len(data))
    for index in range(width):
        swapped[index::width] = data[width - 1 - index::width]
    return bytes(swapped)


def _to_native(data, sampwidth):
    if sampwidth > 1 and sys.byteorder == 'big':
        return _byteswap(data, sampwidth)
    return data


def _read_exact(stream, size):
    data = stream.read(size)
    return data if data is not None else b''


def _skip(stream, size):
    if size <= 0:
        return
    try:
        stream.seek(size, 1)
        return
    except (AttributeError, OSError, ValueError):
        pass
    while size > 0:
        block = _read_exact(stream, min(size, 65536))
        if not block:
            return
        size -= len(block)


def _tell(stream):
    try:
        return stream.tell()
    except (AttributeError, OSError, ValueError):
        return None


class Wave_read:
    """Reader for a WAV file (a file name, path-like or binary file object)."""

    def __init__(self, f):
        self._i_opened_the_file = None
        self._file = None
        if isinstance(f, (str, bytes)) or hasattr(f, '__fspath__'):
            f = builtins.open(f, 'rb')
            self._i_opened_the_file = f
        try:
            self._parse(f)
        except:
            if self._i_opened_the_file:
                f.close()
            raise

    def __del__(self):
        self.close()

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()

    def _parse(self, stream):
        self._file = stream
        head = _read_exact(stream, 8)
        if len(head) < 8:
            raise EOFError
        if head[:4] != b'RIFF':
            raise Error('file does not start with RIFF id')
        if _read_exact(stream, 4) != b'WAVE':
            raise Error('not a WAVE file')
        have_format = False
        self._data_size = None
        while True:
            head = _read_exact(stream, 8)
            if len(head) < 8:
                break
            kind = head[:4]
            size = struct.unpack('<L', head[4:])[0]
            if kind == b'fmt ':
                body = _read_exact(stream, size)
                self._parse_format(body)
                have_format = True
                if size & 1:
                    _skip(stream, 1)
            elif kind == b'data':
                if not have_format:
                    raise Error('data chunk before fmt chunk')
                self._data_size = size
                self._data_start = _tell(stream)
                break
            else:
                _skip(stream, size + (size & 1))
        if not have_format or self._data_size is None:
            raise Error('fmt chunk and/or data chunk missing')
        self._nframes = self._data_size // self._framesize
        self._soundpos = 0
        self._consumed = 0          # bytes of the data chunk read so far
        self._need_seek = False

    def _parse_format(self, body):
        if len(body) < 14:
            raise EOFError
        (tag, self._nchannels, self._framerate, _byte_rate,
         _block_align) = struct.unpack_from('<HHLLH', body)
        if tag not in (WAVE_FORMAT_PCM, WAVE_FORMAT_IEEE_FLOAT,
                       WAVE_FORMAT_EXTENSIBLE):
            raise Error('unknown format: %r' % (tag,))
        if len(body) < 16:
            raise EOFError
        bits = struct.unpack_from('<H', body, 14)[0]
        if tag == WAVE_FORMAT_EXTENSIBLE:
            if len(body) < 40:
                raise EOFError
            subformat = body[24:40]
            if subformat not in (KSDATAFORMAT_SUBTYPE_PCM,
                                 _KSDATAFORMAT_SUBTYPE_IEEE_FLOAT):
                try:
                    import uuid
                    shown = str(uuid.UUID(bytes_le=subformat))
                except Exception:
                    shown = subformat.hex()
                raise Error('unknown extended format: %s' % shown)
        self._format = tag
        self._sampwidth = (bits + 7) // 8
        if self._nchannels == 0:
            raise Error('bad # of channels')
        if self._sampwidth == 0:
            raise Error('bad sample width')
        self._framesize = self._nchannels * self._sampwidth
        self._comptype = 'NONE'
        self._compname = 'not compressed'

    # ---- accessors

    def getfp(self):
        return self._file

    def rewind(self):
        self._soundpos = 0
        self._need_seek = True

    def close(self):
        self._file = None
        opened = self._i_opened_the_file
        if opened:
            self._i_opened_the_file = None
            opened.close()

    def tell(self):
        return self._soundpos

    def getnchannels(self):
        return self._nchannels

    def getnframes(self):
        return self._nframes

    def getsampwidth(self):
        return self._sampwidth

    def getframerate(self):
        return self._framerate

    def getcomptype(self):
        return self._comptype

    def getcompname(self):
        return self._compname

    def getformat(self):
        return self._format

    def getparams(self):
        return _wave_params(self._nchannels, self._sampwidth, self._framerate,
                            self._nframes, self._comptype, self._compname)

    def getmarkers(self):
        return None

    def getmark(self, id):
        raise Error('no marks')

    def setpos(self, pos):
        if pos < 0 or pos > self._nframes:
            raise Error('position not in range')
        self._soundpos = pos
        self._need_seek = True

    def readframes(self, nframes):
        if self._need_seek:
            offset = self._soundpos * self._framesize
            if self._data_start is None:
                raise Error('cannot seek in an unseekable file')
            self._file.seek(self._data_start + offset)
            self._consumed = offset
            self._need_seek = False
        if nframes == 0:
            return b''
        wanted = self._data_size - self._consumed
        if nframes > 0:
            # a negative count reads the rest of the data
            wanted = min(nframes * self._framesize, wanted)
        data = _read_exact(self._file, wanted) if wanted > 0 else b''
        self._consumed += len(data)
        usable = len(data) - len(data) % self._framesize
        if usable != len(data):
            data = data[:usable]
        self._soundpos += usable // self._framesize
        return _to_native(data, self._sampwidth)


class Wave_write:
    """Writer for a WAV file (a file name, path-like or binary file object).

    Set the channel count, sample width and frame rate (and optionally the
    format and the number of frames) before writing frames; the header is
    completed when the file is closed."""

    def __init__(self, f):
        self._file = None
        self._i_opened_the_file = None
        if isinstance(f, (str, bytes)) or hasattr(f, '__fspath__'):
            f = builtins.open(f, 'wb')
            self._i_opened_the_file = f
        self._file = f
        self._nchannels = 0
        self._sampwidth = 0
        self._framerate = 0
        self._nframes = 0
        self._nframeswritten = 0
        self._datawritten = 0
        self._datalength = 0
        self._format = WAVE_FORMAT_PCM
        self._comptype = 'NONE'
        self._compname = 'not compressed'
        self._header_written = False
        self._size_positions = None

    def __del__(self):
        self.close()

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()

    def _check_not_started(self):
        if self._datawritten:
            raise Error('cannot change parameters after starting to write')

    # ---- parameters

    def setnchannels(self, nchannels):
        self._check_not_started()
        if nchannels < 1:
            raise Error('bad # of channels')
        self._nchannels = nchannels

    def getnchannels(self):
        if not self._nchannels:
            raise Error('number of channels not set')
        return self._nchannels

    def setsampwidth(self, sampwidth):
        self._check_not_started()
        if self._format == WAVE_FORMAT_IEEE_FLOAT:
            if sampwidth not in (4, 8):
                raise Error('unsupported sample width for IEEE float format')
        elif sampwidth < 1 or sampwidth > 4:
            raise Error('bad sample width')
        self._sampwidth = sampwidth

    def getsampwidth(self):
        if not self._sampwidth:
            raise Error('sample width not set')
        return self._sampwidth

    def setframerate(self, framerate):
        self._check_not_started()
        framerate = int(round(framerate))
        if framerate <= 0:
            raise Error('bad frame rate')
        self._framerate = framerate

    def getframerate(self):
        if not self._framerate:
            raise Error('frame rate not set')
        return self._framerate

    def setnframes(self, nframes):
        self._check_not_started()
        self._nframes = nframes

    def getnframes(self):
        return self._nframeswritten

    def setcomptype(self, comptype, compname):
        self._check_not_started()
        if comptype != 'NONE':
            raise Error('unsupported compression type')
        self._comptype = comptype
        self._compname = compname

    def getcomptype(self):
        return self._comptype

    def getcompname(self):
        return self._compname

    def setformat(self, format):
        self._check_not_started()
        if format == WAVE_FORMAT_IEEE_FLOAT:
            if self._sampwidth and self._sampwidth not in (4, 8):
                raise Error('unsupported sample width for IEEE float format')
        elif format != WAVE_FORMAT_PCM:
            raise Error('unsupported wave format')
        self._format = format

    def getformat(self):
        return self._format

    def setparams(self, params):
        if len(params) == 7:
            nchannels, sampwidth, framerate, nframes, comptype, compname, format = params
        else:
            nchannels, sampwidth, framerate, nframes, comptype, compname = params
            format = WAVE_FORMAT_PCM
        self._check_not_started()
        # the format decides which sample widths are valid
        if format not in (WAVE_FORMAT_PCM, WAVE_FORMAT_IEEE_FLOAT):
            raise Error('unsupported wave format')
        self._format = format
        self.setnchannels(nchannels)
        self.setsampwidth(sampwidth)
        self.setframerate(framerate)
        self.setnframes(nframes)
        self.setcomptype(comptype, compname)

    def getparams(self):
        if not self._nchannels or not self._sampwidth or not self._framerate:
            raise Error('not all parameters set')
        return _wave_params(self._nchannels, self._sampwidth, self._framerate,
                            self._nframes, self._comptype, self._compname)

    def setmark(self, id, pos, name):
        raise Error('setmark() not supported')

    def getmark(self, id):
        raise Error('no marks')

    def getmarkers(self):
        return None

    def tell(self):
        return self._nframeswritten

    # ---- writing

    def writeframesraw(self, data):
        if not isinstance(data, (bytes, bytearray)):
            data = memoryview(data).cast('B')
        self._write_header_once(len(data))
        framesize = self._sampwidth * self._nchannels
        data = _to_native(bytes(data), self._sampwidth)
        self._file.write(data)
        self._datawritten += len(data)
        self._nframeswritten += len(data) // framesize

    def writeframes(self, data):
        self.writeframesraw(data)
        if self._datalength != self._datawritten:
            self._patch_header()

    def close(self):
        try:
            if self._file:
                self._write_header_once(0)
                if self._datawritten & 1:
                    self._file.write(b'\x00')
                if self._datalength != self._datawritten:
                    self._patch_header()
                self._file.flush()
        finally:
            self._file = None
            opened = self._i_opened_the_file
            if opened:
                self._i_opened_the_file = None
                opened.close()

    # ---- header

    def _format_chunk(self):
        if self._format == WAVE_FORMAT_IEEE_FLOAT:
            # non-PCM formats carry a (here empty) extension size field
            body = struct.pack('<HHLLHHH', self._format, self._nchannels,
                               self._framerate,
                               self._nchannels * self._framerate * self._sampwidth,
                               self._nchannels * self._sampwidth,
                               self._sampwidth * 8, 0)
        else:
            body = struct.pack('<HHLLHH', self._format, self._nchannels,
                               self._framerate,
                               self._nchannels * self._framerate * self._sampwidth,
                               self._nchannels * self._sampwidth,
                               self._sampwidth * 8)
        return b'fmt ' + struct.pack('<L', len(body)) + body

    def _riff_size(self, datalength):
        size = 4 + len(self._format_chunk()) + 8 + datalength + (datalength & 1)
        if self._format == WAVE_FORMAT_IEEE_FLOAT:
            size += 12
        return size

    def _write_header_once(self, datasize):
        if self._header_written:
            return
        if not self._nchannels:
            raise Error('# channels not specified')
        if not self._sampwidth:
            raise Error('sample width not specified')
        if not self._framerate:
            raise Error('sampling rate not specified')
        framesize = self._nchannels * self._sampwidth
        if not self._nframes:
            self._nframes = datasize // framesize
        self._datalength = self._nframes * framesize
        stream = self._file
        start = _tell(stream)
        stream.write(b'RIFF')
        stream.write(struct.pack('<L', self._riff_size(self._datalength)))
        stream.write(b'WAVE')
        stream.write(self._format_chunk())
        fact_pos = None
        if self._format == WAVE_FORMAT_IEEE_FLOAT:
            stream.write(b'fact')
            stream.write(struct.pack('<L', 4))
            if start is not None:
                fact_pos = _tell(stream)
            stream.write(struct.pack('<L', self._nframes))
        stream.write(b'data')
        data_pos = _tell(stream) if start is not None else None
        stream.write(struct.pack('<L', self._datalength))
        if start is not None and data_pos is not None:
            self._size_positions = (start + 4, fact_pos, data_pos)
        self._header_written = True

    def _patch_header(self):
        if not self._header_written:
            return
        if self._size_positions is None:
            # cannot go back in an unseekable stream
            raise OSError('cannot update the header of an unseekable file')
        stream = self._file
        riff_pos, fact_pos, data_pos = self._size_positions
        here = stream.tell()
        stream.seek(riff_pos, 0)
        stream.write(struct.pack('<L', self._riff_size(self._datawritten)))
        if fact_pos is not None:
            stream.seek(fact_pos, 0)
            framesize = self._nchannels * self._sampwidth
            stream.write(struct.pack('<L', self._datawritten // framesize))
        stream.seek(data_pos, 0)
        stream.write(struct.pack('<L', self._datawritten))
        stream.seek(here, 0)
        self._datalength = self._datawritten


def open(f, mode=None):
    """Open a WAV file for reading ('r', 'rb') or writing ('w', 'wb').
    Without `mode`, the mode of a file object is used (default 'rb')."""
    if mode is None:
        mode = getattr(f, 'mode', 'rb')
    if mode in ('r', 'rb'):
        return Wave_read(f)
    if mode in ('w', 'wb'):
        return Wave_write(f)
    raise Error("mode must be 'r', 'rb', 'w', or 'wb'")
