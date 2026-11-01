"""Reading and writing gzip files (RFC 1952).

open() and GzipFile give file objects; compress() and decompress() work on
bytes in memory.  Multi-member files are read transparently.
"""

import builtins
import io
import os
import struct
import time
import weakref
import zlib
from compression._common import _streams

__all__ = ["BadGzipFile", "GzipFile", "open", "compress", "decompress"]

# Header flag bits.
FTEXT, FHCRC, FEXTRA, FNAME, FCOMMENT = 1, 2, 4, 8, 16

READ = 'rb'
WRITE = 'wb'

_COMPRESS_LEVEL_FAST = 1
_COMPRESS_LEVEL_TRADEOFF = 6
_COMPRESS_LEVEL_BEST = 9

READ_BUFFER_SIZE = 128 * 1024
_WRITE_BUFFER_SIZE = 4 * io.DEFAULT_BUFFER_SIZE

_GZIP_MAGIC = b'\x1f\x8b'
_OS_UNKNOWN = 255


class BadGzipFile(OSError):
    """Exception raised in some cases for invalid gzip files."""


def open(filename, mode="rb", compresslevel=_COMPRESS_LEVEL_BEST,
         encoding=None, errors=None, newline=None):
    """Open a gzip file in binary ("rb", "wb", "ab", "xb") or text mode
    ("rt", "wt", "at", "xt").

    filename may be a path or an existing binary file object.  Text mode
    wraps the result in io.TextIOWrapper with the given encoding, errors
    and newline settings (which are rejected in binary mode).
    """
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

    binary_mode = mode.replace("t", "")
    if isinstance(filename, (str, bytes, os.PathLike)):
        binary_file = GzipFile(filename, binary_mode, compresslevel)
    elif hasattr(filename, "read") or hasattr(filename, "write"):
        binary_file = GzipFile(None, binary_mode, compresslevel, filename)
    else:
        raise TypeError("filename must be a str or bytes object, or a file")

    if "t" in mode:
        encoding = io.text_encoding(encoding)
        return io.TextIOWrapper(binary_file, encoding, errors, newline)
    return binary_file


def write32u(output, value):
    output.write(struct.pack("<L", value))


class _PaddedFile:
    """File wrapper that can push bytes back in front of the stream; used to
    return data read past the end of a member."""

    def __init__(self, f, prepend=b''):
        self.file = f
        self._buffer = prepend
        self._pos = 0

    def read(self, size):
        avail = len(self._buffer) - self._pos
        if avail <= 0:
            return self.file.read(size)
        if size <= avail:
            start = self._pos
            self._pos += size
            return self._buffer[start:self._pos]
        head = self._buffer[self._pos:]
        self._buffer = b''
        self._pos = 0
        return head + self.file.read(size - avail)

    def prepend(self, prepend=b''):
        self._buffer = bytes(prepend) + self._buffer[self._pos:]
        self._pos = 0

    def seek(self, off):
        self._buffer = b''
        self._pos = 0
        return self.file.seek(off)

    def seekable(self):
        return True  # emulated by rewinding


class _WriteBufferStream(io.RawIOBase):
    """Raw sink that forwards buffered writes to GzipFile._write_raw."""

    def __init__(self, gzip_file):
        self.gzip_file = weakref.ref(gzip_file)

    def write(self, data):
        gz = self.gzip_file()
        if gz is None:
            raise RuntimeError("lost gzip_file")
        return gz._write_raw(data)

    def seekable(self):
        return False

    def writable(self):
        return True


