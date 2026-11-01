"""bzip2 compression: BZ2File, open(), and one-shot compress()/decompress().

BZ2Compressor and BZ2Decompressor (from _bz2) provide incremental access.
"""

__all__ = ["BZ2File", "BZ2Compressor", "BZ2Decompressor",
           "open", "compress", "decompress"]

from builtins import open as _builtin_open
import io
import os
from compression._common import _streams

from _bz2 import BZ2Compressor, BZ2Decompressor

_MODE_CLOSED = 0
_MODE_READ = 1
_MODE_WRITE = 3


class BZ2File(_streams.BaseStream):
    """A file object giving transparent access to bzip2-compressed data.

    mode: 'r' (default), 'w', 'x' or 'a' (binary only).  In write modes
    compresslevel (1-9, default 9) selects the block size.  Reading handles
    multiple concatenated streams.
    """

    def __init__(self, filename, mode="r", *, compresslevel=9):
        self._fp = None
        self._closefp = False
        self._mode = _MODE_CLOSED

        if not (1 <= compresslevel <= 9):
            raise ValueError("compresslevel must be between 1 and 9")

        if mode in ("", "r", "rb"):
            mode = "rb"
            mode_code = _MODE_READ
        elif mode in ("w", "wb", "x", "xb", "a", "ab"):
            mode = mode[0] + "b"
            mode_code = _MODE_WRITE
            self._compressor = BZ2Compressor(compresslevel)
        else:
            raise ValueError("Invalid mode: %r" % (mode,))

        self._mode_str = mode
        if isinstance(filename, (str, bytes, os.PathLike)):
            self._fp = _builtin_open(os.fspath(filename), mode)
            self._closefp = True
            self._mode = mode_code
        elif hasattr(filename, "read") or hasattr(filename, "write"):
            self._fp = filename
            self._mode = mode_code
        else:
            raise TypeError("filename must be a str, bytes, file or PathLike object")

        if self._mode == _MODE_READ:
            raw = _streams.DecompressReader(self._fp, BZ2Decompressor,
                                            trailing_error=OSError)
            self._buffer = io.BufferedReader(raw)
        else:
            self._pos = 0

    def close(self):
        """Flush and close; the underlying file is closed only if this
        object opened it."""
        if self.closed:
            return
        try:
            if self._mode == _MODE_READ:
                self._buffer.close()
            elif self._mode == _MODE_WRITE:
                self._fp.write(self._compressor.flush())
                self._compressor = None
        finally:
            try:
                if self._closefp:
                    self._fp.close()
            finally:
                self._fp = None
                self._closefp = False
                self._buffer = None
                self._mode = _MODE_CLOSED

    @property
    def closed(self):
        return self._mode == _MODE_CLOSED

    @property
    def name(self):
        self._check_not_closed()
        return self._fp.name

    @property
    def mode(self):
        return 'wb' if self._mode_str[0] in 'wxa' else 'rb'

    def fileno(self):
        self._check_not_closed()
        return self._fp.fileno()

    def seekable(self):
        return self.readable() and self._buffer.seekable()

    def readable(self):
        self._check_not_closed()
        return self._mode == _MODE_READ

    def writable(self):
        self._check_not_closed()
        return self._mode == _MODE_WRITE

    def peek(self, n=0):
        """Return buffered data without advancing (at least one byte
        unless at EOF)."""
        self._check_can_read()
        return self._buffer.peek(n)

    def read(self, size=-1):
        self._check_can_read()
        return self._buffer.read(size)

    def read1(self, size=-1):
        self._check_can_read()
        if size < 0:
            size = io.DEFAULT_BUFFER_SIZE
        return self._buffer.read1(size)

    def readinto(self, b):
        self._check_can_read()
        return self._buffer.readinto(b)

    def readline(self, size=-1):
        if not isinstance(size, int):
            if not hasattr(size, "__index__"):
                raise TypeError("Integer argument expected")
            size = size.__index__()
        self._check_can_read()
        return self._buffer.readline(size)

    def readlines(self, size=-1):
        if not isinstance(size, int):
            if not hasattr(size, "__index__"):
                raise TypeError("Integer argument expected")
            size = size.__index__()
        self._check_can_read()
        return self._buffer.readlines(size)

    def write(self, data):
        """Compress and write data; returns the number of bytes consumed."""
        self._check_can_write()
        if isinstance(data, (bytes, bytearray)):
            length = len(data)
        else:
            data = memoryview(data)
            length = data.nbytes
        self._fp.write(self._compressor.compress(data))
        self._pos += length
        return length

    def writelines(self, seq):
        return _streams.BaseStream.writelines(self, seq)

    def seek(self, offset, whence=io.SEEK_SET):
        """Seek in the decompressed data (emulated, may be slow)."""
        self._check_can_seek()
        return self._buffer.seek(offset, whence)

    def tell(self):
        self._check_not_closed()
        if self._mode == _MODE_READ:
            return self._buffer.tell()
        return self._pos


def open(filename, mode="rb", compresslevel=9,
         encoding=None, errors=None, newline=None):
    """Open a bzip2 file in binary or text ('t' in mode) mode."""
    if "t" in mode:
        if "b" in mode:
            raise ValueError("Invalid mode: %r" % (mode,))
    else:
        if encoding is not None:
            raise ValueError("Argument 'encoding' not supported in binary mode")
        if errors is not None:
            raise ValueError("Argument 'errors' not supported in binary mode")
        if newline is not None:
            raise ValueError("Argument 'newline' not supported in binary mode")

    binary_file = BZ2File(filename, mode.replace("t", ""),
                          compresslevel=compresslevel)
    if "t" in mode:
        encoding = io.text_encoding(encoding)
        return io.TextIOWrapper(binary_file, encoding, errors, newline)
    return binary_file


def compress(data, compresslevel=9):
    """Compress a block of data in one call."""
    comp = BZ2Compressor(compresslevel)
    return comp.compress(data) + comp.flush()


def decompress(data):
    """Decompress data, which may consist of several concatenated streams."""
    parts = []
    while data:
        dec = BZ2Decompressor()
        try:
            parts.append(dec.decompress(data))
        except OSError:
            if parts:
                break   # trailing garbage after a complete stream
            raise
        if not dec.eof:
            raise ValueError("Compressed data ended before the "
                             "end-of-stream marker was reached")
        data = dec.unused_data
    return b"".join(parts)
