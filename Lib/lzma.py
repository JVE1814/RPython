"""LZMA / XZ compression: LZMAFile, open(), and one-shot compress()/decompress().

The incremental LZMACompressor and LZMADecompressor classes, the format/check/filter
constants and LZMAError come from the _lzma accelerator module.
"""

__all__ = [
    "CHECK_NONE", "CHECK_CRC32", "CHECK_CRC64", "CHECK_SHA256",
    "CHECK_ID_MAX", "CHECK_UNKNOWN",
    "FILTER_LZMA1", "FILTER_LZMA2", "FILTER_DELTA", "FILTER_X86", "FILTER_IA64",
    "FILTER_ARM", "FILTER_ARMTHUMB", "FILTER_POWERPC", "FILTER_SPARC",
    "FORMAT_AUTO", "FORMAT_XZ", "FORMAT_ALONE", "FORMAT_RAW",
    "MF_HC3", "MF_HC4", "MF_BT2", "MF_BT3", "MF_BT4",
    "MODE_FAST", "MODE_NORMAL", "PRESET_DEFAULT", "PRESET_EXTREME",

    "LZMACompressor", "LZMADecompressor", "LZMAFile", "LZMAError",
    "open", "compress", "decompress", "is_check_supported",
]

import builtins
import io
import os
from compression._common import _streams

from _lzma import *
from _lzma import _encode_filter_properties, _decode_filter_properties  # noqa: F401

_CLOSED, _READING, _WRITING = 0, 1, 3


class LZMAFile(_streams.BaseStream):
    """File object reading or writing LZMA-compressed data (.xz by default).

    *filename* is a path (str, bytes or PathLike) or an existing binary file
    object.  *mode* is "r" (default), "w", "x" or "a", optionally with "b".
    Reading accepts several concatenated streams.  *format*, *check*,
    *preset* and *filters* have the meaning they have for LZMACompressor
    (when writing) and LZMADecompressor (when reading).
    """

    def __init__(self, filename=None, mode="r", *,
                 format=None, check=-1, preset=None, filters=None):
        self._fp = None
        self._closefp = False
        self._mode = _CLOSED

        if mode in ("r", "rb"):
            if check != -1:
                raise ValueError("Cannot specify an integrity check "
                                 "when opening a file for reading")
            if preset is not None:
                raise ValueError("Cannot specify a preset compression "
                                 "level when opening a file for reading")
            if format is None:
                format = FORMAT_AUTO
            mode_code = _READING
        elif mode in ("w", "wb", "a", "ab", "x", "xb"):
            if format is None:
                format = FORMAT_XZ
            mode_code = _WRITING
            self._compressor = LZMACompressor(format=format, check=check,
                                              preset=preset, filters=filters)
            self._pos = 0
        else:
            raise ValueError("Invalid mode: {!r}".format(mode))

        if isinstance(filename, (str, bytes, os.PathLike)):
            if "b" not in mode:
                mode += "b"
            self._fp = builtins.open(filename, mode)
            self._closefp = True
        elif hasattr(filename, "read") or hasattr(filename, "write"):
            self._fp = filename
        else:
            raise TypeError("filename must be a str, bytes, file or PathLike object")
        self._mode = mode_code

        if self._mode == _READING:
            raw = _streams.DecompressReader(self._fp, LZMADecompressor,
                                            trailing_error=LZMAError,
                                            format=format, filters=filters)
            self._buffer = io.BufferedReader(raw)

    def close(self):
        """Finish writing (if writing) and close; the underlying file is only
        closed when this object opened it."""
        if self._mode == _CLOSED:
            return
        try:
            if self._mode == _READING:
                self._buffer.close()
                self._buffer = None
            elif self._mode == _WRITING:
                self._fp.write(self._compressor.flush())
                self._compressor = None
        finally:
            try:
                if self._closefp:
                    self._fp.close()
            finally:
                self._fp = None
                self._closefp = False
                self._mode = _CLOSED

    @property
    def closed(self):
        return self._mode == _CLOSED

    @property
    def name(self):
        self._check_not_closed()
        return self._fp.name

    @property
    def mode(self):
        return "wb" if self._mode == _WRITING else "rb"

    def fileno(self):
        self._check_not_closed()
        return self._fp.fileno()

    def seekable(self):
        return self.readable() and self._buffer.seekable()

    def readable(self):
        self._check_not_closed()
        return self._mode == _READING

    def writable(self):
        self._check_not_closed()
        return self._mode == _WRITING

    def peek(self, size=-1):
        """Return buffered decompressed data without consuming it."""
        self._check_can_read()
        return self._buffer.peek(size)

    def read(self, size=-1):
        """Read up to *size* decompressed bytes (everything if negative)."""
        self._check_can_read()
        return self._buffer.read(size)

    def read1(self, size=-1):
        """Read up to *size* bytes with at most one read of the raw stream."""
        self._check_can_read()
        if size < 0:
            size = io.DEFAULT_BUFFER_SIZE
        return self._buffer.read1(size)

    def readline(self, size=-1):
        self._check_can_read()
        return self._buffer.readline(size)

    def write(self, data):
        """Compress *data*; returns the number of uncompressed bytes taken."""
        self._check_can_write()
        if isinstance(data, (bytes, bytearray)):
            length = len(data)
        else:
            data = memoryview(data)
            length = data.nbytes
        self._fp.write(self._compressor.compress(data))
        self._pos += length
        return length

    def seek(self, offset, whence=io.SEEK_SET):
        """Move within the decompressed stream (emulated; backwards seeks
        restart decompression)."""
        self._check_can_seek()
        return self._buffer.seek(offset, whence)

    def tell(self):
        self._check_not_closed()
        if self._mode == _READING:
            return self._buffer.tell()
        return self._pos


def open(filename, mode="rb", *,
         format=None, check=-1, preset=None, filters=None,
         encoding=None, errors=None, newline=None):
    """Open an LZMA-compressed file in binary or text ("t" in mode) mode."""
    if "t" in mode:
        if "b" in mode:
            raise ValueError("Invalid mode: %r" % (mode,))
    else:
        for arg, nm in ((encoding, "encoding"), (errors, "errors"),
                        (newline, "newline")):
            if arg is not None:
                raise ValueError("Argument '%s' not supported in binary mode" % nm)

    raw_mode = mode.replace("t", "")
    binary = LZMAFile(filename, raw_mode, format=format, check=check,
                      preset=preset, filters=filters)
    if "t" in mode:
        encoding = io.text_encoding(encoding)
        return io.TextIOWrapper(binary, encoding, errors, newline)
    return binary


def compress(data, format=FORMAT_XZ, check=-1, preset=None, filters=None):
    """Compress *data* in one call (see LZMACompressor for the options)."""
    c = LZMACompressor(format, check, preset, filters)
    return c.compress(data) + c.flush()


def decompress(data, format=FORMAT_AUTO, memlimit=None, filters=None):
    """Decompress *data*, which may hold several concatenated streams;
    junk after at least one complete stream is ignored."""
    pieces = []
    while True:
        d = LZMADecompressor(format, memlimit, filters)
        try:
            pieces.append(d.decompress(data))
        except LZMAError:
            if pieces:
                break
            raise
        if not d.eof:
            raise LZMAError("Compressed data ended before the "
                            "end-of-stream marker was reached")
        data = d.unused_data
        if not data:
            break
    return b"".join(pieces)