class GzipFile(_streams.BaseStream):
    """A file object reading or writing gzip-compressed data.

    It behaves like a regular binary file except that seeking is limited:
    in read mode it is emulated (and may be slow), in write mode only
    forward seeks are possible (zeros are written).
    """

    # Old attribute names kept for compatibility.
    myfileobj = None
    fileobj = None

    def __init__(self, filename=None, mode=None,
                 compresslevel=_COMPRESS_LEVEL_BEST, fileobj=None, mtime=None):
        """filename/fileobj: path or binary file object (fileobj wins).
        mode: 'r', 'rb', 'a', 'ab', 'w', 'wb', 'x' or 'xb'; defaults to the
        file object's mode or 'rb'.  compresslevel: 0-9.  mtime: timestamp
        recorded in the header when writing (default: current time).
        """
        if mode and ('t' in mode or 'U' in mode):
            raise ValueError("Invalid mode: {!r}".format(mode))
        if mode and 'b' not in mode:
            mode += 'b'

        try:
            if fileobj is None:
                fileobj = self.myfileobj = builtins.open(filename, mode or 'rb')
            if filename is None:
                filename = getattr(fileobj, 'name', '')
                if not isinstance(filename, (str, bytes)):
                    filename = ''
            else:
                filename = os.fspath(filename)
            explicit_mode = mode
            if mode is None:
                mode = getattr(fileobj, 'mode', 'rb')

            if mode.startswith('r'):
                self.mode = READ
                raw = _GzipReader(fileobj)
                self._buffer = io.BufferedReader(raw)
                self.name = filename
            elif mode.startswith(('w', 'a', 'x')):
                if explicit_mode is None:
                    import warnings
                    warnings.warn(
                        "GzipFile was opened for writing, but this will "
                        "change in future Python releases.  "
                        "Specify the mode argument for opening it for writing.",
                        FutureWarning, 2)
                self.mode = WRITE
                self._init_write(filename)
                self.compress = zlib.compressobj(compresslevel,
                                                 zlib.DEFLATED,
                                                 -zlib.MAX_WBITS,
                                                 zlib.DEF_MEM_LEVEL,
                                                 0)
                self._write_mtime = mtime
                self._buffer_size = _WRITE_BUFFER_SIZE
                self._buffer = io.BufferedWriter(_WriteBufferStream(self),
                                                 buffer_size=self._buffer_size)
            else:
                raise ValueError("Invalid mode: {!r}".format(mode))

            self.fileobj = fileobj
            if self.mode == WRITE:
                self._write_gzip_header(compresslevel)
        except:
            # Do not leak the file we opened ourselves; a failed object
            # counts as closed.
            self.fileobj = None
            if self.myfileobj is not None:
                self.myfileobj.close()
                self.myfileobj = None
            raise

    def __del__(self):
        if getattr(self, 'mode', None) == WRITE and not self.closed:
            import warnings
            warnings.warn(f"unclosed GzipFile {self!r}", ResourceWarning,
                          source=self)
        super().__del__()

    @property
    def mtime(self):
        """Modification time of the last member read (None before reading)."""
        return self._buffer.raw._last_mtime

    def __repr__(self):
        inner = repr(self.fileobj)
        return '<gzip ' + inner[1:-1] + ' ' + hex(id(self)) + '>'

    def _init_write(self, filename):
        self.name = filename
        self.crc = zlib.crc32(b"")
        self.size = 0
        self.writebuf = []
        self.bufsize = 0
        self.offset = 0  # position in the uncompressed stream

    def _write_gzip_header(self, compresslevel):
        out = self.fileobj
        name = os.path.basename(self.name)
        if not isinstance(name, bytes):
            try:
                name = name.encode('latin-1')
            except UnicodeEncodeError:
                name = b''
        if name.endswith(b'.gz'):
            name = name[:-3]
        flags = FNAME if name else 0
        mtime = self._write_mtime
        if mtime is None:
            mtime = time.time()
        mtime = int(mtime)
        if not 0 <= mtime < 1 << 32:
            mtime = 0
        if compresslevel == _COMPRESS_LEVEL_BEST:
            xfl = 2
        elif compresslevel == _COMPRESS_LEVEL_FAST:
            xfl = 4
        else:
            xfl = 0
        out.write(_GZIP_MAGIC + struct.pack('<BBLBB', 8, flags, mtime, xfl,
                                            _OS_UNKNOWN))
        if name:
            out.write(name + b'\0')

    def write(self, data):
        self._check_not_closed()
        if self.mode != WRITE:
            import errno
            raise OSError(errno.EBADF, "write() on read-only GzipFile object")
        if self.fileobj is None:
            raise ValueError("write() on closed GzipFile object")
        return self._buffer.write(data)

    def _write_raw(self, data):
        if isinstance(data, (bytes, bytearray)):
            length = len(data)
        else:
            data = memoryview(data)
            length = data.nbytes
        if length:
            self.fileobj.write(self.compress.compress(data))
            self.size += length
            self.crc = zlib.crc32(data, self.crc)
            self.offset += length
        return length

    def _check_read(self, what):
        self._check_not_closed()
        if self.mode != READ:
            import errno
            raise OSError(errno.EBADF, f"{what}() on write-only GzipFile object")

    def read(self, size=-1):
        self._check_read("read")
        return self._buffer.read(size)

    def read1(self, size=-1):
        """Read up to size bytes with at most one read of the raw stream."""
        self._check_read("read1")
        if size < 0:
            size = io.DEFAULT_BUFFER_SIZE
        return self._buffer.read1(size)

    def readinto(self, b):
        self._check_read("readinto")
        return self._buffer.readinto(b)

    def readinto1(self, b):
        self._check_read("readinto1")
        return self._buffer.readinto1(b)

    def peek(self, n):
        self._check_read("peek")
        return self._buffer.peek(n)

    @property
    def closed(self):
        return self.fileobj is None

    def close(self):
        fileobj = self.fileobj
        if fileobj is None or self._buffer.closed:
            return
        try:
            if self.mode == WRITE:
                self._buffer.flush()
                fileobj.write(self.compress.flush())
                write32u(fileobj, self.crc)
                write32u(fileobj, self.size & 0xffffffff)
            elif self.mode == READ:
                self._buffer.close()
        finally:
            self.fileobj = None
            own = self.myfileobj
            if own is not None:
                self.myfileobj = None
                own.close()

    def flush(self, zlib_mode=zlib.Z_SYNC_FLUSH):
        self._check_not_closed()
        if self.mode == WRITE:
            self._buffer.flush()
            self.fileobj.write(self.compress.flush(zlib_mode))
            self.fileobj.flush()

    def fileno(self):
        """Descriptor of the underlying file (as returned by its fileno())."""
        return self.fileobj.fileno()

    def rewind(self):
        """Go back to the start of the decompressed data (read mode)."""
        if self.mode != READ:
            raise OSError("Can't rewind in write mode")
        self._buffer.seek(0)

    def readable(self):
        return self.mode == READ

    def writable(self):
        return self.mode == WRITE

    def seekable(self):
        return True

    def seek(self, offset, whence=io.SEEK_SET):
        if self.mode == WRITE:
            self._check_not_closed()
            self._buffer.flush()
            if whence == io.SEEK_CUR:
                offset = self.offset + offset
            elif whence != io.SEEK_SET:
                raise ValueError('Seek from end not supported')
            if offset < self.offset:
                raise OSError('Negative seek in write mode')
            gap = offset - self.offset
            zeros = b'\0' * min(gap, self._buffer_size)
            while gap > 0:
                n = min(gap, len(zeros))
                self._write_raw(zeros[:n])
                gap -= n
            return self.offset
        if self.mode == READ:
            self._check_not_closed()
            return self._buffer.seek(offset, whence)
        return self.offset

    def tell(self):
        self._check_not_closed()
        if self.mode == WRITE:
            self._buffer.flush()
            return self.offset
        return self._buffer.tell()

    def readline(self, size=-1):
        self._check_not_closed()
        return self._buffer.readline(size)


