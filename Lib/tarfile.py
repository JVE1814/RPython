"""Read and write tar archives.

Supports the POSIX.1-1988 (ustar), GNU and POSIX.1-2001 (pax) formats,
transparent gzip / bzip2 / xz / Zstandard compression (also for
non-seekable streams with the "r|*" / "w|*" modes) and extraction filters
that defend against hostile archives.
"""


import io
import os
import re
import shutil
import stat
import struct
import sys
import time

try:
    import pwd
except ImportError:
    pwd = None
try:
    import grp
except ImportError:
    grp = None

# Exceptions raised when a platform cannot create symbolic links.
symlink_exception = (AttributeError, NotImplementedError, OSError)

__all__ = ["TarFile", "TarInfo", "is_tarfile", "TarError", "ReadError",
           "CompressionError", "StreamError", "ExtractError", "HeaderError",
           "ENCODING", "USTAR_FORMAT", "GNU_FORMAT", "PAX_FORMAT",
           "DEFAULT_FORMAT", "open", "fully_trusted_filter", "data_filter",
           "tar_filter", "FilterError", "AbsoluteLinkError",
           "OutsideDestinationError", "SpecialFileError", "AbsolutePathError",
           "LinkOutsideDestinationError", "LinkFallbackError"]

# ---------------------------------------------------------------- constants

NUL = b"\0"
BLOCKSIZE = 512
RECORDSIZE = BLOCKSIZE * 20
GNU_MAGIC = b"ustar  \0"
POSIX_MAGIC = b"ustar\x0000"

LENGTH_NAME = 100
LENGTH_LINK = 100
LENGTH_PREFIX = 155

REGTYPE = b"0"
AREGTYPE = b"\0"
LNKTYPE = b"1"
SYMTYPE = b"2"
CHRTYPE = b"3"
BLKTYPE = b"4"
DIRTYPE = b"5"
FIFOTYPE = b"6"
CONTTYPE = b"7"

GNUTYPE_LONGNAME = b"L"
GNUTYPE_LONGLINK = b"K"
GNUTYPE_SPARSE = b"S"

XHDTYPE = b"x"
XGLTYPE = b"g"
SOLARIS_XHDTYPE = b"X"

USTAR_FORMAT = 0
GNU_FORMAT = 1
PAX_FORMAT = 2
DEFAULT_FORMAT = PAX_FORMAT

SUPPORTED_TYPES = (REGTYPE, AREGTYPE, LNKTYPE, SYMTYPE, DIRTYPE, FIFOTYPE,
                   CONTTYPE, CHRTYPE, BLKTYPE, GNUTYPE_LONGNAME,
                   GNUTYPE_LONGLINK, GNUTYPE_SPARSE)
REGULAR_TYPES = (REGTYPE, AREGTYPE, CONTTYPE, GNUTYPE_SPARSE)
GNU_TYPES = (GNUTYPE_LONGNAME, GNUTYPE_LONGLINK, GNUTYPE_SPARSE)

PAX_FIELDS = ("path", "linkpath", "size", "mtime", "uid", "gid",
              "uname", "gname")
PAX_NAME_FIELDS = {"path", "linkpath", "uname", "gname"}
PAX_NUMBER_FIELDS = {"atime": float, "ctime": float, "mtime": float,
                     "uid": int, "gid": int, "size": int}

if os.name == "nt":
    ENCODING = "utf-8"
else:
    ENCODING = sys.getfilesystemencoding()

# File type bits shown by TarFile.list() for each member type.
_TYPE_MODE_BITS = {
    REGTYPE: stat.S_IFREG,
    AREGTYPE: stat.S_IFREG,
    DIRTYPE: stat.S_IFDIR,
    SYMTYPE: stat.S_IFLNK,
    FIFOTYPE: stat.S_IFIFO,
    CHRTYPE: stat.S_IFCHR,
    BLKTYPE: stat.S_IFBLK,
}


def __getattr__(name):
    if name == "version":
        import warnings
        warnings._deprecated("version", remove=(3, 20))
        return "0.9.0"
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")


# Largest chunk read at once for GNU long names and pax headers, whose
# claimed size comes from the (untrusted) archive.
_EXTHEADER_READ_CHUNK = 1 << 20

# ---------------------------------------------------------------- helpers


def stn(s, length, encoding, errors):
    """Encode str *s* into a NUL-padded bytes field of *length* bytes."""
    if s is None:
        raise ValueError("metadata cannot contain None")
    raw = s.encode(encoding, errors)
    return raw[:length] + (length - len(raw)) * NUL


def nts(s, encoding, errors):
    """Decode a NUL-terminated bytes field."""
    end = s.find(b"\0")
    if end != -1:
        s = s[:end]
    return s.decode(encoding, errors)


def nti(s):
    """Parse a numeric header field (octal text or GNU base-256)."""
    if s[0] in (0o200, 0o377):
        value = int.from_bytes(s[1:], "big")
        if s[0] == 0o377:
            value -= 256 ** (len(s) - 1)
        return value
    try:
        text = nts(s, "ascii", "strict").strip(" \0")
        return int(text or "0", 8)
    except (ValueError, UnicodeDecodeError):
        raise InvalidHeaderError("invalid header") from None


def itn(n, digits=8, format=DEFAULT_FORMAT):
    """Render a number for a header field of *digits* bytes; GNU format
    switches to base-256 for values that do not fit in octal."""
    original = n
    try:
        n = int(n)
    except (TypeError, ValueError, OverflowError):
        raise ValueError(f"invalid number {original!r}") from None
    if 0 <= n < 8 ** (digits - 1):
        return b"%0*o" % (digits - 1, n) + NUL
    if format == GNU_FORMAT and -256 ** (digits - 1) <= n < 256 ** (digits - 1):
        if n >= 0:
            return bytes([0o200]) + n.to_bytes(digits - 1, "big")
        return bytes([0o377]) + (256 ** (digits - 1) + n).to_bytes(digits - 1, "big")
    raise ValueError("overflow in number field")


def calc_chksums(buf):
    """Return (unsigned, signed) header checksums, the chksum field counted
    as eight spaces."""
    head = buf[:148]
    tail = buf[156:512]
    unsigned = 256 + sum(head) + sum(tail)
    signed = 256 + sum(struct.unpack("148b", head)) + sum(struct.unpack("356b", tail))
    return unsigned, signed


def copyfileobj(src, dst, length=None, exception=OSError, bufsize=None):
    """Copy *length* bytes (everything if None) from *src* to *dst*;
    *exception* is raised if *src* ends early."""
    bufsize = bufsize or 16 * 1024
    if length == 0:
        return
    if length is None:
        shutil.copyfileobj(src, dst, bufsize)
        return
    remaining = length
    while remaining > 0:
        chunk = src.read(min(bufsize, remaining))
        if len(chunk) == 0:
            raise exception("unexpected end of data")
        dst.write(chunk)
        remaining -= len(chunk)


