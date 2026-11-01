"""ZstdFile: a binary file object reading or writing Zstandard data, and open()."""

import io
from os import PathLike, fspath
from _zstd import ZstdCompressor, ZstdDecompressor, ZstdError
from compression._common import _streams

__all__ = ('ZstdFile', 'open')

_CLOSED, _READING, _WRITING = 0, 1, 2


def _nbytes(data):
    if isinstance(data, (bytes, bytearray)):
        return len(data)
    with memoryview(data) as view:
        return view.nbytes


class ZstdFile(_streams.BaseStream):
    """Binary file object giving transparent access to Zstandard data.

    *file* is a path (str, bytes or PathLike) or a file object.  *mode* is
    "r" (default), "w", "x" or "a", with an optional "b".  When writing,
    *level* or *options* configure the compressor; when reading, *options*
    configures the decompressor.  *zstd_dict* is an optional dictionary.
    Reading accepts several concatenated frames.
    """

    FLUSH_BLOCK = ZstdCompressor.FLUSH_BLOCK
    FLUSH_FRAME = ZstdCompressor.FLUSH_FRAME

    def __init__(self, file, /, mode='r', *,
                 level=None, options=None, zstd_dict=None):
        self._fp = None
        self._close_fp = False
        self._mode = _CLOSED
        self._buffer = None

        if not isinstance(mode, str):
            raise ValueError(f'invalid mode: {mode!r}')
        if mode in ('r', 'rb'):
            if level is not None:
                raise TypeError('level is illegal in read mode')
            # Validate the arguments before touching the file.
            ZstdDecompressor(zstd_dict=zstd_dict, options=options)
            self._mode = _READING
        elif mode in ('w', 'wb', 'a', 'ab', 'x', 'xb'):
            self._compressor = ZstdCompressor(level=level, options=options,
                                              zstd_dict=zstd_dict)
            self._pos = 0
            self._mode = _WRITING
        else:
            raise ValueError(f'invalid mode: {mode!r}')

        raw_mode = mode[0] + 'b'
        self._mode_str = raw_mode
        if isinstance(file, (str, bytes, PathLike)):
            self._fp = io.open(fspath(file), raw_mode)
            self._close_fp = True
        elif ((self._mode == _READING and hasattr(file, 'read'))
              or (self._mode == _WRITING and hasattr(file, 'write'))):
            self._fp = file
        else:
            self._mode = _CLOSED
            raise TypeError('file must be a file-like object '
                            'or a str, bytes, or PathLike object')

        if self._mode == _READING:
            raw = _streams.DecompressReader(self._fp, ZstdDecompressor,
                                            trailing_error=(),
                                            zstd_dict=zstd_dict,
                                            options=options)
            self._buffer = io.BufferedReader(raw)

    def close(self):
        """Flush and close.  An underlying file object passed in by the
        caller is left open."""
        if self._fp is None:
            return
        try:
            if self._mode == _READING:
                if getattr(self, '_buffer', None):
                    self._buffer.close()
                    self._buffer = None
            elif self._mode == _WRITING:
                self.flush(self.FLUSH_FRAME)
                self._compressor = None
        finally:
            self._mode = _CLOSED
            try:
                if self._close_fp:
                    self._fp.close()
            finally:
                self._fp = None
                self._close_fp = False

    def write(self, data, /):
        """Compress *data*; returns the number of uncompressed bytes taken.

        Output may stay buffered inside the compressor until flush() or
        close()."""
        self._check_can_write()
        length = _nbytes(data)
        out = self._compressor.compress(data)
        self._fp.write(out)
        self._pos += length
        return length

    def flush(self, mode=FLUSH_BLOCK):
        """Write out buffered data.  FLUSH_BLOCK (default) ends the current
        block, FLUSH_FRAME ends the frame.  Does nothing in read mode."""
        if self._mode == _READING:
            return
        self._check_not_closed()
        if mode not in (self.FLUSH_BLOCK, self.FLUSH_FRAME):
            raise ValueError('Invalid mode argument, expected either '
                             'ZstdFile.FLUSH_FRAME or ZstdFile.FLUSH_BLOCK')
        if self._compressor.last_mode == mode:
            return
        out = self._compressor.flush(mode)
        self._fp.write(out)
        if hasattr(self._fp, 'flush'):
            self._fp.flush()

    def read(self, size=-1):
        """Read up to *size* decompressed bytes (all of them if negative)."""
        if size is None:
            size = -1
        self._check_can_read()
        return self._buffer.read(size)

    def read1(self, size=-1):
        """Read up to *size* bytes, doing at most one read of the raw file."""
        self._check_can_read()
        if size < 0:
            size = io.DEFAULT_BUFFER_SIZE
        return self._buffer.read1(size)

    def readinto(self, b):
        self._check_can_read()
        return self._buffer.readinto(b)

    def readinto1(self, b):
        self._check_can_read()
        return self._buffer.readinto1(b)

    def readline(self, size=-1):
        self._check_can_read()
        return self._buffer.readline(size)

    def seek(self, offset, whence=io.SEEK_SET):
        """Move within the decompressed data (emulated; moving backwards
        restarts decompression).  Only in read mode."""
        self._check_can_read()
        return self._buffer.seek(offset, whence)

    def peek(self, size=-1):
        """Return buffered data without advancing the position."""
        self._check_can_read()
        return self._buffer.peek(size)

    def __next__(self):
        line = self.readline()
        if not line:
            raise StopIteration
        return line

    def tell(self):
        self._check_not_closed()
        if self._mode == _READING:
            return self._buffer.tell()
        return self._pos

    def fileno(self):
        self._check_not_closed()
        return self._fp.fileno()

    @property
    def name(self):
        self._check_not_closed()
        return self._fp.name

    @property
    def mode(self):
        return 'wb' if self._mode_str[0] in 'wax' else 'rb'

    @property
    def closed(self):
        return self._mode == _CLOSED

    def seekable(self):
        return self.readable() and self._buffer.seekable()

    def readable(self):
        self._check_not_closed()
        return self._mode == _READING

    def writable(self):
        self._check_not_closed()
        return self._mode == _WRITING


def open(file, /, mode='rb', *, level=None, options=None, zstd_dict=None,
         encoding=None, errors=None, newline=None):
    """Open a Zstandard file in binary or text ("t" in *mode*) mode and
    return a ZstdFile or an io.TextIOWrapper around one."""
    text = 't' in mode
    if text:
        if 'b' in mode:
            raise ValueError(f'Invalid mode: {mode!r}')
    else:
        for name, value in (('encoding', encoding), ('errors', errors),
                            ('newline', newline)):
            if value is not None:
                raise ValueError(f"Argument '{name}' not supported in binary mode")
    binary = ZstdFile(file, mode.replace('t', ''), level=level,
                      options=options, zstd_dict=zstd_dict)
    if text:
        return io.TextIOWrapper(binary, encoding, errors, newline)
    return binary