def _read_exact(fp, n):
    """Read exactly n bytes from fp or raise EOFError."""
    data = fp.read(n)
    while len(data) < n:
        more = fp.read(n - len(data))
        if not more:
            raise EOFError("Compressed file ended before the "
                           "end-of-stream marker was reached")
        data += more
    return data


def _read_gzip_header(fp):
    """Consume a member header from fp.

    Returns the member's mtime, or None if fp is at end of file.
    """
    magic = fp.read(2)
    if magic == b'':
        return None
    if magic != _GZIP_MAGIC:
        raise BadGzipFile('Not a gzipped file (%r)' % magic)

    fixed = _read_exact(fp, 8)
    method, flags, mtime = struct.unpack("<BBIxx", fixed)
    if method != 8:
        raise BadGzipFile('Unknown compression method')
    want_crc = flags & FHCRC
    seen = [magic, fixed] if want_crc else None

    def take(n):
        part = _read_exact(fp, n)
        if seen is not None:
            seen.append(part)
        return part

    if flags & FEXTRA:
        xlen, = struct.unpack("<H", take(2))
        take(xlen)
    for bit in (FNAME, FCOMMENT):
        if flags & bit:
            while take(1) != b'\0':
                pass
    if want_crc:
        expected = zlib.crc32(b"".join(seen)) & 0xFFFF
        stored, = struct.unpack("<H", _read_exact(fp, 2))
        if stored != expected:
            raise BadGzipFile(f"Corrupted gzip header. Checksums do not "
                              f"match: {expected:04x} != {stored:04x}")
    return mtime