def _padded(size):
    """Size rounded up to whole blocks."""
    return -(-size // BLOCKSIZE) * BLOCKSIZE


def _safe_print(s):
    encoding = getattr(sys.stdout, 'encoding', None)
    if encoding is not None:
        s = s.encode(encoding, 'backslashreplace').decode(encoding)
    print(s, end=' ')


filemode = stat.filemode

# ---------------------------------------------------------------- exceptions


class TarError(Exception):
    """Base class for tarfile errors."""


class ExtractError(TarError):
    """Error during extraction (raised depending on errorlevel)."""


class ReadError(TarError):
    """The archive cannot be read."""


class CompressionError(TarError):
    """A compression method is unavailable or failed."""


class StreamError(TarError):
    """Operation not possible on a stream (non-seekable) TarFile."""


class HeaderError(TarError):
    """Base class for header problems."""


class EmptyHeaderError(HeaderError):
    """A header block is empty."""


class TruncatedHeaderError(HeaderError):
    """A header block is shorter than BLOCKSIZE."""


class EOFHeaderError(HeaderError):
    """An end-of-archive (all zero) block was found."""


class InvalidHeaderError(HeaderError):
    """A header block is malformed."""


class SubsequentHeaderError(HeaderError):
    """The header following an extended header is invalid."""


class FilterError(TarError):
    """An extraction filter rejected a member."""

    @property
    def tarinfo(self):
        return self._tarinfo


class AbsolutePathError(FilterError):
    def __init__(self, tarinfo):
        self._tarinfo = tarinfo
        super().__init__(f'member {tarinfo.name!r} has an absolute path')


class OutsideDestinationError(FilterError):
    def __init__(self, tarinfo, path):
        self._tarinfo = tarinfo
        self._path = path
        super().__init__(f'{tarinfo.name!r} would be extracted to {path!r}, '
                         + 'which is outside the destination')


class SpecialFileError(FilterError):
    def __init__(self, tarinfo):
        self._tarinfo = tarinfo
        super().__init__(f'{tarinfo.name!r} is a special file')


class AbsoluteLinkError(FilterError):
    def __init__(self, tarinfo):
        self._tarinfo = tarinfo
        super().__init__(f'{tarinfo.name!r} is a link to an absolute path')


class LinkOutsideDestinationError(FilterError):
    def __init__(self, tarinfo, path):
        self._tarinfo = tarinfo
        self._path = path
        super().__init__(f'{tarinfo.name!r} would link to {path!r}, '
                         + 'which is outside the destination')


class LinkFallbackError(FilterError):
    def __init__(self, tarinfo, path):
        self._tarinfo = tarinfo
        self._path = path
        super().__init__(f'link {tarinfo.name!r} would be extracted as a '
                         + f'copy of {path!r}, which was rejected')


# ---------------------------------------------------------------- streams

class _RawFile:
    """Minimal unbuffered file used by stream mode when given a name."""

    def __init__(self, name, mode):
        flags = {"r": os.O_RDONLY,
                 "w": os.O_WRONLY | os.O_CREAT | os.O_TRUNC}[mode]
        flags |= getattr(os, "O_BINARY", 0)
        self.fd = os.open(name, flags, 0o666)

    def close(self):
        os.close(self.fd)

    def read(self, size):
        return os.read(self.fd, size)

    def write(self, data):
        os.write(self.fd, data)


class _GzipStreamWriter:
    """Write a gzip member around raw deflate data (stream mode)."""

    def __init__(self, fileobj, compresslevel, mtime, name):
        import zlib
        self._zlib = zlib
        self.fileobj = fileobj
        self.comp = zlib.compressobj(compresslevel, zlib.DEFLATED,
                                     -zlib.MAX_WBITS, zlib.DEF_MEM_LEVEL, 0)
        self.crc = zlib.crc32(b"")
        self.size = 0
        if mtime is None:
            mtime = time.time()
        mtime = int(mtime)
        if not 0 <= mtime < 2 ** 32:
            mtime = 0
        flags = 0
        fname = b""
        if name:
            base = os.path.basename(name)
            if isinstance(base, str):
                base = base.encode("iso-8859-1", "replace")
            if base.endswith(b".gz"):
                base = base[:-3]
            if base:
                flags = 0x08
                fname = base + NUL
        xfl = 2 if compresslevel == 9 else (4 if compresslevel == 1 else 0)
        fileobj.write(b"\x1f\x8b\x08" + bytes([flags])
                      + struct.pack("<L", mtime) + bytes([xfl, 255]) + fname)

    def write(self, data):
        self.crc = self._zlib.crc32(data, self.crc)
        self.size += len(data)
        self.fileobj.write(self.comp.compress(data))

    def close(self):
        self.fileobj.write(self.comp.flush())
        self.fileobj.write(struct.pack("<LL", self.crc & 0xFFFFFFFF,
                                       self.size & 0xFFFFFFFF))


class _GzipStreamReader:
    """Decode a (possibly multi-member) gzip stream incrementally."""

    def __init__(self, zlib):
        self.zlib = zlib
        self.dec = zlib.decompressobj(16 + zlib.MAX_WBITS)

    def decompress(self, data):
        out = []
        while data:
            try:
                out.append(self.dec.decompress(data))
            except self.zlib.error as e:
                raise ReadError("invalid compressed data") from e
            if not self.dec.eof:
                break
            data = self.dec.unused_data
            if not data:
                break
            self.dec = self.zlib.decompressobj(16 + self.zlib.MAX_WBITS)
        return b"".join(out)


class _CompressedStream:
    """Sequential access to a (possibly compressed) tar stream.

    Used by the "r|*" and "w|*" modes: the underlying file object is only
    read or written sequentially and forward seeks are emulated.
    """

    def __init__(self, name, mode, comptype, fileobj, bufsize,
                 compresslevel, preset, mtime=None):
        self._own = False
        if fileobj is None:
            fileobj = _RawFile(name, mode)
            self._own = True
        if comptype == '*':
            # Detect the compression from the first bytes.
            fileobj = _PeekableFile(fileobj)
            comptype = fileobj.detect()
        self.name = name or ""
        self.mode = mode
        self.comptype = comptype
        self.fileobj = fileobj
        self.bufsize = bufsize
        self.buf = b""
        self.pos = 0
        self.closed = False
        self.encoder = None
        self.decoder = None
        self.exception = ()
        try:
            if comptype == "gz":
                try:
                    import zlib
                except ImportError:
                    raise CompressionError("zlib module is not available") from None
                self.exception = zlib.error
                if mode == "r":
                    self.decoder = _GzipStreamReader(zlib)
                else:
                    self.encoder = _GzipStreamWriter(fileobj, compresslevel,
                                                     mtime, self.name)
            elif comptype == "bz2":
                try:
                    import bz2
                except ImportError:
                    raise CompressionError("bz2 module is not available") from None
                self.exception = OSError
                if mode == "r":
                    self.decoder = _Chain(bz2.BZ2Decompressor)
                else:
                    self.encoder = _Encoder(fileobj, bz2.BZ2Compressor(compresslevel))
            elif comptype == "xz":
                try:
                    import lzma
                except ImportError:
                    raise CompressionError("lzma module is not available") from None
                self.exception = lzma.LZMAError
                if mode == "r":
                    self.decoder = _Chain(lzma.LZMADecompressor)
                else:
                    self.encoder = _Encoder(fileobj, lzma.LZMACompressor(preset=preset))
            elif comptype == "zst":
                try:
                    from compression import zstd
                except ImportError:
                    raise CompressionError("compression.zstd module is not available") from None
                self.exception = zstd.ZstdError
                if mode == "r":
                    self.decoder = _Chain(zstd.ZstdDecompressor)
                else:
                    self.encoder = _Encoder(fileobj, zstd.ZstdCompressor(),
                                            zstd=True)
            elif comptype != "tar":
                raise CompressionError("unknown compression type %r" % comptype)
        except:
            if self._own:
                fileobj.close()
            self.closed = True
            raise

    def __del__(self):
        if hasattr(self, "closed") and not self.closed:
            self.close()

    def write(self, data):
        self.pos += len(data)
        if self.encoder is not None:
            self.encoder.write(data)
        else:
            self.buf += data
            if len(self.buf) > self.bufsize:
                self.fileobj.write(self.buf)
                self.buf = b""

    def close(self):
        if self.closed:
            return
        self.closed = True
        try:
            if self.mode == "w":
                if self.encoder is not None:
                    self.encoder.close()
                elif self.buf:
                    self.fileobj.write(self.buf)
                self.buf = b""
        finally:
            if self._own:
                self.fileobj.close()

    def tell(self):
        return self.pos

    def seek(self, pos=0):
        if pos < self.pos:
            raise StreamError("seeking backwards is not allowed")
        remaining = pos - self.pos
        while remaining > 0:
            got = self.read(min(remaining, self.bufsize))
            if not got:
                break
            remaining -= len(got)
        return self.pos

    def _fill(self, size):
        while len(self.buf) < size:
            raw = self.fileobj.read(self.bufsize)
            if not raw:
                break
            if self.decoder is not None:
                try:
                    raw = self.decoder.decompress(raw)
                except self.exception as e:
                    raise ReadError("invalid compressed data") from e
            self.buf += raw

    def read(self, size):
        if size is None or size < 0:
            parts = []
            while True:
                chunk = self.read(self.bufsize)
                if not chunk:
                    break
                parts.append(chunk)
            return b"".join(parts)
        self._fill(size)
        data, self.buf = self.buf[:size], self.buf[size:]
        self.pos += len(data)
        return data


class _Chain:
    """Decompress concatenated streams with fresh decompressor objects."""

    def __init__(self, factory):
        self.factory = factory
        self.dec = factory()

    def decompress(self, data):
        out = []
        while data:
            if self.dec.eof:
                self.dec = self.factory()
            out.append(self.dec.decompress(data))
            if not self.dec.eof:
                break
            data = self.dec.unused_data
        return b"".join(out)


class _Encoder:
    def __init__(self, fileobj, comp, zstd=False):
        self.fileobj = fileobj
        self.comp = comp
        self.zstd = zstd

    def write(self, data):
        self.fileobj.write(self.comp.compress(data))

    def close(self):
        self.fileobj.write(self.comp.flush())


class _PeekableFile:
    """Wrap a file object so that its first block can be inspected."""

    def __init__(self, fileobj):
        self.fileobj = fileobj
        self.head = fileobj.read(BLOCKSIZE)

    def read(self, size):
        if self.head:
            data, self.head = self.head[:size], self.head[size:]
            if len(data) < size:
                data += self.fileobj.read(size - len(data))
            return data
        return self.fileobj.read(size)

    def close(self):
        self.fileobj.close()

    def detect(self):
        h = self.head
        if h.startswith(b"\x1f\x8b\x08"):
            return "gz"
        if h[0:3] == b"BZh" and h[4:10] == b"1AY&SY":
            return "bz2"
        if h.startswith((b"\x5d\x00\x00\x80", b"\xfd7zXZ")):
            return "xz"
        if h.startswith(b"\x28\xb5\x2f\xfd"):
            return "zst"
        return "tar"


class _FileInFile:
    """Read-only view of a member's data inside the archive file, with
    sparse regions filled with zeros."""

    def __init__(self, fileobj, offset, size, name, blockinfo=None):
        self.fileobj = fileobj
        self.offset = offset
        self.size = size
        self.position = 0
        self.name = name
        self.closed = False
        if blockinfo is None:
            blockinfo = [(0, size)]
        # (is_data, start, end, offset in archive)
        self.map = []
        pos = 0
        last_real = offset
        for off, length in blockinfo:
            if off > pos:
                self.map.append((False, pos, off, None))
            self.map.append((True, off, off + length, last_real))
            last_real += length
            pos = off + length
        if pos < size:
            self.map.append((False, pos, size, None))
        self.map_index = 0

    def flush(self):
        pass

    @property
    def mode(self):
        return 'rb'

    def readable(self):
        return True

    def writable(self):
        return False

    def seekable(self):
        return self.fileobj.seekable()

    def tell(self):
        return self.position

    def seek(self, position, whence=io.SEEK_SET):
        if whence == io.SEEK_SET:
            self.position = min(max(position, 0), self.size)
        elif whence == io.SEEK_CUR:
            if position < 0:
                self.position = max(self.position + position, 0)
            else:
                self.position = min(self.position + position, self.size)
        elif whence == io.SEEK_END:
            self.position = max(min(self.size + position, self.size), 0)
        else:
            raise ValueError("Invalid argument")
        return self.position

    def read(self, size=None):
        if size is None or size < 0:
            size = self.size - self.position
        else:
            size = min(size, self.size - self.position)
        out = bytearray()
        while size > 0:
            while True:
                is_data, start, stop, real = self.map[self.map_index]
                if start <= self.position < stop:
                    break
                self.map_index += 1
                if self.map_index == len(self.map):
                    self.map_index = 0
            n = min(size, stop - self.position)
            if is_data:
                self.fileobj.seek(real + (self.position - start))
                chunk = self.fileobj.read(n)
                if len(chunk) != n:
                    raise ReadError("unexpected end of data")
                out += chunk
            else:
                out += NUL * n
            size -= n
            self.position += n
        return bytes(out)

    def readinto(self, b):
        data = self.read(len(b))
        b[:len(data)] = data
        return len(data)

    def close(self):
        self.closed = True


class ExFileObject(io.BufferedReader):
    """Buffered reader for a member's data (returned by extractfile())."""

    def __init__(self, tarfile, tarinfo):
        raw = _FileInFile(tarfile.fileobj, tarinfo.offset_data, tarinfo.size,
                          tarinfo.name, tarinfo.sparse)
        super().__init__(raw)


# ---------------------------------------------------------------- TarInfo

class TarInfo(object):
    """Metadata of one archive member.  Instances are created by TarFile
    when reading, or by the user / TarFile.gettarinfo() for addfile()."""

    __slots__ = dict(
        name='Name of the archive member.',
        mode='Permission bits.',
        uid='User ID of the owner.',
        gid='Group ID of the owner.',
        size='Size in bytes.',
        mtime='Time of last modification.',
        chksum='Header checksum.',
        type=('File type: one of the REGTYPE, DIRTYPE, SYMTYPE, ... '
              'constants.'),
        linkname='Target of a hard or symbolic link.',
        uname='User name.',
        gname='Group name.',
        devmajor='Device major number.',
        devminor='Device minor number.',
        offset='Offset of the header in the archive.',
        offset_data="Offset of the member's data in the archive.",
        pax_headers='Pax extended header key/value pairs.',
        sparse='Sparse member information.',
        _sparse_structs=None,
        _link_target=None,
        _raw_type=None,
    )

    def __init__(self, name=""):
        self.name = name
        self.mode = 0o644
        self.uid = 0
        self.gid = 0
        self.size = 0
        self.mtime = 0
        self.chksum = 0
        self.type = REGTYPE
        self.linkname = ""
        self.uname = ""
        self.gname = ""
        self.devmajor = 0
        self.devminor = 0
        self.offset = 0
        self.offset_data = 0
        self.sparse = None
        self.pax_headers = {}
        self._raw_type = None

    @property
    def tarfile(self):
        import warnings
        warnings.warn('The undocumented "tarfile" attribute of TarInfo '
                      'objects is deprecated', category=DeprecationWarning,
                      stacklevel=2)
        return None

    @tarfile.setter
    def tarfile(self, tarfile):
        import warnings
        warnings.warn('The undocumented "tarfile" attribute of TarInfo '
                      'objects is deprecated', category=DeprecationWarning,
                      stacklevel=2)

    @property
    def path(self):
        """Alias of name."""
        return self.name

    @path.setter
    def path(self, name):
        self.name = name

    @property
    def linkpath(self):
        """Alias of linkname."""
        return self.linkname

    @linkpath.setter
    def linkpath(self, linkname):
        self.linkname = linkname

    def __repr__(self):
        return "<%s %r at %#x>" % (type(self).__name__, self.name, id(self))

    def replace(self, *, name=None, mtime=None, mode=None, linkname=None,
                uid=None, gid=None, uname=None, gname=None, deep=True,
                _KEEP=None):
        """Return a copy with the given attributes changed (``_KEEP``
        sentinel semantics: pass the value to set, including None)."""
        raise NotImplementedError  # replaced below

    def get_info(self):
        """Header fields as a dict."""
        mode = self.mode
        if mode is not None:
            mode = mode & 0o7777
        info = {
            "name": self.name,
            "mode": mode,
            "uid": self.uid,
            "gid": self.gid,
            "size": self.size,
            "mtime": self.mtime,
            "chksum": self.chksum,
            "type": self.type,
            "linkname": self.linkname,
            "uname": self.uname,
            "gname": self.gname,
            "devmajor": self.devmajor,
            "devminor": self.devminor,
        }
        if info["type"] == DIRTYPE and not info["name"].endswith("/"):
            info["name"] += "/"
        return info

    def tobuf(self, format=DEFAULT_FORMAT, encoding=ENCODING,
              errors="surrogateescape"):
        """Header block(s) for this member in the given format."""
        info = self.get_info()
        for key, value in info.items():
            if value is None:
                raise ValueError("%s may not be None" % key)
        if format == USTAR_FORMAT:
            return self.create_ustar_header(info, encoding, errors)
        if format == GNU_FORMAT:
            return self.create_gnu_header(info, encoding, errors)
        if format == PAX_FORMAT:
            return self.create_pax_header(info, encoding)
        raise ValueError("invalid format")

    def create_ustar_header(self, info, encoding, errors):
        info["magic"] = POSIX_MAGIC
        if len(info["linkname"].encode(encoding, errors)) > LENGTH_LINK:
            raise ValueError("linkname is too long")
        if len(info["name"].encode(encoding, errors)) > LENGTH_NAME:
            info["prefix"], info["name"] = self._split_ustar_name(
                info["name"], encoding, errors)
        return self._header_block(info, USTAR_FORMAT, encoding, errors)

    def create_gnu_header(self, info, encoding, errors):
        info["magic"] = GNU_MAGIC
        out = b""
        if len(info["linkname"].encode(encoding, errors)) > LENGTH_LINK:
            out += self._gnu_long_block(info["linkname"],
                                                GNUTYPE_LONGLINK,
                                                encoding, errors)
        if len(info["name"].encode(encoding, errors)) > LENGTH_NAME:
            out += self._gnu_long_block(info["name"],
                                                GNUTYPE_LONGNAME,
                                                encoding, errors)
        return out + self._header_block(info, GNU_FORMAT, encoding, errors)

    def create_pax_header(self, info, encoding):
        """Pax extended header (if needed) followed by a ustar header."""
        info["magic"] = POSIX_MAGIC
        pax = self.pax_headers.copy()
        # Text fields that are not plain ASCII or too long go to pax.
        for name, hname, length in (("name", "path", LENGTH_NAME),
                                    ("linkname", "linkpath", LENGTH_LINK),
                                    ("uname", "uname", 32),
                                    ("gname", "gname", 32)):
            if hname in pax:
                continue
            try:
                info[name].encode("ascii", "strict")
            except UnicodeEncodeError:
                pax[hname] = info[name]
                continue
            if len(info[name]) > length:
                pax[hname] = info[name]
        # Numbers that do not fit in their octal fields go to pax.
        for name, digits in (("uid", 8), ("gid", 8), ("size", 12),
                             ("mtime", 12)):
            value = info[name]
            fits = 0 <= value < 8 ** (digits - 1)
            if fits and not isinstance(value, float):
                continue
            if name not in pax:
                pax[name] = str(value)
            # The ustar field gets the nearest representable value.
            info[name] = round(value) if fits else 0
        if pax:
            out = self._pax_block(pax, XHDTYPE, encoding)
        else:
            out = b""
        return out + self._header_block(info, USTAR_FORMAT, "ascii",
                                         "replace")

    @classmethod
    def create_pax_global_header(cls, pax_headers):
        """Global pax header block(s) for *pax_headers*."""
        return cls._pax_block(pax_headers, XGLTYPE, "utf-8")

    def _split_ustar_name(self, name, encoding, errors):
        """Split a long name into (prefix, name) at a slash."""
        parts = name.split("/")
        for i in range(1, len(parts)):
            prefix = "/".join(parts[:i])
            rest = "/".join(parts[i:])
            if (len(prefix.encode(encoding, errors)) <= LENGTH_PREFIX and
                    len(rest.encode(encoding, errors)) <= LENGTH_NAME):
                return prefix, rest
        raise ValueError("name is too long")

    @staticmethod
    def _header_block(info, format, encoding, errors):
        has_dev = info.get("type") in (CHRTYPE, BLKTYPE)
        if has_dev:
            devmajor = itn(info.get("devmajor", 0), 8, format)
            devminor = itn(info.get("devminor", 0), 8, format)
        else:
            devmajor = stn("", 8, encoding, errors)
            devminor = stn("", 8, encoding, errors)
        filetype = info.get("type", REGTYPE)
        if filetype is None:
            raise ValueError("TarInfo.type must not be None")
        fields = [
            stn(info.get("name", ""), 100, encoding, errors),
            itn(info.get("mode", 0) & 0o7777, 8, format),
            itn(info.get("uid", 0), 8, format),
            itn(info.get("gid", 0), 8, format),
            itn(info.get("size", 0), 12, format),
            itn(info.get("mtime", 0), 12, format),
            b"        ",
            filetype,
            stn(info.get("linkname", ""), 100, encoding, errors),
            info.get("magic", POSIX_MAGIC),
            stn(info.get("uname", ""), 32, encoding, errors),
            stn(info.get("gname", ""), 32, encoding, errors),
            devmajor,
            devminor,
            stn(info.get("prefix", ""), 155, encoding, errors),
        ]
        buf = struct.pack("%ds" % BLOCKSIZE, b"".join(fields))
        chksum = calc_chksums(buf[-BLOCKSIZE:])[0]
        return buf[:148] + b"%06o\0" % chksum + buf[155:]

    @staticmethod
    def _pad_block(payload):
        blocks, rest = divmod(len(payload), BLOCKSIZE)
        if rest:
            payload += (BLOCKSIZE - rest) * NUL
        return payload

    @classmethod
    def _gnu_long_block(cls, name, type, encoding, errors):
        name = name.encode(encoding, errors) + NUL
        info = {"name": "././@LongLink", "type": type, "size": len(name),
                "magic": GNU_MAGIC}
        return (cls._header_block(info, USTAR_FORMAT, encoding, errors)
                + cls._pad_block(name))

    @classmethod
    def _pax_block(cls, pax_headers, type, encoding):
        # Values that are not valid UTF-8 (surrogate-escaped) force the
        # binary character set for the whole header.
        binary = False
        for value in pax_headers.values():
            try:
                value.encode("utf-8", "strict")
            except UnicodeEncodeError:
                binary = True
                break
        records = b""
        if binary:
            records += b"21 hdrcharset=BINARY\n"
        for key, value in pax_headers.items():
            key = key.encode("utf-8")
            if binary:
                value = value.encode(encoding, "surrogateescape")
            else:
                value = value.encode("utf-8")
            body = len(key) + len(value) + 3  # space, '=', newline
            n = p = 0
            while True:
                n = body + len(str(p))
                if n == p:
                    break
                p = n
            records += bytes(str(p), "ascii") + b" " + key + b"=" + value + b"\n"
        info = {"name": "././@PaxHeader", "type": type, "size": len(records),
                "magic": POSIX_MAGIC}
        return (cls._header_block(info, USTAR_FORMAT, "ascii", "replace")
                + cls._pad_block(records))

    @classmethod
    def frombuf(cls, buf, encoding, errors):
        """Build a TarInfo from one 512-byte header block."""
        if len(buf) == 0:
            raise EmptyHeaderError("empty header")
        if len(buf) != BLOCKSIZE:
            raise TruncatedHeaderError("truncated header")
        if buf.count(NUL) == BLOCKSIZE:
            raise EOFHeaderError("end of file header")
        chksum = nti(buf[148:156])
        if chksum not in calc_chksums(buf):
            raise InvalidHeaderError("bad checksum")

        obj = cls()
        obj.name = nts(buf[0:100], encoding, errors)
        obj.mode = nti(buf[100:108])
        obj.uid = nti(buf[108:116])
        obj.gid = nti(buf[116:124])
        obj.size = nti(buf[124:136])
        obj.mtime = nti(buf[136:148])
        obj.chksum = chksum
        obj.type = buf[156:157]
        obj.linkname = nts(buf[157:257], encoding, errors)
        obj.uname = nts(buf[265:297], encoding, errors)
        obj.gname = nts(buf[297:329], encoding, errors)
        obj.devmajor = nti(buf[329:337])
        obj.devminor = nti(buf[337:345])
        prefix = nts(buf[345:500], encoding, errors)

        if obj.size < 0:
            raise InvalidHeaderError("invalid header")

        # Old V7 archives mark directories with a trailing slash only.
        obj._raw_type = obj.type
        if obj.type == AREGTYPE and obj.name.endswith("/"):
            obj.type = DIRTYPE

        if obj.type == GNUTYPE_SPARSE:
            structs = []
            pos = 386
            for _ in range(4):
                try:
                    offset = nti(buf[pos:pos + 12])
                    numbytes = nti(buf[pos + 12:pos + 24])
                except ValueError:
                    break
                structs.append((offset, numbytes))
                pos += 24
            isextended = bool(buf[482])
            origsize = nti(buf[483:495])
            obj._sparse_structs = (structs, isextended, origsize)

        if obj.isdir():
            obj.name = obj.name.rstrip("/")

        if prefix and obj.type not in GNU_TYPES:
            obj.name = prefix + "/" + obj.name
        return obj

    @classmethod
    def fromtarfile(cls, tarfile):
        """Read the next member (with its extended headers) from *tarfile*."""
        buf = tarfile.fileobj.read(BLOCKSIZE)
        obj = cls.frombuf(buf, tarfile.encoding, tarfile.errors)
        obj.offset = tarfile.fileobj.tell() - BLOCKSIZE
        return obj._read_rest(tarfile)

    # -- processing of the various member kinds

    def _read_rest(self, tarfile):
        if self.type in (GNUTYPE_LONGNAME, GNUTYPE_LONGLINK):
            return self._finish_gnu_long(tarfile)
        if self.type == GNUTYPE_SPARSE:
            return self._finish_old_sparse(tarfile)
        if self.type in (XHDTYPE, XGLTYPE, SOLARIS_XHDTYPE):
            return self._finish_pax(tarfile)
        return self._finish_plain(tarfile)

    def _finish_plain(self, tarfile):
        self.offset_data = tarfile.fileobj.tell()
        offset = self.offset_data
        if self.isreg() or self.type not in SUPPORTED_TYPES:
            offset += _padded(self.size)
        tarfile.offset = offset
        self._merge_pax(tarfile.pax_headers, tarfile.encoding,
                             tarfile.errors)
        if self.isdir():
            self.name = self.name.rstrip("/")
        return self

    def _read_ext_data(self, tarfile):
        """Read this header's data area in bounded chunks."""
        want = _padded(self.size)
        chunks = []
        got = 0
        while got < want:
            chunk = tarfile.fileobj.read(min(_EXTHEADER_READ_CHUNK, want - got))
            if not chunk:
                break
            chunks.append(chunk)
            got += len(chunk)
        data = b"".join(chunks)
        if len(data) < self.size:
            raise TruncatedHeaderError("truncated extended header")
        return data

    def _next_header(self, tarfile):
        try:
            return self.fromtarfile(tarfile)
        except HeaderError as e:
            raise SubsequentHeaderError(str(e)) from None

    def _finish_gnu_long(self, tarfile):
        buf = self._read_ext_data(tarfile)
        nxt = self._next_header(tarfile)
        nxt.offset = self.offset
        if self.type == GNUTYPE_LONGNAME:
            nxt.name = nts(buf, tarfile.encoding, tarfile.errors)
            nxt._recheck_v7_dir()
        elif self.type == GNUTYPE_LONGLINK:
            nxt.linkname = nts(buf, tarfile.encoding, tarfile.errors)
        return nxt

    def _finish_old_sparse(self, tarfile):
        structs, isextended, origsize = self._sparse_structs
        del self._sparse_structs
        while isextended:
            buf = tarfile.fileobj.read(BLOCKSIZE)
            pos = 0
            for _ in range(21):
                try:
                    offset = nti(buf[pos:pos + 12])
                    numbytes = nti(buf[pos + 12:pos + 24])
                except ValueError:
                    break
                if offset and numbytes:
                    structs.append((offset, numbytes))
                pos += 24
            isextended = bool(buf[504])
        self.sparse = structs
        self.offset_data = tarfile.fileobj.tell()
        tarfile.offset = self.offset_data + _padded(self.size)
        self.size = origsize
        return self

    _PAX_RECORD = re.compile(br"(\d{1,20}) ")

    def _finish_pax(self, tarfile):
        buf = self._read_ext_data(tarfile)
        if self.type == XGLTYPE:
            pax_headers = tarfile.pax_headers
        else:
            pax_headers = tarfile.pax_headers.copy()

        # The character set of the values; BINARY means "as in the archive".
        encoding = "utf-8"
        match = re.search(br"\d+ hdrcharset=([^\n]+)\n", buf)
        if match is not None:
            charset = match.group(1).decode("utf-8")
            if charset == "BINARY":
                encoding = tarfile.encoding
            else:
                encoding = "utf-8"

        sparse_map = []
        pos = 0
        end = len(buf)
        while pos < end and buf[pos] != 0:
            m = self._PAX_RECORD.match(buf, pos)
            if not m:
                raise InvalidHeaderError("invalid header")
            length = int(m.group(1))
            if length < 5 or pos + length > len(buf):
                raise InvalidHeaderError("invalid header")
            record = buf[m.end():pos + length]
            if not record.endswith(b"\n") or b"=" not in record:
                raise InvalidHeaderError("invalid header")
            raw_key, raw_value = record[:-1].split(b"=", 1)
            pos += length

            key = self._pax_text(raw_key, "utf-8", "utf-8",
                                         tarfile.errors)
            if key in PAX_NAME_FIELDS:
                value = self._pax_text(raw_value, encoding,
                                               tarfile.encoding,
                                               tarfile.errors)
            else:
                value = self._pax_text(raw_value, "utf-8", "utf-8",
                                               tarfile.errors)
            # GNU sparse format 0.0 repeats offset/numbytes records.
            if key == "GNU.sparse.offset":
                sparse_map.append(int(value))
                continue
            if key == "GNU.sparse.numbytes":
                sparse_map.append(int(value))
                continue
            pax_headers[key] = value

        nxt = self._next_header(tarfile)

        if self.type in (XHDTYPE, SOLARIS_XHDTYPE):
            data_start = nxt.offset_data
            nxt._merge_pax(pax_headers, tarfile.encoding, tarfile.errors)
            nxt.offset = self.offset
            # Size of the data actually stored in the archive.
            stored_size = nxt.size
            if sparse_map:
                pax_headers["GNU.sparse.map"] = ",".join(map(str, sparse_map))
            if "GNU.sparse.map" in pax_headers:
                self._pax_sparse_v01(nxt, pax_headers)
            elif (pax_headers.get("GNU.sparse.major") == "1"
                  and pax_headers.get("GNU.sparse.minor") == "0"):
                self._pax_sparse_v10(nxt, pax_headers, tarfile)
            elif "GNU.sparse.size" in pax_headers:
                self._pax_sparse_v00(nxt, pax_headers)
            if nxt.sparse is not None:
                real = pax_headers.get("GNU.sparse.realsize",
                                       pax_headers.get("GNU.sparse.size"))
                if real is not None:
                    nxt.size = int(real)
            if nxt.isreg() or nxt.type not in SUPPORTED_TYPES:
                tarfile.offset = data_start + _padded(stored_size)
            else:
                tarfile.offset = data_start
        return nxt

    def _pax_sparse_v00(self, nxt, pax_headers):
        nxt.sparse = []
        nxt.size = int(pax_headers["GNU.sparse.size"])

    def _pax_sparse_v01(self, nxt, pax_headers):
        values = [int(x) for x in pax_headers["GNU.sparse.map"].split(",")]
        nxt.sparse = list(zip(values[::2], values[1::2]))
        if "GNU.sparse.size" in pax_headers:
            pax_headers.setdefault("GNU.sparse.realsize",
                                   pax_headers["GNU.sparse.size"])

    def _pax_sparse_v10(self, nxt, pax_headers, tarfile):
        # The map is stored at the start of the data area as decimal
        # numbers separated by newlines, padded to a block boundary.
        fields = []
        buf = tarfile.fileobj.read(BLOCKSIZE)
        consumed = BLOCKSIZE
        text = buf

        def next_number():
            nonlocal text, consumed
            while b"\n" not in text:
                more = tarfile.fileobj.read(BLOCKSIZE)
                if not more:
                    raise InvalidHeaderError("invalid sparse map")
                consumed += BLOCKSIZE
                text += more
            num, text = text.split(b"\n", 1)
            return int(num)

        count = next_number()
        for _ in range(2 * count):
            fields.append(next_number())
        nxt.offset_data = tarfile.fileobj.tell()
        nxt.sparse = list(zip(fields[::2], fields[1::2]))

    def _merge_pax(self, pax_headers, encoding, errors):
        for key, value in pax_headers.items():
            if key == "GNU.sparse.name":
                key = "path"
            elif key not in PAX_FIELDS:
                continue
            if key in PAX_NUMBER_FIELDS:
                try:
                    value = PAX_NUMBER_FIELDS[key](value)
                except ValueError:
                    value = 0
            setattr(self, key, value)
            if key == "path":
                self._recheck_v7_dir()
        self.pax_headers = pax_headers.copy()

    def _recheck_v7_dir(self):
        """Old-style members are directories when their name ends with a
        slash; re-evaluate after the name was replaced by an extended
        header, then drop the trailing slash of directories."""
        if self._raw_type == AREGTYPE:
            self.type = DIRTYPE if self.name.endswith("/") else AREGTYPE
        if self.isdir():
            self.name = self.name.rstrip("/")

    def _pax_text(self, value, encoding, fallback_encoding,
                          fallback_errors):
        try:
            return value.decode(encoding, "strict")
        except (UnicodeDecodeError, TypeError, LookupError):
            return value.decode(fallback_encoding, fallback_errors)

    # -- type predicates

    def isreg(self):
        """True for regular files."""
        return self.type in REGULAR_TYPES

    def isfile(self):
        """True for regular files."""
        return self.isreg()

    def isdir(self):
        return self.type == DIRTYPE

    def issym(self):
        return self.type == SYMTYPE

    def islnk(self):
        return self.type == LNKTYPE

    def ischr(self):
        return self.type == CHRTYPE

    def isblk(self):
        return self.type == BLKTYPE

    def isfifo(self):
        return self.type == FIFOTYPE

    def issparse(self):
        return self.sparse is not None

    def isdev(self):
        """True for character devices, block devices and FIFOs."""
        return self.type in (CHRTYPE, BLKTYPE, FIFOTYPE)


_KEEP = object()


def _tarinfo_replace(self, *, name=_KEEP, mtime=_KEEP, mode=_KEEP,
                     linkname=_KEEP, uid=_KEEP, gid=_KEEP, uname=_KEEP,
                     gname=_KEEP, deep=True):
    """Return a copy of this TarInfo with the given attributes replaced."""
    import copy
    result = copy.deepcopy(self) if deep else copy.copy(self)
    for attr, value in (("name", name), ("mtime", mtime), ("mode", mode),
                        ("linkname", linkname), ("uid", uid), ("gid", gid),
                        ("uname", uname), ("gname", gname)):
        if value is not _KEEP:
            setattr(result, attr, value)
    return result


TarInfo.replace = _tarinfo_replace
del _tarinfo_replace


# ---------------------------------------------------------------- filters

def _inside(path, root):
    return os.path.commonpath([path, root]) == root


def _filtered_attrs(member, dest_path, for_data):
    changes = {}
    name = member.name
    dest_path = os.path.realpath(dest_path, strict=os.path.ALLOW_MISSING)
    if name.startswith(("/", os.sep)):
        name = changes["name"] = member.path.lstrip("/" + os.sep)
    if os.path.isabs(name):
        raise AbsolutePathError(member)
    target = os.path.realpath(os.path.join(dest_path, name),
                              strict=os.path.ALLOW_MISSING)
    if not _inside(target, dest_path):
        raise OutsideDestinationError(member, target)
    is_link = member.islnk() or member.issym()
    if (not member.isdir() and
            os.path.normpath(os.path.join(dest_path, name)) == dest_path):
        # A member named like the destination itself (e.g. a link called
        # "." or "") would replace it.
        raise OutsideDestinationError(member, target)
    if ".." in name.replace(os.sep, "/").split("/"):
        # Directories along the stored name would be created outside the
        # destination even though the final path is inside: use the
        # resolved location instead.
        name = changes["name"] = os.path.relpath(target, dest_path)

    mode = member.mode
    if mode is not None:
        # Both filters drop setuid/setgid/sticky and group/other write.
        mode = mode & 0o7777 & ~(stat.S_ISUID | stat.S_ISGID | stat.S_ISVTX
                                 | stat.S_IWGRP | stat.S_IWOTH)
        if for_data:
            if member.isreg() or member.islnk():
                if not mode & stat.S_IXUSR:
                    mode &= ~(stat.S_IXGRP | stat.S_IXOTH)
                mode |= stat.S_IRUSR | stat.S_IWUSR
            elif member.isdir() or member.issym():
                mode = None
            else:
                raise SpecialFileError(member)
        if mode != member.mode:
            changes["mode"] = mode

    if for_data:
        for attr in ("uid", "gid", "uname", "gname"):
            if getattr(member, attr) is not None:
                changes[attr] = None
        if is_link:
            if os.path.isabs(member.linkname):
                raise AbsoluteLinkError(member)
            linkname = os.path.normpath(member.linkname)
            if linkname != member.linkname:
                changes["linkname"] = linkname
            if member.issym():
                # Relative to the directory holding the link (a trailing
                # slash on the member name does not make it a directory).
                parent = os.path.dirname(name.rstrip("/" + os.sep))
                link_target = os.path.join(dest_path, parent, linkname)
            else:
                link_target = os.path.join(dest_path, linkname)
            link_target = os.path.realpath(link_target,
                                           strict=os.path.ALLOW_MISSING)
            if not _inside(link_target, dest_path):
                raise LinkOutsideDestinationError(member, link_target)
    return changes


def fully_trusted_filter(member, dest_path):
    """Extract everything as stored (only for trusted archives)."""
    return member


def tar_filter(member, dest_path):
    """Refuse absolute names and paths escaping the destination; drop
    setuid/setgid/sticky and group/other write permissions."""
    changes = _filtered_attrs(member, dest_path, False)
    if changes:
        return member.replace(**changes, deep=False)
    return member


def data_filter(member, dest_path):
    """Like tar_filter, and additionally refuse links leaving the
    destination and special files, normalise permissions and drop owner
    information: suitable for untrusted data archives."""
    changes = _filtered_attrs(member, dest_path, True)
    if changes:
        return member.replace(**changes, deep=False)
    return member


_NAMED_FILTERS = {
    "fully_trusted": fully_trusted_filter,
    "tar": tar_filter,
    "data": data_filter,
}


# ---------------------------------------------------------------- TarFile

class TarFile(object):
    """A tar archive opened for reading, writing or appending."""

    debug = 0
    dereference = False
    ignore_zeros = False
    errorlevel = 1
    format = DEFAULT_FORMAT
    encoding = ENCODING
    errors = None
    tarinfo = TarInfo
    fileobject = ExFileObject
    extraction_filter = None

    def __init__(self, name=None, mode="r", fileobj=None, format=None,
                 tarinfo=None, dereference=None, ignore_zeros=None,
                 encoding=None, errors="surrogateescape", pax_headers=None,
                 debug=None, errorlevel=None, copybufsize=None, stream=False):
        modes = {"r": "rb", "a": "r+b", "w": "wb", "x": "xb"}
        if mode not in modes:
            raise ValueError("mode must be 'r', 'a', 'w' or 'x'")
        self.mode = mode
        self._mode = modes[mode]

        if not fileobj:
            if self.mode == "a" and not os.path.exists(name):
                # Appending to a missing file creates it.
                self.mode = "w"
                self._mode = "wb"
            fileobj = bltn_open(name, self._mode)
            self._extfileobj = False
        else:
            if (name is None and hasattr(fileobj, "name") and
                    isinstance(fileobj.name, (str, bytes))):
                name = fileobj.name
            if hasattr(fileobj, "mode"):
                self._mode = fileobj.mode
            self._extfileobj = True
        self.name = os.path.abspath(name) if name else None
        self.fileobj = fileobj
        self.stream = stream

        if format is not None:
            self.format = format
        if tarinfo is not None:
            self.tarinfo = tarinfo
        if dereference is not None:
            self.dereference = dereference
        if ignore_zeros is not None:
            self.ignore_zeros = ignore_zeros
        if encoding is not None:
            self.encoding = encoding
        self.errors = errors
        if pax_headers is not None and self.format == PAX_FORMAT:
            self.pax_headers = pax_headers
        else:
            self.pax_headers = {}
        if debug is not None:
            self.debug = debug
        if errorlevel is not None:
            self.errorlevel = errorlevel

        self.copybufsize = copybufsize
        self.closed = False
        self.members = []
        self._loaded = False
        self.offset = self.fileobj.tell()
        self.inodes = {}
        self._unames = {}
        self._gnames = {}

        try:
            if self.mode == "r":
                self.firstmember = None
                self.firstmember = self.next()
            if self.mode == "a":
                # Find the end of the existing archive.
                while True:
                    self.fileobj.seek(self.offset)
                    try:
                        tarinfo = self.tarinfo.fromtarfile(self)
                        self.members.append(tarinfo)
                    except EOFHeaderError:
                        self.fileobj.seek(self.offset)
                        break
                    except HeaderError as e:
                        raise ReadError(str(e)) from None
            if self.mode in ("a", "w", "x"):
                self._loaded = True
                if self.pax_headers:
                    buf = self.tarinfo.create_pax_global_header(
                        self.pax_headers.copy())
                    self.fileobj.write(buf)
                    self.offset += len(buf)
        except:
            if not self._extfileobj:
                self.fileobj.close()
            self.closed = True
            raise

    # -- constructors

    @classmethod
    def open(cls, name=None, mode="r", fileobj=None, bufsize=RECORDSIZE,
             **kwargs):
        """Open a tar archive; see the module documentation for *mode*
        ('r', 'r:*', 'r:', 'r:gz', 'r|*', 'w:xz', 'a', 'x:bz2', ...)."""
        if not name and not fileobj:
            raise ValueError("nothing to open")

        if mode in ("r", "r:*"):
            if cls.extraction_filter is None:
                pass

            def not_compressed(comptype):
                return cls.OPEN_METH[comptype] == "taropen"

            failures = []
            for comptype in sorted(cls.OPEN_METH, key=not_compressed):
                func = getattr(cls, cls.OPEN_METH[comptype])
                saved_pos = fileobj.tell() if fileobj is not None else None
                try:
                    return func(name, "r", fileobj, **kwargs)
                except (ReadError, CompressionError) as e:
                    failures.append(f"- method {comptype}: {e!r}")
                    if fileobj is not None:
                        fileobj.seek(saved_pos)
                    continue
            raise ReadError("file could not be opened successfully:\n"
                            + "\n".join(failures))

        if ":" in mode:
            filemode, comptype = mode.split(":", 1)
            filemode = filemode or "r"
            comptype = comptype or "tar"
            if comptype in cls.OPEN_METH:
                func = getattr(cls, cls.OPEN_METH[comptype])
            else:
                raise CompressionError("unknown compression type %r" % comptype)
            return func(name, filemode, fileobj, **kwargs)

        if "|" in mode:
            filemode, comptype = mode.split("|", 1)
            filemode = filemode or "r"
            comptype = comptype or "tar"
            if filemode not in ("r", "w"):
                raise ValueError("mode must be 'r' or 'w'")
            if "compresslevel" in kwargs and comptype not in ("gz", "bz2"):
                raise ValueError("compresslevel is only valid for w|gz and w|bz2")
            if "preset" in kwargs and comptype != "xz":
                raise ValueError("preset is only valid for w|xz")
            compresslevel = kwargs.pop("compresslevel", 9)
            preset = kwargs.pop("preset", None)
            mtime = kwargs.pop("mtime", None)
            stream = _CompressedStream(name, filemode, comptype, fileobj,
                                       bufsize, compresslevel, preset, mtime)
            try:
                t = cls(name, filemode, stream, **kwargs)
            except:
                stream.close()
                raise
            t._extfileobj = False
            return t

        if mode in ("a", "w", "x"):
            return cls.taropen(name, mode, fileobj, **kwargs)

        raise ValueError("undiscernible mode")

    @classmethod
    def taropen(cls, name, mode="r", fileobj=None, **kwargs):
        """Open an uncompressed archive."""
        if mode not in ("r", "a", "w", "x"):
            raise ValueError("mode must be 'r', 'a', 'w' or 'x'")
        return cls(name, mode, fileobj, **kwargs)

    @classmethod
    def gzopen(cls, name, mode="r", fileobj=None, compresslevel=9, *,
               mtime=None, **kwargs):
        """Open a gzip-compressed archive."""
        if mode not in ("r", "w", "x"):
            raise ValueError("mode must be 'r', 'w' or 'x'")
        try:
            from gzip import GzipFile
        except ImportError:
            raise CompressionError("gzip module is not available") from None
        try:
            fileobj = GzipFile(name, mode + "b", compresslevel, fileobj,
                               mtime=mtime)
        except OSError as e:
            if fileobj is not None and mode == "r":
                raise ReadError("not a gzip file") from e
            raise
        try:
            t = cls.taropen(name, mode, fileobj, **kwargs)
        except OSError as e:
            fileobj.close()
            if mode == "r":
                raise ReadError("not a gzip file") from e
            raise
        except:
            fileobj.close()
            raise
        t._extfileobj = False
        return t

    @classmethod
    def bz2open(cls, name, mode="r", fileobj=None, compresslevel=9, **kwargs):
        """Open a bzip2-compressed archive."""
        if mode not in ("r", "w", "x"):
            raise ValueError("mode must be 'r', 'w' or 'x'")
        try:
            from bz2 import BZ2File
        except ImportError:
            raise CompressionError("bz2 module is not available") from None
        fileobj = BZ2File(fileobj or name, mode, compresslevel=compresslevel)
        try:
            t = cls.taropen(name, mode, fileobj, **kwargs)
        except (OSError, EOFError) as e:
            fileobj.close()
            if mode == "r":
                raise ReadError("not a bzip2 file") from e
            raise
        except:
            fileobj.close()
            raise
        t._extfileobj = False
        return t

    @classmethod
    def xzopen(cls, name, mode="r", fileobj=None, preset=None, **kwargs):
        """Open an xz (LZMA) compressed archive."""
        if mode not in ("r", "w", "x"):
            raise ValueError("mode must be 'r', 'w' or 'x'")
        try:
            from lzma import LZMAFile, LZMAError
        except ImportError:
            raise CompressionError("lzma module is not available") from None
        fileobj = LZMAFile(fileobj or name, mode, preset=preset)
        try:
            t = cls.taropen(name, mode, fileobj, **kwargs)
        except (LZMAError, EOFError) as e:
            fileobj.close()
            if mode == "r":
                raise ReadError("not an lzma file") from e
            raise
        except:
            fileobj.close()
            raise
        t._extfileobj = False
        return t

    @classmethod
    def zstopen(cls, name, mode="r", fileobj=None, level=None, options=None,
                zstd_dict=None, **kwargs):
        """Open a Zstandard compressed archive."""
        if mode not in ("r", "w", "x"):
            raise ValueError("mode must be 'r', 'w' or 'x'")
        try:
            from compression.zstd import ZstdFile, ZstdError
        except ImportError:
            raise CompressionError("compression.zstd module is not available") from None
        fileobj = ZstdFile(fileobj or name, mode, level=level,
                           options=options, zstd_dict=zstd_dict)
        try:
            t = cls.taropen(name, mode, fileobj, **kwargs)
        except (ZstdError, EOFError) as e:
            fileobj.close()
            if mode == "r":
                raise ReadError("not a zstd file") from e
            raise
        except:
            fileobj.close()
            raise
        t._extfileobj = False
        return t

    OPEN_METH = {
        "tar": "taropen",
        "gz": "gzopen",
        "bz2": "bz2open",
        "xz": "xzopen",
        "zst": "zstopen",
    }

    # -- archive-level operations

    def close(self):
        """Close the archive (writing the end-of-archive marker when
        writing).  A file object passed in by the caller is not closed."""
        if self.closed:
            return
        self.closed = True
        try:
            if self.mode in ("a", "w", "x"):
                self.fileobj.write(NUL * (BLOCKSIZE * 2))
                self.offset += BLOCKSIZE * 2
                blocks, remainder = divmod(self.offset, RECORDSIZE)
                if remainder > 0:
                    self.fileobj.write(NUL * (RECORDSIZE - remainder))
        finally:
            if not self._extfileobj:
                self.fileobj.close()

    def getmember(self, name):
        """The TarInfo of member *name* (its last occurrence); KeyError if
        absent."""
        tarinfo = self._lookup(name)
        if tarinfo is None:
            tarinfo = self._lookup(name.rstrip('/'))
        if tarinfo is None:
            raise KeyError("filename %r not found" % name)
        return tarinfo

    def getmembers(self):
        """All members as a list of TarInfo objects (in archive order)."""
        self._ensure()
        if not self._loaded:
            self._read_all()
        return self.members

    def getnames(self):
        """Names of all members."""
        return [tarinfo.name for tarinfo in self.getmembers()]

    def gettarinfo(self, name=None, arcname=None, fileobj=None):
        """Create a TarInfo from a file on disk (by *name*) or from the
        os.fstat() of a file object."""
        self._ensure("awx")

        if fileobj is not None:
            name = fileobj.name
        if arcname is None:
            arcname = name
        drv, arcname = os.path.splitdrive(arcname)
        arcname = arcname.replace(os.sep, "/")
        arcname = arcname.lstrip("/")

        tarinfo = self.tarinfo()
        tarinfo._link_target = None

        if fileobj is None:
            if not self.dereference:
                statres = os.lstat(name)
            else:
                statres = os.stat(name)
        else:
            statres = os.fstat(fileobj.fileno())
        linkname = ""

        stmd = statres.st_mode
        if stat.S_ISREG(stmd):
            inode = (statres.st_ino, statres.st_dev)
            if (not self.dereference and statres.st_nlink > 1 and
                    inode in self.inodes and arcname != self.inodes[inode]):
                # Already archived: store as a hard link.
                type = LNKTYPE
                linkname = self.inodes[inode]
            else:
                type = REGTYPE
                if inode[0]:
                    self.inodes[inode] = arcname
        elif stat.S_ISDIR(stmd):
            type = DIRTYPE
        elif stat.S_ISFIFO(stmd):
            type = FIFOTYPE
        elif stat.S_ISLNK(stmd):
            type = SYMTYPE
            linkname = os.readlink(name)
        elif stat.S_ISCHR(stmd):
            type = CHRTYPE
        elif stat.S_ISBLK(stmd):
            type = BLKTYPE
        else:
            return None

        tarinfo.name = arcname
        tarinfo.mode = stmd
        tarinfo.uid = statres.st_uid
        tarinfo.gid = statres.st_gid
        if type == REGTYPE:
            tarinfo.size = statres.st_size
        else:
            tarinfo.size = 0
        tarinfo.mtime = statres.st_mtime
        tarinfo.type = type
        tarinfo.linkname = linkname

        if pwd:
            if tarinfo.uid in self._unames:
                tarinfo.uname = self._unames[tarinfo.uid]
            else:
                try:
                    tarinfo.uname = pwd.getpwuid(tarinfo.uid)[0]
                    self._unames[tarinfo.uid] = tarinfo.uname
                except (KeyError, OverflowError):
                    self._unames[tarinfo.uid] = ''
        if grp:
            if tarinfo.gid in self._gnames:
                tarinfo.gname = self._gnames[tarinfo.gid]
            else:
                try:
                    tarinfo.gname = grp.getgrgid(tarinfo.gid)[0]
                    self._gnames[tarinfo.gid] = tarinfo.gname
                except (KeyError, OverflowError):
                    self._gnames[tarinfo.gid] = ''

        if type in (CHRTYPE, BLKTYPE):
            if hasattr(os, "major") and hasattr(os, "minor"):
                tarinfo.devmajor = os.major(statres.st_rdev)
                tarinfo.devminor = os.minor(statres.st_rdev)
        return tarinfo

    def list(self, verbose=True, *, members=None):
        """Print a table of contents (like ``tar -t``, ``tar -tv`` when
        *verbose*)."""
        self._ensure()
        if members is None:
            members = self
        for tarinfo in members:
            if verbose:
                if tarinfo.mode is None:
                    _safe_print("??????????")
                else:
                    _safe_print(stat.filemode(tarinfo.mode | _TYPE_MODE_BITS.get(tarinfo.type, 0)))
                _safe_print("%s/%s" % (tarinfo.uname or tarinfo.uid,
                                       tarinfo.gname or tarinfo.gid))
                if tarinfo.ischr() or tarinfo.isblk():
                    _safe_print("%10s" %
                                ("%d,%d" % (tarinfo.devmajor, tarinfo.devminor)))
                else:
                    _safe_print("%10d" % tarinfo.size)
                if tarinfo.mtime is None:
                    _safe_print("????-??-?? ??:??:??")
                else:
                    _safe_print("%d-%02d-%02d %02d:%02d:%02d"
                                % time.localtime(tarinfo.mtime)[:6])

            _safe_print(tarinfo.name + ("/" if tarinfo.isdir() else ""))

            if verbose:
                if tarinfo.issym():
                    _safe_print("-> " + tarinfo.linkname)
                if tarinfo.islnk():
                    _safe_print("link to " + tarinfo.linkname)
            print()

    def add(self, name, arcname=None, recursive=True, *, filter=None):
        """Add the file *name* (directories recursively) under *arcname*.
        *filter* may modify each TarInfo or return None to skip it."""
        self._ensure("awx")

        if arcname is None:
            arcname = name

        if self.name is not None and os.path.abspath(name) == self.name:
            self._debug_msg(2, "tarfile: Skipped %r" % name)
            return

        self._debug_msg(1, name)

        tarinfo = self.gettarinfo(name, arcname)
        if tarinfo is None:
            self._debug_msg(1, "tarfile: Unsupported type %r" % name)
            return

        if filter is not None:
            tarinfo = filter(tarinfo)
            if tarinfo is None:
                self._debug_msg(2, "tarfile: Excluded %r" % name)
                return

        if tarinfo.isreg():
            with bltn_open(name, "rb") as f:
                self.addfile(tarinfo, f)
        elif tarinfo.isdir():
            self.addfile(tarinfo)
            if recursive:
                for f in sorted(os.listdir(name)):
                    self.add(os.path.join(name, f), os.path.join(arcname, f),
                             recursive, filter=filter)
        else:
            self.addfile(tarinfo)

    def addfile(self, tarinfo, fileobj=None):
        """Write *tarinfo*'s header and, for regular files, tarinfo.size
        bytes read from *fileobj*."""
        self._ensure("awx")

        if fileobj is None and tarinfo.isreg() and tarinfo.size != 0:
            raise ValueError("fileobj not provided for non zero-size regular file")

        import copy
        tarinfo = copy.copy(tarinfo)

        buf = tarinfo.tobuf(self.format, self.encoding, self.errors)
        self.fileobj.write(buf)
        self.offset += len(buf)
        bufsize = self.copybufsize
        if fileobj is not None:
            copyfileobj(fileobj, self.fileobj, tarinfo.size, bufsize=bufsize)
            blocks, remainder = divmod(tarinfo.size, BLOCKSIZE)
            if remainder > 0:
                self.fileobj.write(NUL * (BLOCKSIZE - remainder))
                blocks += 1
            self.offset += blocks * BLOCKSIZE

        self.members.append(tarinfo)

    # -- extraction

    def _resolve_filter(self, filter):
        if filter is None:
            filter = self.extraction_filter
            if filter is None:
                return data_filter
            if isinstance(filter, str):
                raise TypeError(
                    'String names are not supported for '
                    + 'TarFile.extraction_filter. Use a function such as '
                    + 'tarfile.data_filter directly.')
            return filter
        if callable(filter):
            return filter
        try:
            return _NAMED_FILTERS[filter]
        except KeyError:
            raise ValueError(f"filter {filter!r} not found") from None

    def extractall(self, path=".", members=None, *, numeric_owner=False,
                   filter=None):
        """Extract all (or the given) members into *path*, applying the
        extraction *filter* (default: data_filter)."""
        directories = []

        filter_function = self._resolve_filter(filter)
        if members is None:
            members = self

        for member in members:
            tarinfo, unfiltered = self._prepare_member(
                member, filter_function, path)
            if tarinfo is None:
                continue
            if tarinfo.isdir():
                # Directory attributes are applied last, after the content
                # has been written.
                directories.append(unfiltered)
            self._extract_checked(tarinfo, path, set_attrs=not tarinfo.isdir(),
                              numeric_owner=numeric_owner,
                              filter_function=filter_function)

        directories.sort(key=lambda a: a.name, reverse=True)

        for unfiltered in directories:
            try:
                tarinfo = filter_function(unfiltered, path)
            except FilterError as e:
                self._skip_dir_fixup(unfiltered, repr(e))
                continue
            if tarinfo is None:
                self._skip_dir_fixup(unfiltered,
                                             'excluded by filter')
                continue
            dirpath = os.path.join(path, tarinfo.name)
            try:
                lstat = os.lstat(dirpath)
            except FileNotFoundError:
                self._skip_dir_fixup(tarinfo, 'missing')
                continue
            if not stat.S_ISDIR(lstat.st_mode):
                self._skip_dir_fixup(tarinfo, 'not a directory')
                continue
            try:
                self.chown(tarinfo, dirpath, numeric_owner=numeric_owner)
                self.utime(tarinfo, dirpath)
                self.chmod(tarinfo, dirpath)
            except ExtractError as e:
                self._soft_error(e)

    def _skip_dir_fixup(self, member, reason):
        self._debug_msg(2, "tarfile: Not fixing up directory %r (%s)" %
                  (member.name, reason))

    def extract(self, member, path="", set_attrs=True, *, numeric_owner=False,
                filter=None):
        """Extract one member (a name or TarInfo) into *path*."""
        filter_function = self._resolve_filter(filter)
        tarinfo, unfiltered = self._prepare_member(
            member, filter_function, path)
        if tarinfo is not None:
            self._extract_checked(tarinfo, path, set_attrs, numeric_owner,
                              filter_function=filter_function)

    def _prepare_member(self, member, filter_function, path):
        if isinstance(member, str):
            unfiltered = self.getmember(member)
        else:
            unfiltered = member

        filtered = None
        try:
            filtered = filter_function(unfiltered, path)
        except (OSError, UnicodeEncodeError, FilterError) as e:
            self._hard_error(e)
        except ExtractError as e:
            self._soft_error(e)
        if filtered is None:
            self._debug_msg(2, "tarfile: Excluded %r" % unfiltered.name)
            return None, None

        # Hard links are resolved against the archive's member names.
        if filtered.islnk():
            filtered = filtered.replace(deep=False)
            filtered._link_target = os.path.join(path, filtered.linkname)
        return filtered, unfiltered

    def _extract_checked(self, tarinfo, path, set_attrs, numeric_owner,
                     filter_function=None):
        self._ensure("r")
        try:
            self._write_member(tarinfo, os.path.join(path, tarinfo.name),
                                 set_attrs=set_attrs,
                                 numeric_owner=numeric_owner,
                                 filter_function=filter_function,
                                 extraction_root=path)
        except (OSError, UnicodeEncodeError) as e:
            self._hard_error(e)
        except ExtractError as e:
            self._soft_error(e)

    def _soft_error(self, e):
        if self.errorlevel > 1:
            raise
        self._debug_msg(1, "tarfile: %s" % e)

    def _hard_error(self, e):
        if self.errorlevel > 0:
            raise
        if isinstance(e, OSError):
            if e.filename is None:
                self._debug_msg(1, "tarfile: %s" % e.strerror)
            else:
                self._debug_msg(1, "tarfile: %s %r" % (e.strerror, e.filename))
        else:
            self._debug_msg(1, "tarfile: %s %s" % (type(e).__name__, e))

    def extractfile(self, member):
        """A file object (io.BufferedReader) for a regular member or a link
        to one; None for other types."""
        self._ensure("r")

        if isinstance(member, str):
            tarinfo = self.getmember(member)
        else:
            tarinfo = member

        if tarinfo.isreg() or tarinfo.type not in SUPPORTED_TYPES:
            return self.fileobject(self, tarinfo)
        if tarinfo.islnk() or tarinfo.issym():
            if isinstance(self.fileobj, _CompressedStream):
                raise StreamError("cannot extract (sym)link as file object")
            return self.extractfile(self._link_source(tarinfo))
        return None

    def _write_member(self, tarinfo, targetpath, set_attrs=True,
                        numeric_owner=False, *, filter_function=None,
                        extraction_root=None):
        targetpath = targetpath.rstrip("/")
        targetpath = targetpath.replace("/", os.sep)

        upperdirs = os.path.dirname(targetpath)
        if upperdirs and not os.path.exists(upperdirs):
            os.makedirs(upperdirs, exist_ok=True)

        if tarinfo.islnk() or tarinfo.issym():
            self._debug_msg(1, "%s -> %s" % (tarinfo.name, tarinfo.linkname))
        else:
            self._debug_msg(1, tarinfo.name)

        if tarinfo.isreg():
            self.makefile(tarinfo, targetpath)
        elif tarinfo.isdir():
            self.makedir(tarinfo, targetpath)
        elif tarinfo.isfifo():
            self.makefifo(tarinfo, targetpath)
        elif tarinfo.ischr() or tarinfo.isblk():
            self.makedev(tarinfo, targetpath)
        elif tarinfo.islnk() or tarinfo.issym():
            self._make_link_filtered(tarinfo, targetpath, filter_function,
                                      extraction_root)
        elif tarinfo.type not in SUPPORTED_TYPES:
            self.makeunknown(tarinfo, targetpath)
        else:
            self.makefile(tarinfo, targetpath)

        if set_attrs:
            self.chown(tarinfo, targetpath, numeric_owner)
            if not tarinfo.issym():
                self.chmod(tarinfo, targetpath)
                self.utime(tarinfo, targetpath)

    def makedir(self, tarinfo, targetpath):
        """Create a directory."""
        try:
            if tarinfo.mode is None:
                os.mkdir(targetpath)
            else:
                # Created accessible to us; final permissions come later.
                os.mkdir(targetpath, 0o700)
        except FileExistsError:
            if not os.path.isdir(targetpath):
                raise

    def makefile(self, tarinfo, targetpath):
        """Create a regular file from the member's data."""
        source = self.fileobj
        source.seek(tarinfo.offset_data)
        bufsize = self.copybufsize
        with bltn_open(targetpath, "wb") as target:
            if tarinfo.sparse is not None:
                for offset, size in tarinfo.sparse:
                    target.seek(offset)
                    copyfileobj(source, target, size, ReadError, bufsize)
                target.seek(tarinfo.size)
                target.truncate()
            else:
                copyfileobj(source, target, tarinfo.size, ReadError, bufsize)

    def makeunknown(self, tarinfo, targetpath):
        """Extract an unknown member type as a regular file."""
        self.makefile(tarinfo, targetpath)
        self._debug_msg(1, "tarfile: Unknown file type %r, extracted as regular file."
                  % tarinfo.type)

    def makefifo(self, tarinfo, targetpath):
        """Create a FIFO."""
        if hasattr(os, "mkfifo"):
            os.mkfifo(targetpath)
        else:
            raise ExtractError("fifo not supported by system")

    def makedev(self, tarinfo, targetpath):
        """Create a character or block device node."""
        if not hasattr(os, "mknod") or not hasattr(os, "makedev"):
            raise ExtractError("special devices not supported by system")
        mode = tarinfo.mode
        if mode is None:
            mode = 0o600
        if tarinfo.isblk():
            mode |= stat.S_IFBLK
        else:
            mode |= stat.S_IFCHR
        os.mknod(targetpath, mode,
                 os.makedev(tarinfo.devmajor, tarinfo.devminor))

    def makelink(self, tarinfo, targetpath):
        return self._make_link_filtered(tarinfo, targetpath, None, None)

    def _make_link_filtered(self, tarinfo, targetpath, filter_function,
                             extraction_root):
        """Create a symbolic or hard link; fall back to copying the target
        member where links are unavailable."""
        keyerror_to_extracterror = False
        try:
            if tarinfo.issym():
                if os.path.lexists(targetpath):
                    os.unlink(targetpath)
                os.symlink(tarinfo.linkname, targetpath)
                return
            else:
                if os.path.exists(tarinfo._link_target):
                    if os.path.lexists(targetpath):
                        os.unlink(targetpath)
                    os.link(tarinfo._link_target, targetpath)
                    return
        except symlink_exception:
            keyerror_to_extracterror = True

        try:
            unfiltered = self._link_source(tarinfo)
        except KeyError:
            if keyerror_to_extracterror:
                raise ExtractError(
                    "unable to resolve link inside archive") from None
            else:
                raise

        if filter_function is None:
            filtered = unfiltered
        else:
            if extraction_root is None:
                raise ExtractError(
                    "_make_link_filtered: if filter_function is not None, "
                    + "extraction_root must also not be None")
            try:
                filtered = filter_function(unfiltered, extraction_root)
                if filtered is not None and not unfiltered.isreg():
                    # The copy lands at the link's location, where e.g. a
                    # relative symlink means something else: check it there.
                    relocated = unfiltered.replace(name=tarinfo.name,
                                                   deep=False)
                    if filter_function(relocated, extraction_root) is None:
                        filtered = None
            except FilterError as cause:
                raise LinkFallbackError(tarinfo, unfiltered.name) from cause
        if filtered is not None:
            self._write_member(filtered, targetpath,
                                 filter_function=filter_function,
                                 extraction_root=extraction_root)

    def chown(self, tarinfo, targetpath, numeric_owner):
        """Set owner and group (only when running as root)."""
        if hasattr(os, "geteuid") and os.geteuid() == 0:
            g = tarinfo.gid
            u = tarinfo.uid
            if not numeric_owner:
                try:
                    if grp and tarinfo.gname:
                        g = grp.getgrnam(tarinfo.gname)[2]
                except KeyError:
                    pass
                try:
                    if pwd and tarinfo.uname:
                        u = pwd.getpwnam(tarinfo.uname)[2]
                except KeyError:
                    pass
            if g is None:
                g = -1
            if u is None:
                u = -1
            try:
                if tarinfo.issym() and hasattr(os, "lchown"):
                    os.lchown(targetpath, u, g)
                else:
                    os.chown(targetpath, u, g)
            except (OSError, OverflowError) as e:
                raise ExtractError("could not change owner") from e

    def chmod(self, tarinfo, targetpath):
        """Apply the member's permission bits."""
        if tarinfo.mode is None:
            return
        try:
            os.chmod(targetpath, tarinfo.mode)
        except OSError as e:
            raise ExtractError("could not change mode") from e

    def utime(self, tarinfo, targetpath):
        """Apply the member's modification time."""
        mtime = tarinfo.mtime
        if mtime is None:
            return
        if not hasattr(os, 'utime'):
            return
        try:
            os.utime(targetpath, (mtime, mtime))
        except OSError as e:
            raise ExtractError("could not change modification time") from e

    # -- iteration

    def next(self):
        """Return the next member (None at the end of the archive)."""
        self._ensure("ra")
        if self.firstmember is not None:
            m = self.firstmember
            self.firstmember = None
            return m

        if self.offset != self.fileobj.tell():
            if self.offset == 0:
                return None
            self.fileobj.seek(self.offset - 1)
            if not self.fileobj.read(1):
                raise ReadError("unexpected end of data")

        tarinfo = None
        while True:
            try:
                tarinfo = self.tarinfo.fromtarfile(self)
            except EOFHeaderError as e:
                if self.ignore_zeros:
                    self._debug_msg(2, "0x%X: %s" % (self.offset, e))
                    self.offset += BLOCKSIZE
                    continue
            except InvalidHeaderError as e:
                if self.ignore_zeros:
                    self._debug_msg(2, "0x%X: %s" % (self.offset, e))
                    self.offset += BLOCKSIZE
                    continue
                elif self.offset == 0:
                    raise ReadError(str(e)) from None
            except EmptyHeaderError:
                if self.offset == 0:
                    raise ReadError("empty file") from None
            except TruncatedHeaderError as e:
                if self.offset == 0:
                    raise ReadError(str(e)) from None
            except SubsequentHeaderError as e:
                raise ReadError(str(e)) from None
            except Exception as e:
                try:
                    import zlib
                    if isinstance(e, zlib.error):
                        raise ReadError(f'zlib error: {e}') from None
                except ImportError:
                    pass
                raise e
            break

        if tarinfo is not None:
            if not self.stream:
                self.members.append(tarinfo)
        else:
            self._loaded = True
        return tarinfo

    def _lookup(self, name, tarinfo=None, normalize=False):
        members = self.getmembers()
        skipping = False
        if tarinfo is not None:
            try:
                index = members.index(tarinfo)
            except ValueError:
                skipping = True
            else:
                members = members[:index]
        if normalize:
            name = os.path.normpath(name)
        for member in reversed(members):
            if skipping:
                if tarinfo.offset == member.offset:
                    skipping = False
                continue
            if normalize:
                member_name = os.path.normpath(member.name)
            else:
                member_name = member.name
            if name == member_name:
                return member
        if skipping:
            raise ValueError(tarinfo)
        return None

    def _read_all(self):
        """Read all remaining members."""
        if not self.stream:
            while self.next() is not None:
                pass
            self._loaded = True

    def _ensure(self, mode=None):
        if self.closed:
            raise OSError("%s is closed" % self.__class__.__name__)
        if mode is not None and self.mode not in mode:
            raise OSError("bad operation for mode %r" % self.mode)

    def _link_source(self, tarinfo):
        if tarinfo.issym():
            linkname = "/".join(filter(None, (os.path.dirname(tarinfo.name),
                                              tarinfo.linkname)))
            limit = None
        else:
            linkname = tarinfo.linkname
            limit = tarinfo
        member = self._lookup(linkname, tarinfo=limit, normalize=True)
        if member is None:
            raise KeyError("linkname %r not found" % linkname)
        return member

    def __iter__(self):
        if self._loaded:
            yield from self.members
            return
        index = 0
        if self.firstmember is not None:
            tarinfo = self.next()
            index += 1
            yield tarinfo
        while True:
            if index < len(self.members):
                tarinfo = self.members[index]
            elif not self._loaded:
                tarinfo = self.next()
                if not tarinfo:
                    self._loaded = True
                    return
            else:
                return
            index += 1
            yield tarinfo

    def _debug_msg(self, level, msg):
        if level <= self.debug:
            print(msg, file=sys.stderr)

    def __enter__(self):
        self._ensure()
        return self

    def __exit__(self, type, value, traceback):
        if type is None:
            self.close()
        else:
            # On error, do not write the end-of-archive blocks.
            if not self._extfileobj:
                self.fileobj.close()
            self.closed = True


# ---------------------------------------------------------------- functions

def is_tarfile(name):
    """True if *name* (a path or file object) looks like a tar archive
    that this module can read."""
    try:
        if hasattr(name, "read"):
            pos = name.tell()
            t = open(fileobj=name)
            name.seek(pos)
        else:
            t = open(name)
        t.close()
        return True
    except TarError:
        return False


open = TarFile.open
import builtins as _builtins
bltn_open = _builtins.open


def main():
    """Command-line interface: python -m tarfile ..."""
    import argparse

    description = 'List, test, extract or create tar archives.'
    parser = argparse.ArgumentParser(description=description, color=True)
    parser.add_argument('-v', '--verbose', action='store_true', default=False,
                        help='print what is being done')
    parser.add_argument('--filter', metavar='<filtername>',
                        choices=_NAMED_FILTERS,
                        help='extraction filter: fully_trusted, tar or data')

    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument('-l', '--list', metavar='<tarfile>',
                       help='list the members of an archive')
    group.add_argument('-e', '--extract', nargs='+',
                       metavar=('<tarfile>', '<output_dir>'),
                       help='extract an archive (into the given directory)')
    group.add_argument('-c', '--create', nargs='+',
                       metavar=('<name>', '<file>'),
                       help='create an archive from files and directories')
    group.add_argument('-t', '--test', metavar='<tarfile>',
                       help='check that an archive can be read')

    args = parser.parse_args()

    if args.filter and args.extract is None:
        parser.exit(1, '--filter is only valid for extraction\n')

    if args.test is not None:
        src = args.test
        if is_tarfile(src):
            with open(src, 'r') as tar:
                tar.getmembers()
                print(tar.getmembers(), file=sys.stderr)
            if args.verbose:
                print('{!r} is a tar archive.'.format(src))
        else:
            parser.exit(1, '{!r} is not a tar archive.\n'.format(src))

    elif args.list is not None:
        src = args.list
        if is_tarfile(src):
            with TarFile.open(src, 'r:*') as tf:
                tf.list(verbose=args.verbose)
        else:
            parser.exit(1, '{!r} is not a tar archive.\n'.format(src))

    elif args.extract is not None:
        if len(args.extract) == 1:
            src = args.extract[0]
            curdir = os.curdir
        elif len(args.extract) == 2:
            src, curdir = args.extract
        else:
            parser.exit(1, parser.format_help())

        if is_tarfile(src):
            with TarFile.open(src, 'r:*') as tf:
                tf.extractall(path=curdir, filter=args.filter)
            if args.verbose:
                if curdir == '.':
                    msg = '{!r} file is extracted.'.format(src)
                else:
                    msg = ('{!r} file is extracted '
                           'into {!r} directory.').format(src, curdir)
                print(msg)
        else:
            parser.exit(1, '{!r} is not a tar archive.\n'.format(src))

    elif args.create is not None:
        tar_name = args.create.pop(0)
        _, ext = os.path.splitext(tar_name)
        compressions = {
            '.gz': 'gz', '.tgz': 'gz',
            '.xz': 'xz', '.txz': 'xz',
            '.bz2': 'bz2', '.tbz': 'bz2', '.tbz2': 'bz2', '.tb2': 'bz2',
            '.zst': 'zst', '.tzst': 'zst',
        }
        tar_mode = 'w:' + compressions[ext] if ext in compressions else 'w'
        tar_files = args.create

        with TarFile.open(tar_name, tar_mode) as tf:
            for file_name in tar_files:
                tf.add(file_name)

        if args.verbose:
            print('{!r} file created.'.format(tar_name))


if __name__ == '__main__':
    main()
