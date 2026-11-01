"""Building blocks shared by the gzip, bz2, lzma and zstd file objects."""

import io
import sys

# Amount of compressed input fetched from the underlying file per refill.
BUFFER_SIZE = io.DEFAULT_BUFFER_SIZE


class BaseStream(io.BufferedIOBase):
    """Common state checks for the compressed-file classes."""

    def _check_not_closed(self):
        if self.closed:
            raise ValueError("I/O operation on closed file")

    def _check_can_read(self):
        if not self.readable():
            raise io.UnsupportedOperation("File not open for reading")

    def _check_can_write(self):
        if not self.writable():
            raise io.UnsupportedOperation("File not open for writing")

    def _check_can_seek(self):
        if not self.readable():
            raise io.UnsupportedOperation("Seeking is only supported "
                                          "on files open for reading")
        if not self.seekable():
            raise io.UnsupportedOperation("The underlying file object "
                                          "does not support seeking")


class DecompressReader(io.RawIOBase):
    """Raw, read-only stream decompressing data pulled from *fp*.

    A new decompressor (decomp_factory(**decomp_args)) is created for every
    concatenated stream ("member"); data following the last member that
    makes the decompressor raise one of *trailing_error* is ignored.
    Seeking is emulated by re-reading from the start when going backwards.
    """

    def readable(self):
        return True

    def __init__(self, fp, decomp_factory, trailing_error=(), **decomp_args):
        self._fp = fp
        self._eof = False
        self._pos = 0          # position in the decompressed stream
        self._size = -1        # total decompressed size once known
        self._decomp_factory = decomp_factory
        self._decomp_args = decomp_args
        self._decompressor = decomp_factory(**decomp_args)
        self._trailing_error = trailing_error

    def close(self):
        self._decompressor = None
        return super().close()

    def seekable(self):
        return self._fp.seekable()

    def readinto(self, b):
        with memoryview(b) as view, view.cast("B") as flat:
            data = self.read(len(flat))
            flat[:len(data)] = data
        return len(data)

    def read(self, size=-1):
        if size < 0:
            return self.readall()
        if not size or self._eof:
            return b""
        result = None
        while True:
            dec = self._decompressor
            if dec.eof:
                pending = dec.unused_data or self._fp.read(BUFFER_SIZE)
                if not pending:
                    break
                # Another stream follows: start a fresh decompressor.
                self._decompressor = self._decomp_factory(**self._decomp_args)
                try:
                    result = self._decompressor.decompress(pending, size)
                except self._trailing_error:
                    break
            elif dec.needs_input:
                chunk = self._fp.read(BUFFER_SIZE)
                if not chunk:
                    raise EOFError("Compressed file ended before the "
                                   "end-of-stream marker was reached")
                result = dec.decompress(chunk, size)
            else:
                result = dec.decompress(b"", size)
            if result:
                break
        if not result:
            self._eof = True
            self._size = self._pos
            return b""
        self._pos += len(result)
        return result

    def readall(self):
        parts = []
        while True:
            piece = self.read(sys.maxsize)
            if not piece:
                break
            parts.append(piece)
        return b"".join(parts)

    def _rewind(self):
        self._fp.seek(0)
        self._eof = False
        self._pos = 0
        self._decompressor = self._decomp_factory(**self._decomp_args)

    def seek(self, offset, whence=io.SEEK_SET):
        if whence == io.SEEK_SET:
            target = offset
        elif whence == io.SEEK_CUR:
            target = self._pos + offset
        elif whence == io.SEEK_END:
            if self._size < 0:
                while self.read(io.DEFAULT_BUFFER_SIZE):
                    pass
            target = self._size + offset
        else:
            raise ValueError(f"Invalid value for whence: {whence}")

        if target < self._pos:
            self._rewind()
        else:
            target -= self._pos

        while target > 0:
            got = self.read(min(io.DEFAULT_BUFFER_SIZE, target))
            if not got:
                break
            target -= len(got)
        return self._pos

    def tell(self):
        """Position in the decompressed data."""
        return self._pos