class _GzipReader(_streams.DecompressReader):
    def __init__(self, fp):
        super().__init__(_PaddedFile(fp), zlib._ZlibDecompressor,
                         wbits=-zlib.MAX_WBITS)
        self._new_member = True
        self._last_mtime = None

    def _init_read(self):
        self._crc = zlib.crc32(b"")
        self._stream_size = 0

    def _read_gzip_header(self):
        mtime = _read_gzip_header(self._fp)
        if mtime is None:
            return False
        self._last_mtime = mtime
        return True

    def read(self, size=-1):
        if size < 0:
            return self.readall()
        if not size:
            return b""
        while True:
            if self._decompressor.eof:
                # Finished a member: check its trailer and prepare for the next.
                self._read_eof()
                self._new_member = True
                self._decompressor = self._decomp_factory(**self._decomp_args)
            if self._new_member:
                self._init_read()
                if not self._read_gzip_header():
                    self._size = self._pos
                    return b""
                self._new_member = False
            dec = self._decompressor
            if dec.needs_input:
                chunk = self._fp.read(READ_BUFFER_SIZE)
                if not chunk:
                    raise EOFError("Compressed file ended before the "
                                   "end-of-stream marker was reached")
            else:
                chunk = b""
            out = dec.decompress(chunk, size)
            if dec.eof and dec.unused_data:
                self._fp.prepend(dec.unused_data)
            if out:
                break
        self._crc = zlib.crc32(out, self._crc)
        self._stream_size += len(out)
        self._pos += len(out)
        return out

    def _read_eof(self):
        crc, isize = struct.unpack("<II", _read_exact(self._fp, 8))
        if crc != self._crc:
            raise BadGzipFile("CRC check failed %s != %s" % (hex(crc),
                                                             hex(self._crc)))
        if isize != (self._stream_size & 0xffffffff):
            raise BadGzipFile("Incorrect length of data produced")
        # Members may be followed by zero padding.
        pad = b"\x00"
        while pad == b"\x00":
            pad = self._fp.read(1)
        if pad:
            self._fp.prepend(pad)

    def _rewind(self):
        super()._rewind()
        self._new_member = True


def compress(data, compresslevel=_COMPRESS_LEVEL_BEST, *, mtime=0):
    """Return data compressed as a single gzip member.

    mtime is stored in the header; None means the current time.
    """
    if mtime is None:
        mtime = time.time()
    mtime = int(mtime)
    if not 0 <= mtime < 1 << 32:
        mtime = 0
    if compresslevel == _COMPRESS_LEVEL_BEST:
        xfl = 2
    elif compresslevel == _COMPRESS_LEVEL_FAST:
        xfl = 4
    else:
        xfl = 0
    header = _GZIP_MAGIC + struct.pack('<BBLBB', 8, 0, mtime, xfl, _OS_UNKNOWN)
    body = zlib.compress(data, level=compresslevel, wbits=-zlib.MAX_WBITS)
    trailer = struct.pack("<LL", zlib.crc32(data), len(data) & 0xffffffff)
    return header + body + trailer


def decompress(data):
    """Decompress gzip data (all members) and return the bytes."""
    parts = []
    while True:
        fp = io.BytesIO(data)
        if _read_gzip_header(fp) is None:
            return b"".join(parts)
        dec = zlib.decompressobj(wbits=-zlib.MAX_WBITS)
        out = dec.decompress(data[fp.tell():])
        if not dec.eof or len(dec.unused_data) < 8:
            raise EOFError("Compressed file ended before the end-of-stream "
                           "marker was reached")
        crc, size = struct.unpack("<II", dec.unused_data[:8])
        if crc != zlib.crc32(out):
            raise BadGzipFile("CRC check failed")
        if size != (len(out) & 0xffffffff):
            raise BadGzipFile("Incorrect length of data produced")
        parts.append(out)
        data = dec.unused_data[8:].lstrip(b"\x00")


def main():
    from argparse import ArgumentParser
    import sys
    parser = ArgumentParser(
        description="A simple command line interface for the gzip module: "
                    "act like gzip, but do not delete the input file.",
        color=True,
    )
    group = parser.add_mutually_exclusive_group()
    group.add_argument('--fast', action='store_true', help='compress faster')
    group.add_argument('--best', action='store_true', help='compress better')
    group.add_argument("-d", "--decompress", action="store_true",
                       help="act like gunzip instead of gzip")
    parser.add_argument("args", nargs="*", default=["-"], metavar='file')
    args = parser.parse_args()

    level = _COMPRESS_LEVEL_TRADEOFF
    if args.fast:
        level = _COMPRESS_LEVEL_FAST
    elif args.best:
        level = _COMPRESS_LEVEL_BEST

    for arg in args.args:
        if args.decompress:
            if arg == "-":
                src = GzipFile(filename="", mode="rb",
                               fileobj=sys.stdin.buffer)
                dst = sys.stdout.buffer
            else:
                if not arg.endswith(".gz"):
                    sys.exit(f"filename doesn't end in .gz: {arg!r}")
                src = open(arg, "rb")
                dst = builtins.open(arg[:-3], "wb")
        else:
            if arg == "-":
                src = sys.stdin.buffer
                dst = GzipFile(filename="", mode="wb",
                               fileobj=sys.stdout.buffer, compresslevel=level)
            else:
                src = builtins.open(arg, "rb")
                dst = open(arg + ".gz", "wb")
        while True:
            chunk = src.read(READ_BUFFER_SIZE)
            if not chunk:
                break
            dst.write(chunk)
        if dst is not sys.stdout.buffer:
            dst.close()
        if src is not sys.stdin.buffer:
            src.close()


if __name__ == '__main__':
    main()
