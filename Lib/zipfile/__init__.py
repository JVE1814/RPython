"""Read and write ZIP archives (PKWARE APPNOTE format, including ZIP64).

ZipFile is the archive object, ZipInfo describes one member, Path offers a
pathlib-like view.  Supported member compressions: stored, deflated and, when
the corresponding modules exist, bzip2, LZMA and Zstandard.
"""

import binascii
import importlib.util
import io
import os
import shutil
import stat
import struct
import sys
import threading
import time

try:
    import zlib
    crc32 = zlib.crc32
except ImportError:
    zlib = None
    crc32 = binascii.crc32

try:
    import bz2
except ImportError:
    bz2 = None

try:
    import lzma
except ImportError:
    lzma = None

try:
    from compression import zstd
except ImportError:
    zstd = None

__all__ = ["BadZipFile", "BadZipfile", "error",
           "ZIP_STORED", "ZIP_DEFLATED", "ZIP_BZIP2", "ZIP_LZMA",
           "ZIP_ZSTANDARD", "is_zipfile", "ZipInfo", "ZipFile", "PyZipFile",
           "LargeZipFile", "Path"]


class BadZipFile(Exception):
    pass


class LargeZipFile(Exception):
    """Raised when writing a file that would need ZIP64 extensions while
    allowZip64 is false."""


error = BadZipfile = BadZipFile      # historical aliases

ZIP64_LIMIT = (1 << 31) - 1
ZIP_FILECOUNT_LIMIT = (1 << 16) - 1
ZIP_MAX_COMMENT = (1 << 16) - 1

# compression method numbers
ZIP_STORED = 0
ZIP_DEFLATED = 8
ZIP_BZIP2 = 12
ZIP_LZMA = 14
ZIP_ZSTANDARD = 93

# "version needed to extract" values
DEFAULT_VERSION = 20
ZIP64_VERSION = 45
BZIP2_VERSION = 46
LZMA_VERSION = 63
ZSTANDARD_VERSION = 63
# Version needed to extract, by compression method.
_METHOD_MIN_VERSION = {12: BZIP2_VERSION, 14: LZMA_VERSION, 93: ZSTANDARD_VERSION}
MAX_EXTRACT_VERSION = 63

# ---------------------------------------------------------------- record layouts
# end of central directory
structEndArchive = b"<4s4H2LH"
stringEndArchive = b"PK\005\006"
sizeEndCentDir = struct.calcsize(structEndArchive)
_END_SIGNATURE, _END_DISK_NUMBER, _END_DISK_START, _END_ENTRIES_THIS_DISK, \
    _END_ENTRIES_TOTAL, _END_SIZE, _END_OFFSET, _END_COMMENT_SIZE = range(8)
_END_COMMENT = 8
_END_LOCATION = 9

# central directory file header
structCentralDir = "<4s4B4HL2L5H2L"
stringCentralDir = b"PK\001\002"
sizeCentralDir = struct.calcsize(structCentralDir)
(_CEN_SIGNATURE, _CEN_CREATE_VERSION, _CEN_CREATE_SYSTEM, _CEN_EXTRACT_VERSION,
 _CEN_EXTRACT_SYSTEM, _CEN_FLAG_BITS, _CEN_COMPRESS_TYPE, _CEN_TIME, _CEN_DATE,
 _CEN_CRC, _CEN_COMPRESSED_SIZE, _CEN_UNCOMPRESSED_SIZE, _CEN_FILENAME_LENGTH,
 _CEN_EXTRA_FIELD_LENGTH, _CEN_COMMENT_LENGTH, _CEN_DISK_NUMBER_START,
 _CEN_INTERNAL_FILE_ATTRIBUTES, _CEN_EXTERNAL_FILE_ATTRIBUTES,
 _CEN_LOCAL_HEADER_OFFSET) = range(19)

# local file header
structFileHeader = "<4s2B4HL2L2H"
stringFileHeader = b"PK\003\004"
sizeFileHeader = struct.calcsize(structFileHeader)
(_LOC_SIGNATURE, _LOC_EXTRACT_VERSION, _LOC_EXTRACT_SYSTEM,
 _LOC_GENERAL_PURPOSE_FLAG_BITS, _LOC_COMPRESSION_METHOD, _LOC_LAST_MOD_TIME,
 _LOC_LAST_MOD_DATE, _LOC_CRC, _LOC_COMPRESSED_SIZE, _LOC_UNCOMPRESSED_SIZE,
 _LOC_FILENAME_LENGTH, _LOC_EXTRA_FIELD_LENGTH) = range(12)

# ZIP64 end of central directory locator and record
structEndArchive64Locator = "<4sLQL"
stringEndArchive64Locator = b"PK\x06\x07"
sizeEndCentDir64Locator = struct.calcsize(structEndArchive64Locator)

structEndArchive64 = "<4sQ2H2L4Q"
stringEndArchive64 = b"PK\x06\x06"
sizeEndCentDir64 = struct.calcsize(structEndArchive64)
(_END64_SIGNATURE, _END64_DIRECTORY_RECSIZE, _END64_CREATE_VERSION,
 _END64_EXTRACT_VERSION, _END64_DISK_NUMBER, _END64_DISK_NUMBER_START,
 _END64_NUMBER_ENTRIES_THIS_DISK, _END64_NUMBER_ENTRIES_TOTAL,
 _END64_DIRECTORY_SIZE, _END64_OFFSET_START_CENTDIR) = range(10)

_DD_SIGNATURE = 0x08074b50

# general purpose flag bits
_MASK_ENCRYPTED = 1 << 0
_MASK_COMPRESS_OPTION_1 = 1 << 1
_MASK_USE_DATA_DESCRIPTOR = 1 << 3
_MASK_COMPRESSED_PATCH = 1 << 5
_MASK_STRONG_ENCRYPTION = 1 << 6
_MASK_UTF_FILENAME = 1 << 11

_EXTRA_ZIP64 = 0x0001


class _Extra(bytes):
    """Helpers for the (id, size, data) records of an extra field."""

    FIELD_STRUCT = struct.Struct('<HH')

    def __new__(cls, val, id=None):
        return super().__new__(cls, val)

    def __init__(self, val, id=None):
        self.id = id

    @classmethod
    def read_one(cls, raw):
        try:
            xid, xlen = cls.FIELD_STRUCT.unpack(raw[:4])
        except struct.error:
            xid = None
            xlen = 0
        return cls(raw[:4 + xlen], xid), raw[4 + xlen:]

    @classmethod
    def split(cls, data):
        while data:
            field, data = cls.read_one(data)
            yield field

    @classmethod
    def strip(cls, data, xids):
        """Remove the extra records whose id is in *xids*."""
        return b''.join(f for f in cls.split(data) if f.id not in xids)


def _without_extra_ids(extra, xids):
    return _Extra.strip(extra, xids)


def _looks_like_zip(fp):
    """True if fp ends with a plausible end-of-central-directory record:
    one describing an empty archive, or one whose central directory
    starts with the central directory signature."""
    try:
        end = _find_end_record(fp)
    except (OSError, BadZipFile):
        return False
    if not end:
        return False
    size, offset = end[_END_SIZE], end[_END_OFFSET]
    if end[_END_ENTRIES_TOTAL] == 0 and size == 0 and offset == 0:
        return True
    directory = end[_END_LOCATION] - size
    if not size or directory < 0:
        return False
    try:
        fp.seek(directory)
        return fp.read(4) == stringCentralDir
    except OSError:
        return False


def is_zipfile(filename):
    """Quickly see if a file is a ZIP file by checking the magic number.

    The filename argument may be a file or file-like object too.
    """
    try:
        if hasattr(filename, "read"):
            pos = filename.tell()
            try:
                return _looks_like_zip(fp=filename)
            finally:
                filename.seek(pos)
        with open(filename, "rb") as fp:
            return _looks_like_zip(fp)
    except OSError:
        return False


def _merge_zip64_end(fpin, offset, endrec):
    """Merge the ZIP64 end-of-central-directory data into *endrec*.

    *offset* is the position of the regular end record relative to the end
    of the file.
    """
    try:
        fpin.seek(offset - sizeEndCentDir64Locator, 2)
    except OSError:
        return endrec
    data = fpin.read(sizeEndCentDir64Locator)
    if len(data) != sizeEndCentDir64Locator:
        return endrec
    sig, diskno, reloff, disks = struct.unpack(structEndArchive64Locator, data)
    if sig != stringEndArchive64Locator:
        return endrec
    if diskno != 0 or disks > 1:
        raise BadZipFile("zipfiles that span multiple disks are not supported")

    locator_pos = fpin.tell() - sizeEndCentDir64Locator
    if reloff > locator_pos - sizeEndCentDir64:
        raise BadZipFile("Corrupt zip64 end of central directory locator")
    def record_at(pos):
        fpin.seek(pos)
        head = fpin.read(sizeEndCentDir64)
        if len(head) != sizeEndCentDir64:
            raise BadZipFile("Corrupt zip64 end of central directory record")
        return struct.unpack(structEndArchive64, head)

    # Normally the record is where the locator says (it may carry
    # extensible data).  If data was prepended to the archive the stored
    # offset is too small; then the record (without extensible data) sits
    # right before the locator and must follow the central directory.
    record_pos = reloff
    fields = record_at(record_pos)
    if fields[0] != stringEndArchive64 or fields[1] + 12 != locator_pos - reloff:
        record_pos = locator_pos - sizeEndCentDir64
        fields = record_at(record_pos)
        if fields[0] != stringEndArchive64:
            raise BadZipFile("Zip64 end of central directory record not found")
        if (fields[1] + 12 != sizeEndCentDir64
                or fields[9] + fields[8] != reloff):
            raise BadZipFile("Corrupt zip64 end of central directory record")
    (sig, sz, create_version, read_version, disk_num, disk_dir,
     dircount, dircount2, dirsize, diroffset) = fields
    if diskno != 0 or disk_dir != 0 or dircount != dircount2:
        raise BadZipFile("zipfiles that span multiple disks are not supported")

    endrec[_END_SIGNATURE] = sig
    endrec[_END_DISK_NUMBER] = disk_num
    endrec[_END_DISK_START] = disk_dir
    endrec[_END_ENTRIES_THIS_DISK] = dircount
    endrec[_END_ENTRIES_TOTAL] = dircount2
    endrec[_END_SIZE] = dirsize
    endrec[_END_OFFSET] = diroffset
    endrec[_END_LOCATION] = record_pos
    return endrec


def _find_end_record(fpin):
    """Locate the end-of-central-directory record.

    Returns a list [signature, disk, disk_cd, entries_disk, entries, cd_size,
    cd_offset, comment_size, comment, location] or None.
    """
    fpin.seek(0, 2)
    filesize = fpin.tell()

    def finish(data, pos, comment):
        endrec = list(struct.unpack(structEndArchive, data))
        endrec.append(comment)
        endrec.append(pos)
        return _merge_zip64_end(fpin, pos - filesize, endrec)

    # Fast path: no archive comment.
    try:
        fpin.seek(-sizeEndCentDir, 2)
    except OSError:
        return None
    data = fpin.read(sizeEndCentDir)
    if (len(data) == sizeEndCentDir and data[:4] == stringEndArchive
            and data[-2:] == b"\000\000"):
        return finish(data, filesize - sizeEndCentDir, b"")

    # Search the trailing 64 KiB (+ record size) for the signature.
    start = max(filesize - ZIP_MAX_COMMENT - sizeEndCentDir, 0)
    fpin.seek(start, 0)
    tail = fpin.read()
    i = tail.rfind(stringEndArchive)
    if i >= 0:
        rec = tail[i:i + sizeEndCentDir]
        if len(rec) != sizeEndCentDir:
            return None
        comment_len = struct.unpack(structEndArchive, rec)[_END_COMMENT_SIZE]
        comment = tail[i + sizeEndCentDir:i + sizeEndCentDir + comment_len]
        return finish(rec, start + i, comment)
    return None


def _clean_member_name(filename):
    """Terminate at NUL and use '/' as the separator."""
    nul = filename.find(chr(0))
    if nul >= 0:
        filename = filename[:nul]
    if os.sep != "/" and os.sep in filename:
        filename = filename.replace(os.sep, "/")
    if os.altsep and os.altsep != "/" and os.altsep in filename:
        filename = filename.replace(os.altsep, "/")
    return filename


class ZipInfo:
    """Metadata of one archive member."""

    __slots__ = (
        'orig_filename', 'filename', 'date_time', 'compress_type',
        'compress_level', 'comment', 'extra', 'create_system',
        'create_version', 'extract_version', 'reserved', 'flag_bits',
        'volume', 'internal_attr', 'external_attr', 'header_offset', 'CRC',
        'compress_size', 'file_size', '_raw_time', '_end_offset', '_stored_name',
    )

    def __init__(self, filename="NoName", date_time=(1980, 1, 1, 0, 0, 0)):
        self.orig_filename = filename
        filename = _clean_member_name(filename)
        self.filename = filename
        self.date_time = date_time
        if date_time[0] < 1980:
            raise ValueError('ZIP does not support timestamps before 1980')
        self.compress_type = ZIP_STORED
        self.compress_level = None
        self.comment = b""
        self.extra = b""
        self.create_system = 0 if sys.platform == 'win32' else 3
        self.create_version = DEFAULT_VERSION
        self.extract_version = DEFAULT_VERSION
        self.reserved = 0
        self.flag_bits = 0
        self.volume = 0
        self.internal_attr = 0
        self.external_attr = 0
        self.compress_size = 0
        self.file_size = 0
        self._end_offset = None
        # (encoded name, decoded name) as found in an archive being read
        self._stored_name = None
        self.header_offset = 0
        self.CRC = 0

    # compatibility alias
    @property
    def _compresslevel(self):
        return self.compress_level

    @_compresslevel.setter
    def _compresslevel(self, value):
        self.compress_level = value

    def __repr__(self):
        parts = [f'<{self.__class__.__name__} filename={self.filename!r}']
        if self.compress_type != ZIP_STORED:
            parts.append(' compress_type=%s' % compressor_names.get(
                self.compress_type, self.compress_type))
        hi = self.external_attr >> 16
        lo = self.external_attr & 0xFFFF
        if hi:
            parts.append(' filemode=%r' % stat.filemode(hi))
        if lo:
            parts.append(' external_attr=%#x' % lo)
        isdir = self.is_dir()
        if not isdir or self.file_size:
            parts.append(' file_size=%r' % self.file_size)
        if ((not isdir or self.compress_size) and
                (self.compress_type != ZIP_STORED or
                 self.file_size != self.compress_size)):
            parts.append(' compress_size=%r' % self.compress_size)
        parts.append('>')
        return ''.join(parts)

    def FileHeader(self, zip64=None):
        """Return the local file header bytes for this member."""
        dt = self.date_time
        dosdate = (dt[0] - 1980) << 9 | dt[1] << 5 | dt[2]
        dostime = dt[3] << 11 | dt[4] << 5 | (dt[5] // 2)
        if self.flag_bits & _MASK_USE_DATA_DESCRIPTOR:
            CRC = compress_size = file_size = 0
        else:
            CRC = self.CRC
            compress_size = self.compress_size
            file_size = self.file_size

        extra = self.extra
        min_version = 0
        if zip64 is None:
            zip64 = file_size > ZIP64_LIMIT or compress_size > ZIP64_LIMIT
        if zip64:
            extra = extra + struct.pack('<HHQQ', _EXTRA_ZIP64, 16,
                                        file_size, compress_size)
            file_size = 0xffffffff
            compress_size = 0xffffffff
            min_version = ZIP64_VERSION

        if self.compress_type == ZIP_BZIP2:
            min_version = max(BZIP2_VERSION, min_version)
        elif self.compress_type == ZIP_LZMA:
            min_version = max(LZMA_VERSION, min_version)
        elif self.compress_type == ZIP_ZSTANDARD:
            min_version = max(ZSTANDARD_VERSION, min_version)

        self.extract_version = max(min_version, self.extract_version)
        self.create_version = max(min_version, self.create_version)
        filename, flag_bits = self._encodeFilenameFlags()
        header = struct.pack(structFileHeader, stringFileHeader,
                             self.extract_version, self.reserved, flag_bits,
                             self.compress_type, dostime, dosdate, CRC,
                             compress_size, file_size,
                             len(filename), len(extra))
        return header + filename + extra

    def _encodeFilenameFlags(self):
        stored = self._stored_name
        if (stored is not None and stored[1] == self.filename
                and not self.flag_bits & _MASK_UTF_FILENAME):
            # A legacy (non-UTF-8) name read from an archive is written back
            # byte for byte, so that rewriting the directory keeps it intact.
            return stored[0], self.flag_bits
        try:
            return self.filename.encode('ascii'), self.flag_bits
        except UnicodeEncodeError:
            return (self.filename.encode('utf-8'),
                    self.flag_bits | _MASK_UTF_FILENAME)

    def _decodeExtra(self, filename_crc):
        extra = self.extra
        while len(extra) >= 4:
            tp, ln = struct.unpack('<HH', extra[:4])
            if ln + 4 > len(extra):
                raise BadZipFile(f"Corrupt extra field {tp:04x} (size={ln})")
            if tp == _EXTRA_ZIP64:
                data = extra[4:ln + 4]
                try:
                    if self.file_size in (0xFFFF_FFFF_FFFF_FFFF, 0xFFFF_FFFF):
                        field = "File size"
                        self.file_size, = struct.unpack('<Q', data[:8])
                        data = data[8:]
                    if self.compress_size == 0xFFFF_FFFF:
                        field = "Compress size"
                        self.compress_size, = struct.unpack('<Q', data[:8])
                        data = data[8:]
                    if self.header_offset == 0xFFFF_FFFF:
                        field = "Header offset"
                        self.header_offset, = struct.unpack('<Q', data[:8])
                except struct.error:
                    raise BadZipFile(f"Corrupt zip64 extra field. "
                                     f"{field} not found.") from None
            elif tp == 0x7075:
                # Info-ZIP Unicode Path extra field
                data = extra[4:ln + 4]
                try:
                    up_version, up_name_crc = struct.unpack('<BL', data[:5])
                    if up_version == 1 and up_name_crc == filename_crc:
                        up_unicode_name = data[5:].decode('utf-8')
                        if up_unicode_name:
                            self.filename = _clean_member_name(up_unicode_name)
                        else:
                            import warnings
                            warnings.warn("Empty unicode path extra field "
                                          "(0x7075)", stacklevel=2)
                except struct.error as e:
                    raise BadZipFile("Corrupt unicode path extra field "
                                     "(0x7075)") from e
                except UnicodeDecodeError as e:
                    raise BadZipFile('Corrupt unicode path extra field '
                                     '(0x7075): invalid utf-8 bytes') from e
            extra = extra[ln + 4:]

    @classmethod
    def from_file(cls, filename, arcname=None, *, strict_timestamps=True):
        """Build a ZipInfo for a file on disk (archive name defaults to the
        path without drive and leading separators)."""
        if isinstance(filename, os.PathLike):
            filename = os.fspath(filename)
        st = os.stat(filename)
        isdir = stat.S_ISDIR(st.st_mode)
        mtime = time.localtime(st.st_mtime)
        date_time = mtime[0:6]
        if not strict_timestamps and date_time[0] < 1980:
            date_time = (1980, 1, 1, 0, 0, 0)
        elif not strict_timestamps and date_time[0] > 2107:
            date_time = (2107, 12, 31, 23, 59, 59)
        if arcname is None:
            arcname = filename
        arcname = os.path.normpath(os.path.splitdrive(arcname)[1])
        while arcname[0] in (os.sep, os.altsep):
            arcname = arcname[1:]
        if isdir:
            arcname += '/'
        zinfo = cls(arcname, date_time)
        zinfo.external_attr = (st.st_mode & 0xFFFF) << 16
        if isdir:
            zinfo.file_size = 0
            zinfo.external_attr |= 0x10   # MS-DOS directory flag
        else:
            zinfo.file_size = st.st_size
        return zinfo

    def is_dir(self):
        """True if this member is a directory."""
        return self.filename.endswith('/')

    def _for_archive(self, archive):
        """Fill in defaults (timestamp, compression, permissions) for a
        member about to be written to *archive*; returns self."""
        epoch = os.environ.get('SOURCE_DATE_EPOCH')
        if epoch:
            # Reproducible builds: a fixed timestamp, in UTC.
            self.date_time = time.gmtime(int(epoch))[:6]
        else:
            self.date_time = time.localtime(time.time())[:6]
        self.compress_type = archive.compression
        self.compress_level = archive.compresslevel
        if self.filename.endswith('/'):
            self.external_attr = (0o40775 << 16) | 0x10
        else:
            self.external_attr = 0o600 << 16
        return self


# ---------------------------------------------------------------- ZipCrypto

def _reflected_crc_table():
    # CRC-32 lookup table (reflected polynomial 0xEDB88320), one entry per
    # byte value
    table = []
    for value in range(256):
        for _ in range(8):
            value = (value >> 1) ^ (0xEDB88320 if value & 1 else 0)
        table.append(value)
    return table


_CRC_TABLE = None


class _ZipCryptoDecoder:
    """The traditional PKWARE stream cipher ("ZipCrypto").

    Three 32-bit keys start from fixed values and are updated with every
    plaintext byte, the password first.  Calling the object decrypts a
    chunk of data and keeps the state for the next chunk.
    """

    __slots__ = ('_keys', '_table')

    def __init__(self, password):
        global _CRC_TABLE
        if _CRC_TABLE is None:
            _CRC_TABLE = _reflected_crc_table()
        self._table = _CRC_TABLE
        self._keys = [0x12345678, 0x23456789, 0x34567890]
        for byte in password:
            self._mix(byte)

    def _mix(self, byte):
        table = self._table
        keys = self._keys
        first = (keys[0] >> 8) ^ table[(keys[0] ^ byte) & 0xFF]
        second = ((keys[1] + (first & 0xFF)) * 134775813 + 1) & 0xFFFFFFFF
        third = (keys[2] >> 8) ^ table[(keys[2] ^ (second >> 24)) & 0xFF]
        keys[0], keys[1], keys[2] = first, second, third

    def __call__(self, data):
        plain = bytearray()
        keep = plain.append
        table = self._table
        k0, k1, k2 = self._keys
        for byte in data:
            stream = k2 | 2
            byte ^= ((stream * (stream ^ 1)) >> 8) & 0xFF
            keep(byte)
            k0 = (k0 >> 8) ^ table[(k0 ^ byte) & 0xFF]
            k1 = ((k1 + (k0 & 0xFF)) * 134775813 + 1) & 0xFFFFFFFF
            k2 = (k2 >> 8) ^ table[(k2 ^ (k1 >> 24)) & 0xFF]
        self._keys[:] = (k0, k1, k2)
        return bytes(plain)


# ---------------------------------------------------------------- codecs

class LZMACompressor:
    """Raw LZMA1 stream with the 4-byte zip header (version + props size)."""

    def __init__(self):
        self._comp = None

    def _init(self):
        props = lzma._encode_filter_properties({'id': lzma.FILTER_LZMA1})
        self._comp = lzma.LZMACompressor(lzma.FORMAT_RAW, filters=[
            lzma._decode_filter_properties(lzma.FILTER_LZMA1, props)])
        return struct.pack('<BBH', 9, 4, len(props)) + props

    def compress(self, data):
        if self._comp is None:
            return self._init() + self._comp.compress(data)
        return self._comp.compress(data)

    def flush(self):
        if self._comp is None:
            return self._init() + self._comp.flush()
        return self._comp.flush()


class LZMADecompressor:

    def __init__(self):
        self._decomp = None
        self._unconsumed = b''
        self.eof = False

    @property
    def needs_input(self):
        return self._decomp is None or self._decomp.needs_input

    def decompress(self, data, max_length=-1):
        if self._decomp is None:
            self._unconsumed += data
            if len(self._unconsumed) <= 4:
                return b''
            psize, = struct.unpack('<H', self._unconsumed[2:4])
            if len(self._unconsumed) <= 4 + psize:
                return b''
            self._decomp = lzma.LZMADecompressor(lzma.FORMAT_RAW, filters=[
                lzma._decode_filter_properties(
                    lzma.FILTER_LZMA1, self._unconsumed[4:4 + psize])])
            data = self._unconsumed[4 + psize:]
            del self._unconsumed
        result = self._decomp.decompress(data, max_length)
        self.eof = self._decomp.eof
        return result


compressor_names = {
    0: 'store', 1: 'shrink', 2: 'reduce', 3: 'reduce', 4: 'reduce',
    5: 'reduce', 6: 'implode', 7: 'tokenize', 8: 'deflate',
    9: 'deflate64', 10: 'implode', 12: 'bzip2', 14: 'lzma', 18: 'terse',
    19: 'lz77', 93: 'zstd', 97: 'wavpack', 98: 'ppmd',
}


def _check_compression(compression):
    if compression == ZIP_STORED:
        return
    needed = {ZIP_DEFLATED: (zlib, "zlib"), ZIP_BZIP2: (bz2, "bz2"),
              ZIP_LZMA: (lzma, "lzma"), ZIP_ZSTANDARD: (zstd, "compression.zstd")}
    if compression not in needed:
        raise NotImplementedError("That compression method is not supported")
    module, name = needed[compression]
    if not module:
        raise RuntimeError(f"Compression requires the (missing) {name} module")


def _get_compressor(compress_type, compresslevel=None):
    if compress_type == ZIP_DEFLATED:
        if compresslevel is not None:
            return zlib.compressobj(compresslevel, zlib.DEFLATED, -15)
        return zlib.compressobj(zlib.Z_DEFAULT_COMPRESSION, zlib.DEFLATED, -15)
    if compress_type == ZIP_BZIP2:
        if compresslevel is not None:
            return bz2.BZ2Compressor(compresslevel)
        return bz2.BZ2Compressor()
    if compress_type == ZIP_LZMA:
        return LZMACompressor()
    if compress_type == ZIP_ZSTANDARD:
        return zstd.ZstdCompressor(level=compresslevel)
    return None


def _get_decompressor(compress_type):
    _check_compression(compress_type)
    if compress_type == ZIP_STORED:
        return None
    if compress_type == ZIP_DEFLATED:
        return zlib.decompressobj(-15)
    if compress_type == ZIP_BZIP2:
        return bz2.BZ2Decompressor()
    if compress_type == ZIP_LZMA:
        return LZMADecompressor()
    if compress_type == ZIP_ZSTANDARD:
        return zstd.ZstdDecompressor()
    descr = compressor_names.get(compress_type)
    if descr:
        raise NotImplementedError("compression type %d (%s)" % (compress_type, descr))
    raise NotImplementedError("compression type %d" % (compress_type,))


class _ArchiveView:
    """View of the archive file used by one open member; keeps its own
    position and serialises access through the archive lock."""

    def __init__(self, file, pos, close, lock, writing):
        self._file = file
        self._pos = pos
        self._close = close
        self._lock = lock
        self._writing = writing
        self.seekable = file.seekable

    def tell(self):
        return self._pos

    def seek(self, offset, whence=0):
        with self._lock:
            if self._writing():
                raise ValueError("Can't reposition in the ZIP file while "
                                 "there is an open writing handle on it. "
                                 "Close the writing handle before trying to read.")
            if whence == 1:
                self._file.seek(self._pos + offset)
            else:
                self._file.seek(offset, whence)
            self._pos = self._file.tell()
            return self._pos

    def read(self, n=-1):
        with self._lock:
            if self._writing():
                raise ValueError("Can't read from the ZIP file while there "
                                 "is an open writing handle on it. "
                                 "Close the writing handle before trying to read.")
            self._file.seek(self._pos)
            data = self._file.read(n)
            self._pos = self._file.tell()
            return data

    def close(self):
        if self._file is not None:
            fileobj = self._file
            self._file = None
            self._close(fileobj)


class _CountingWriter:
    """Output stream wrapper for unseekable targets: counts the bytes
    written so that tell() can report the position."""

    def __init__(self, fp):
        self.fp = fp
        self.offset = 0

    def write(self, data):
        written = self.fp.write(data)
        self.offset += written
        return written

    def tell(self):
        return self.offset

    def flush(self):
        self.fp.flush()

    def close(self):
        self.fp.close()


class ZipExtFile(io.BufferedIOBase):
    """Readable file object for one archive member.

    Data is decrypted and decompressed on demand and the CRC-32 is checked
    when the end of the member is reached.  Seeking is supported when the
    archive file is seekable (backwards by starting over).
    """

    # largest request passed to one decompression step by read(-1)
    MAX_N = 1 << 31 - 1
    # compressed bytes fetched per step; also the smallest output limit
    # given to the decompressor in one step
    MIN_READ_SIZE = 4096
    # a forward seek decompresses in pieces of at most this size
    MAX_SEEK_READ = 1 << 24

    def __init__(self, fileobj, mode, zipinfo, pwd=None, close_fileobj=False):
        self._fileobj = fileobj
        self._pwd = pwd
        self._close_fileobj = close_fileobj
        self._compress_type = zipinfo.compress_type
        self._sizes = (zipinfo.compress_size, zipinfo.file_size)
        self.newlines = None
        self.mode = mode
        self.name = zipinfo.filename
        # directories have no meaningful CRC
        self._crc_original = (zipinfo.CRC if hasattr(zipinfo, 'CRC')
                              and not zipinfo.is_dir() else None)
        self._seekable = False
        self._data_start = None
        try:
            if fileobj.seekable():
                self._data_start = fileobj.tell()
                self._seekable = True
        except AttributeError:
            pass
        self._decrypter = None
        self._restart()
        if pwd:
            # the last byte of the encryption header must match the CRC (or
            # the DOS time when sizes and CRC follow the data)
            if zipinfo.flag_bits & _MASK_USE_DATA_DESCRIPTOR:
                expected = (zipinfo._raw_time >> 8) & 0xff
            else:
                expected = (zipinfo.CRC >> 24) & 0xff
            if self._start_decryption() != expected:
                raise RuntimeError("Bad password for file %r" % zipinfo.orig_filename)

    def _restart(self):
        # reading state for the beginning of the member
        self._compress_left, self._left = self._sizes
        self._decompressor = _get_decompressor(self._compress_type)
        self._eof = False
        self._readbuffer = b''      # decompressed data not yet returned ...
        self._offset = 0            # ... starting at this index
        self._crc_seen = crc32(b'')
        self._crc_wanted = self._crc_original

    def _start_decryption(self):
        """Set up ZipCrypto from the 12-byte header; return its last
        decrypted byte."""
        self._decrypter = _ZipCryptoDecoder(self._pwd)
        header = self._fileobj.read(12)
        self._compress_left -= 12
        return self._decrypter(header)[11]

    def __repr__(self):
        cls = type(self)
        text = f'<{cls.__module__}.{cls.__qualname__}'
        if self.closed:
            return text + ' [closed]>'
        text += f' name={self.name!r} mode={self.mode!r}'
        if self._compress_type != ZIP_STORED:
            text += ' compress_type=%s' % compressor_names.get(
                self._compress_type, self._compress_type)
        return text + '>'

    def readline(self, limit=-1):
        """Read one line (up to limit bytes) of the member."""
        if limit < 0:
            newline = self._readbuffer.find(b'\n', self._offset)
            if newline >= 0:
                start, self._offset = self._offset, newline + 1
                return self._readbuffer[start:self._offset]
        return io.BufferedIOBase.readline(self, limit)

    def peek(self, n=1):
        """Return buffered bytes without advancing the position."""
        if len(self._readbuffer) - self._offset < n:
            got = self.read(n)
            if len(got) <= self._offset:
                # what was read is still in the buffer: step back over it
                self._offset -= len(got)
            else:
                self._readbuffer = got + self._readbuffer[self._offset:]
                self._offset = 0
        return self._readbuffer[self._offset:self._offset + 512]

    def readable(self):
        if self.closed:
            raise ValueError("I/O operation on closed file.")
        return True

    def _take(self, n, single):
        """Return up to n bytes (all if n is None/negative): first from the
        look-ahead buffer, then by decompressing more.  With *single*, stop
        after the first chunk that produced data."""
        avail = len(self._readbuffer) - self._offset
        if n is not None and 0 <= n <= avail:
            start = self._offset
            self._offset += n
            return self._readbuffer[start:self._offset]
        parts = [self._readbuffer[self._offset:]]
        self._readbuffer = b''
        self._offset = 0
        need = None if n is None or n < 0 else n - avail
        while not self._eof and (need is None or need > 0):
            chunk = self._read1(self.MAX_N if need is None else need)
            if need is not None and len(chunk) > need:
                # keep the surplus for the next call
                self._readbuffer = chunk
                self._offset = need
                chunk = chunk[:need]
            parts.append(chunk)
            if need is not None:
                need -= len(chunk)
            if single and chunk:
                break
        return b''.join(parts)

    def read(self, n=-1):
        """Read and return up to n bytes (everything if n is None/negative)."""
        if self.closed:
            raise ValueError("read from closed file.")
        return self._take(n, single=False)

    def read1(self, n):
        """Read up to n bytes, decompressing at most one chunk."""
        avail = len(self._readbuffer) - self._offset
        if n is not None and n >= 0 and avail:
            return self._take(min(n, avail), True)
        return self._take(n, single=True)

    def _check_crc(self, data):
        if self._crc_wanted is None:
            return
        self._crc_seen = crc32(data, self._crc_seen)
        if self._eof and self._crc_seen != self._crc_wanted:
            raise BadZipFile("Bad CRC-32 for file %r" % self.name)

    def _read1(self, n):
        """Decompress at most one chunk of input and return the result,
        which is bounded by max(n, MIN_READ_SIZE) bytes."""
        if self._eof or n <= 0:
            return b''
        kind = self._compress_type
        engine = self._decompressor
        limit = max(n, self.MIN_READ_SIZE)
        if kind == ZIP_STORED:
            data = self._fetch(n)
            self._eof = self._compress_left <= 0
        elif kind == ZIP_DEFLATED:
            pending = engine.unconsumed_tail
            if n > len(pending):
                pending += self._fetch(n - len(pending))
            data = engine.decompress(pending, limit)
            self._eof = engine.eof or (self._compress_left <= 0
                                       and not engine.unconsumed_tail)
            if self._eof:
                data += engine.flush()
        elif hasattr(engine, 'needs_input'):
            # a decompressor still holding input is drained before more is
            # fed; the output limit protects against huge expansion
            pending = self._fetch(n) if engine.needs_input else b''
            data = engine.decompress(pending, limit)
            self._eof = engine.eof or (self._compress_left <= 0
                                       and engine.needs_input)
        else:
            # third-party decompressors offering only decompress(data)
            data = engine.decompress(self._fetch(n))
            self._eof = engine.eof or self._compress_left <= 0
        data = data[:self._left]
        self._left -= len(data)
        if self._left <= 0:
            self._eof = True
        self._check_crc(data)
        return data

    def _fetch(self, n):
        # the next piece of (decrypted) compressed data, MIN_READ_SIZE bytes
        # at least and never past the member's end
        if self._compress_left <= 0:
            return b''
        size = min(max(n, self.MIN_READ_SIZE), self._compress_left)
        data = self._fileobj.read(size)
        self._compress_left -= len(data)
        if not data:
            raise EOFError
        if self._decrypter is not None:
            data = self._decrypter(data)
        return data

    def close(self):
        try:
            if self._close_fileobj:
                self._fileobj.close()
        finally:
            super().close()

    def seekable(self):
        if self.closed:
            raise ValueError("I/O operation on closed file.")
        return self._seekable

    def seek(self, offset, whence=os.SEEK_SET):
        if self.closed:
            raise ValueError("seek on closed file.")
        if not self._seekable:
            raise io.UnsupportedOperation("underlying stream is not seekable")
        here = self.tell()
        size = self._sizes[1]
        if whence == os.SEEK_SET:
            target = offset
        elif whence == os.SEEK_CUR:
            target = here + offset
        elif whence == os.SEEK_END:
            target = size + offset
        else:
            raise ValueError("whence must be os.SEEK_SET (0), "
                             "os.SEEK_CUR (1), or os.SEEK_END (2)")
        target = min(max(target, 0), size)
        distance = target - here
        if 0 <= self._offset + distance < len(self._readbuffer):
            # inside the look-ahead buffer
            self._offset += distance
            return self.tell()
        if self._compress_type == ZIP_STORED and self._decrypter is None:
            # unencrypted stored data maps directly onto the archive file
            # (the CRC can then no longer be checked)
            self._crc_wanted = None
            self._fileobj.seek(self._data_start + target)
            self._compress_left = self._sizes[0] - target
            self._left = size - target
            self._eof = self._left <= 0
            self._readbuffer = b''
            self._offset = 0
            return self.tell()
        if distance < 0:
            # backwards: decompress again from the start
            self._fileobj.seek(self._data_start)
            self._restart()
            if self._decrypter is not None:
                self._start_decryption()
            distance = target
        while distance > 0:
            step = min(self.MAX_SEEK_READ, distance)
            self.read(step)
            distance -= step
        return self.tell()

    def tell(self):
        if self.closed:
            raise ValueError("tell on closed file.")
        if not self._seekable:
            raise io.UnsupportedOperation("underlying stream is not seekable")
        return (self._sizes[1] - self._left - len(self._readbuffer)
                + self._offset)


class _MemberWriter(io.BufferedIOBase):
    """Writable file object for a new member (from ZipFile.open(..., 'w'))."""

    def __init__(self, zf, zinfo, zip64):
        self._zinfo = zinfo
        self._zip64 = zip64
        self._zipfile = zf
        self._compressor = _get_compressor(zinfo.compress_type,
                                           zinfo.compress_level)
        self._file_size = 0
        self._compress_size = 0
        self._crc = 0

    @property
    def _fileobj(self):
        return self._zipfile.fp

    @property
    def name(self):
        return self._zinfo.filename

    @property
    def mode(self):
        return 'wb'

    def writable(self):
        return True

    def write(self, data):
        if self.closed:
            raise ValueError('I/O operation on closed file.')
        if isinstance(data, (bytes, bytearray)):
            nbytes = len(data)
        else:
            data = memoryview(data)
            nbytes = data.nbytes
        self._file_size += nbytes
        self._crc = crc32(data, self._crc)
        if self._compressor:
            data = self._compressor.compress(data)
            self._compress_size += len(data)
        self._fileobj.write(data)
        return nbytes

    def close(self):
        if self.closed:
            return
        try:
            super().close()
            if self._compressor:
                tail = self._compressor.flush()
                self._compress_size += len(tail)
                self._fileobj.write(tail)
                self._zinfo.compress_size = self._compress_size
            else:
                self._zinfo.compress_size = self._file_size
            self._zinfo.CRC = self._crc
            self._zinfo.file_size = self._file_size

            if not self._zip64:
                if self._file_size > ZIP64_LIMIT:
                    raise RuntimeError("File size too large, try using force_zip64")
                if self._compress_size > ZIP64_LIMIT:
                    raise RuntimeError("Compressed size too large, try using force_zip64")

            if self._zinfo.flag_bits & _MASK_USE_DATA_DESCRIPTOR:
                fmt = '<LLQQ' if self._zip64 else '<LLLL'
                self._fileobj.write(struct.pack(
                    fmt, _DD_SIGNATURE, self._zinfo.CRC,
                    self._zinfo.compress_size, self._zinfo.file_size))
                self._zipfile.start_dir = self._fileobj.tell()
            else:
                # Go back and patch the local header.
                self._zipfile.start_dir = self._fileobj.tell()
                self._fileobj.seek(self._zinfo.header_offset)
                self._fileobj.write(self._zinfo.FileHeader(self._zip64))
                self._fileobj.seek(self._zipfile.start_dir)

            self._zipfile.filelist.append(self._zinfo)
            self._zipfile.NameToInfo[self._zinfo.filename] = self._zinfo
        finally:
            self._zipfile._writing = False


class ZipFile:
    """A ZIP archive.

    ZipFile(file, mode='r', compression=ZIP_STORED, allowZip64=True,
            compresslevel=None, *, strict_timestamps=True,
            metadata_encoding=None)

    file: path or file object; mode: 'r', 'w', 'x' or 'a'.
    """

    fp = None
    _windows_illegal_name_trans_table = None

    def __init__(self, file, mode="r", compression=ZIP_STORED, allowZip64=True,
                 compresslevel=None, *, strict_timestamps=True,
                 metadata_encoding=None):
        if mode not in ('r', 'w', 'x', 'a'):
            raise ValueError("ZipFile requires mode 'r', 'w', 'x', or 'a'")

        _check_compression(compression)

        self._allowZip64 = allowZip64
        self._didModify = False
        self.debug = 0
        self.NameToInfo = {}
        self.filelist = []
        self.compression = compression
        self.compresslevel = compresslevel
        self.mode = mode
        self._pwd = None
        self._comment = b''
        self._strict_timestamps = strict_timestamps
        self.metadata_encoding = metadata_encoding

        if self.metadata_encoding and mode != 'r':
            raise ValueError(
                "metadata_encoding is only supported for reading files")

        if isinstance(file, os.PathLike):
            file = os.fspath(file)
        if isinstance(file, str):
            self._filePassed = 0
            self.filename = file
            modes = {'r': 'rb', 'w': 'w+b', 'x': 'x+b', 'a': 'r+b',
                     'r+b': 'w+b', 'w+b': 'wb', 'x+b': 'xb'}
            filemode = modes[mode]
            while True:
                try:
                    self.fp = io.open(file, filemode)
                except OSError:
                    if filemode in modes:
                        filemode = modes[filemode]
                        continue
                    raise
                break
        else:
            self._filePassed = 1
            self.fp = file
            self.filename = getattr(file, 'name', None)
        self._fileRefCnt = 1
        self._lock = threading.RLock()
        self._seekable = True
        self._writing = False
        # File position where the archive starts (offsets are stored
        # relative to it).
        self._start_disk = 0

        try:
            if mode == 'r':
                self._RealGetContents()
            elif mode in ('w', 'x'):
                self._didModify = True
                try:
                    self.start_dir = self.fp.tell()
                    self._start_disk = self.start_dir
                except (AttributeError, OSError):
                    self.fp = _CountingWriter(self.fp)
                    self.start_dir = 0
                    self._seekable = False
                else:
                    try:
                        self.fp.seek(self.start_dir)
                    except (AttributeError, OSError):
                        self._seekable = False
            elif mode == 'a':
                try:
                    self._RealGetContents()
                    self.fp.seek(self.start_dir)
                except BadZipFile:
                    # Not an archive yet: append a new one at the end.
                    self.fp.seek(0, 2)
                    self._didModify = True
                    self.start_dir = self.fp.tell()
                    self._start_disk = self.start_dir
        except:
            fp = self.fp
            self.fp = None
            self._fpclose(fp)
            raise

    def __enter__(self):
        return self

    def __exit__(self, type, value, traceback):
        self.close()

    def __repr__(self):
        result = ['<%s.%s' % (self.__class__.__module__,
                              self.__class__.__qualname__)]
        if self.fp is not None:
            if self._filePassed:
                result.append(' file=%r' % self.fp)
            elif self.filename is not None:
                result.append(' filename=%r' % self.filename)
            result.append(' mode=%r' % self.mode)
        else:
            result.append(' [closed]')
        result.append('>')
        return ''.join(result)

    def _RealGetContents(self):
        """Read the central directory."""
        fp = self.fp
        try:
            endrec = _find_end_record(fp)
        except OSError:
            raise BadZipFile("File is not a zip file")
        if not endrec:
            raise BadZipFile("File is not a zip file")
        if self.debug > 1:
            print(endrec)
        self._comment = endrec[_END_COMMENT]
        size_cd = endrec[_END_SIZE]
        offset_cd = endrec[_END_OFFSET]

        # Bytes prepended to the archive (e.g. a launcher stub): the
        # directory physically ends where the end record (or ZIP64 record)
        # starts, while the stored offsets count from the archive start.
        concat = endrec[_END_LOCATION] - size_cd - offset_cd
        if concat < 0:
            raise BadZipFile("Bad offset for central directory")

        if self.debug > 2:
            print("given, inferred, offset", offset_cd, concat, offset_cd + concat)
        self.start_dir = offset_cd + concat
        self._start_disk = concat
        if self.start_dir < 0:
            raise BadZipFile("Bad offset for central directory")
        fp.seek(self.start_dir, 0)
        data = fp.read(size_cd)
        fp = io.BytesIO(data)
        total = 0
        while total < size_cd:
            centdir = fp.read(sizeCentralDir)
            if len(centdir) != sizeCentralDir:
                raise BadZipFile("Truncated central directory")
            centdir = struct.unpack(structCentralDir, centdir)
            if centdir[_CEN_SIGNATURE] != stringCentralDir:
                raise BadZipFile("Bad magic number for central directory")
            if self.debug > 2:
                print(centdir)
            filename = fp.read(centdir[_CEN_FILENAME_LENGTH])
            orig_filename_crc = crc32(filename)
            flags = centdir[_CEN_FLAG_BITS]
            raw_name = filename
            if flags & _MASK_UTF_FILENAME:
                filename = filename.decode('utf-8')
            else:
                filename = filename.decode(self.metadata_encoding or 'cp437')
            x = ZipInfo(filename)
            if not flags & _MASK_UTF_FILENAME:
                x._stored_name = (raw_name, x.filename)
            x.extra = fp.read(centdir[_CEN_EXTRA_FIELD_LENGTH])
            x.comment = fp.read(centdir[_CEN_COMMENT_LENGTH])
            x.header_offset = centdir[_CEN_LOCAL_HEADER_OFFSET]
            (x.create_version, x.create_system, x.extract_version, x.reserved,
             x.flag_bits, x.compress_type, t, d,
             x.CRC, x.compress_size, x.file_size) = centdir[1:12]
            if x.extract_version > MAX_EXTRACT_VERSION:
                raise NotImplementedError("zip file version %.1f" %
                                          (x.extract_version / 10))
            x.volume, x.internal_attr, x.external_attr = centdir[15:18]
            x._raw_time = t
            x.date_time = ((d >> 9) + 1980, (d >> 5) & 0xF, d & 0x1F,
                           t >> 11, (t >> 5) & 0x3F, (t & 0x1F) * 2)
            x._decodeExtra(orig_filename_crc)
            x.header_offset = x.header_offset + concat
            self.filelist.append(x)
            self.NameToInfo[x.filename] = x
            total = (total + sizeCentralDir + centdir[_CEN_FILENAME_LENGTH]
                     + centdir[_CEN_EXTRA_FIELD_LENGTH]
                     + centdir[_CEN_COMMENT_LENGTH])
            if self.debug > 2:
                print("total", total)

        end_offset = self.start_dir
        for zinfo in reversed(sorted(self.filelist,
                                     key=lambda zi: zi.header_offset)):
            zinfo._end_offset = end_offset
            end_offset = zinfo.header_offset

    def namelist(self):
        """Names of the members, in archive order."""
        return [data.filename for data in self.filelist]

    def infolist(self):
        """ZipInfo objects of the members, in archive order."""
        return self.filelist

    def printdir(self, file=None):
        """Print a table of contents."""
        print("%-46s %19s %12s" % ("File Name", "Modified    ", "Size"),
              file=file)
        for zinfo in self.filelist:
            date = "%d-%02d-%02d %02d:%02d:%02d" % zinfo.date_time[:6]
            print("%-46s %s %12d" % (zinfo.filename, date, zinfo.file_size),
                  file=file)

    def testzip(self):
        """Read every member; return the name of the first bad one or None."""
        chunk_size = 2 ** 20
        for zinfo in self.filelist:
            try:
                with self.open(zinfo.filename, "r") as f:
                    while f.read(chunk_size):
                        pass
            except BadZipFile:
                return zinfo.filename
        return None

    def getinfo(self, name):
        """ZipInfo for the member called *name*."""
        info = self.NameToInfo.get(name)
        if info is None:
            raise KeyError(
                'There is no item named %r in the archive' % name)
        return info

    def setpassword(self, pwd):
        """Default password for encrypted members."""
        if pwd and not isinstance(pwd, bytes):
            raise TypeError("pwd: expected bytes, got %s" % type(pwd).__name__)
        if pwd:
            self._pwd = pwd
        else:
            self._pwd = None

    @property
    def pwd(self):
        return self._pwd

    @pwd.setter
    def pwd(self, value):
        if value and not isinstance(value, bytes):
            raise TypeError("pwd: expected bytes, got %s" % type(value).__name__)
        self._pwd = value

    @property
    def comment(self):
        """Archive comment (bytes)."""
        return self._comment

    @comment.setter
    def comment(self, comment):
        if not isinstance(comment, bytes):
            raise TypeError("comment: expected bytes, got %s" % type(comment).__name__)
        if len(comment) > ZIP_MAX_COMMENT:
            import warnings
            warnings.warn('Archive comment is too long; truncating to %d bytes'
                          % ZIP_MAX_COMMENT, stacklevel=2)
            comment = comment[:ZIP_MAX_COMMENT]
        self._comment = comment
        self._didModify = True

    def read(self, name, pwd=None):
        """Return the bytes of member *name*."""
        with self.open(name, "r", pwd) as fp:
            return fp.read()

    def open(self, name, mode="r", pwd=None, *, force_zip64=False):
        """Return a file object for member *name* (a name or ZipInfo).

        mode 'r' reads an existing member; mode 'w' adds a new one.
        """
        if mode not in {"r", "w"}:
            raise ValueError('open() requires mode "r" or "w"')
        if pwd and (mode == "w"):
            raise ValueError("pwd is only supported for reading files")
        if pwd and not isinstance(pwd, bytes):
            raise TypeError("pwd: expected bytes, got %s" % type(pwd).__name__)
        if not self.fp:
            raise ValueError(
                "Attempt to use ZIP archive that was already closed")

        if isinstance(name, ZipInfo):
            zinfo = name
        elif mode == 'w':
            zinfo = ZipInfo(name)
            zinfo.compress_type = self.compression
            zinfo.compress_level = self.compresslevel
        else:
            zinfo = self.getinfo(name)

        if mode == 'w':
            return self._open_to_write(zinfo, force_zip64=force_zip64)

        if self._writing:
            raise ValueError("Can't open %s while there is an open writing "
                             "handle on it. Close the writing handle before "
                             "trying to read." % (name,))

        self._fileRefCnt += 1
        zef_file = _ArchiveView(self.fp, zinfo.header_offset,
                               self._fpclose, self._lock, lambda: self._writing)
        try:
            fheader = zef_file.read(sizeFileHeader)
            if len(fheader) != sizeFileHeader:
                raise BadZipFile("Truncated file header")
            fheader = struct.unpack(structFileHeader, fheader)
            if fheader[_LOC_SIGNATURE] != stringFileHeader:
                raise BadZipFile("Bad magic number for file header")

            fname = zef_file.read(fheader[_LOC_FILENAME_LENGTH])
            if fheader[_LOC_EXTRA_FIELD_LENGTH]:
                zef_file.seek(fheader[_LOC_EXTRA_FIELD_LENGTH], whence=1)

            if zinfo.flag_bits & _MASK_COMPRESSED_PATCH:
                raise NotImplementedError("compressed patched data (flag bit 5)")
            if zinfo.flag_bits & _MASK_STRONG_ENCRYPTION:
                raise NotImplementedError("strong encryption (flag bit 6)")

            if fheader[_LOC_GENERAL_PURPOSE_FLAG_BITS] & _MASK_UTF_FILENAME:
                fname_str = fname.decode("utf-8")
            else:
                fname_str = fname.decode(self.metadata_encoding or "cp437")

            if fname_str != zinfo.orig_filename:
                raise BadZipFile(
                    'File name in directory %r and header %r differ.'
                    % (zinfo.orig_filename, fname))

            if (zinfo._end_offset is not None and
                    zef_file.tell() + zinfo.compress_size > zinfo._end_offset):
                if zinfo._end_offset == zinfo.header_offset:
                    import warnings
                    warnings.warn(
                        f"Overlapped entries: {zinfo.orig_filename!r} "
                        f"(possible zip bomb)", skip_file_prefixes=(os.path.dirname(__file__),))
                else:
                    raise BadZipFile(
                        f"Overlapped entries: {zinfo.orig_filename!r} "
                        f"(possible zip bomb)")

            is_encrypted = zinfo.flag_bits & _MASK_ENCRYPTED
            if is_encrypted:
                if not pwd:
                    pwd = self._pwd
                if pwd and not isinstance(pwd, bytes):
                    raise TypeError("pwd: expected bytes, got %s" % type(pwd).__name__)
                if not pwd:
                    raise RuntimeError("File %r is encrypted, password "
                                       "required for extraction" % name)
            else:
                pwd = None

            return ZipExtFile(zef_file, mode + 'b', zinfo, pwd, True)
        except:
            zef_file.close()
            raise

    def _open_to_write(self, zinfo, force_zip64=False):
        if force_zip64 and not self._allowZip64:
            raise ValueError(
                "force_zip64 is True, but allowZip64 was False when opening "
                "the ZIP file.")
        if self._writing:
            raise ValueError("Can't write to the ZIP file while there is "
                             "another write handle open on it. "
                             "Close the first handle before opening another.")

        zinfo.compress_size = 0
        zinfo.CRC = 0
        zinfo.flag_bits = 0x00
        if zinfo.compress_type == ZIP_LZMA:
            zinfo.flag_bits |= _MASK_COMPRESS_OPTION_1
        if not self._seekable:
            zinfo.flag_bits |= _MASK_USE_DATA_DESCRIPTOR
        if not zinfo.external_attr:
            zinfo.external_attr = 0o600 << 16  # rw-------

        zip64 = force_zip64 or (zinfo.file_size * 1.05 > ZIP64_LIMIT)
        if not self._allowZip64 and zip64:
            raise LargeZipFile("Filesize would require ZIP64 extensions")

        if self._seekable:
            self.fp.seek(self.start_dir)
        zinfo.header_offset = self.fp.tell()

        self._writecheck(zinfo)
        self._didModify = True

        self.fp.write(zinfo.FileHeader(zip64))

        self._writing = True
        return _MemberWriter(self, zinfo, zip64)

    def extract(self, member, path=None, pwd=None):
        """Extract one member to *path* (default: current directory).
        Returns the normalized path created."""
        if path is None:
            path = os.getcwd()
        else:
            path = os.fspath(path)
        return self._extract_member(member, path, pwd)

    def extractall(self, path=None, members=None, pwd=None):
        """Extract all (or the given) members to *path*."""
        if members is None:
            members = self.namelist()
        if path is None:
            path = os.getcwd()
        else:
            path = os.fspath(path)
        for zipinfo in members:
            self._extract_member(zipinfo, path, pwd)

    @classmethod
    def _sanitize_windows_name(cls, arcname, pathsep):
        """Replace characters Windows forbids in file names."""
        table = cls._windows_illegal_name_trans_table
        if not table:
            illegal = ':<>|"?*'
            table = str.maketrans(illegal, '_' * len(illegal))
            cls._windows_illegal_name_trans_table = table
        arcname = arcname.translate(table)
        arcname = (x.rstrip(' .') for x in arcname.split(pathsep))
        arcname = pathsep.join(x for x in arcname if x)
        return arcname

    def _extract_member(self, member, targetpath, pwd):
        if not isinstance(member, ZipInfo):
            member = self.getinfo(member)

        # Build a safe relative path: no drive, no absolute part, no '..'.
        arcname = member.filename.replace('/', os.path.sep)
        if os.path.altsep:
            arcname = arcname.replace(os.path.altsep, os.path.sep)
        arcname = os.path.splitdrive(arcname)[1]
        invalid_path_parts = ('', os.path.curdir, os.path.pardir)
        arcname = os.path.sep.join(x for x in arcname.split(os.path.sep)
                                   if x not in invalid_path_parts)
        if os.path.sep == '\\':
            arcname = self._sanitize_windows_name(arcname, os.path.sep)

        if not arcname and not member.is_dir():
            raise ValueError("Empty filename.")

        targetpath = os.path.join(targetpath, arcname)
        targetpath = os.path.normpath(targetpath)

        upperdirs = os.path.dirname(targetpath)
        if upperdirs and not os.path.exists(upperdirs):
            os.makedirs(upperdirs, exist_ok=True)

        if member.is_dir():
            if not os.path.isdir(targetpath):
                try:
                    os.mkdir(targetpath)
                except FileExistsError:
                    if not os.path.isdir(targetpath):
                        raise
            return targetpath

        with self.open(member, pwd=pwd) as source, \
                open(targetpath, "wb") as target:
            shutil.copyfileobj(source, target)
        return targetpath

    def _writecheck(self, zinfo):
        """Validate before adding a member."""
        if zinfo.filename in self.NameToInfo:
            import warnings
            warnings.warn('Duplicate name: %r' % zinfo.filename, stacklevel=3)
        if self.mode not in ('w', 'x', 'a'):
            raise ValueError("write() requires mode 'w', 'x', or 'a'")
        if not self.fp:
            raise ValueError(
                "Attempt to write ZIP archive that was already closed")
        _check_compression(zinfo.compress_type)
        if not self._allowZip64:
            requires_zip64 = None
            if len(self.filelist) >= ZIP_FILECOUNT_LIMIT:
                requires_zip64 = "Files count"
            elif zinfo.file_size > ZIP64_LIMIT:
                requires_zip64 = "Filesize"
            elif zinfo.header_offset > ZIP64_LIMIT:
                requires_zip64 = "Zipfile size"
            if requires_zip64:
                raise LargeZipFile(requires_zip64 +
                                   " would require ZIP64 extensions")

    def write(self, filename, arcname=None,
              compress_type=None, compresslevel=None):
        """Add the file *filename* to the archive as *arcname*."""
        if not self.fp:
            raise ValueError(
                "Attempt to write to ZIP archive that was already closed")
        if self._writing:
            raise ValueError(
                "Can't write to ZIP archive while an open writing handle exists")

        zinfo = ZipInfo.from_file(filename, arcname,
                                  strict_timestamps=self._strict_timestamps)

        if zinfo.is_dir():
            zinfo.compress_size = 0
            zinfo.CRC = 0
            self.mkdir(zinfo)
        else:
            zinfo.compress_type = (compress_type if compress_type is not None
                                   else self.compression)
            zinfo.compress_level = (compresslevel if compresslevel is not None
                                    else self.compresslevel)
            with open(filename, "rb") as src, self.open(zinfo, 'w') as dest:
                shutil.copyfileobj(src, dest, 1024 * 8)

    def writestr(self, zinfo_or_arcname, data,
                 compress_type=None, compresslevel=None):
        """Add a member with contents *data* (bytes or str)."""
        if isinstance(data, str):
            data = data.encode("utf-8")
        if not isinstance(zinfo_or_arcname, ZipInfo):
            zinfo = ZipInfo(zinfo_or_arcname)._for_archive(self)
        else:
            zinfo = zinfo_or_arcname

        if not self.fp:
            raise ValueError(
                "Attempt to write to ZIP archive that was already closed")
        if self._writing:
            raise ValueError(
                "Can't write to ZIP archive while an open writing handle exists.")

        if compress_type is not None:
            zinfo.compress_type = compress_type
        if compresslevel is not None:
            zinfo.compress_level = compresslevel

        zinfo.file_size = len(data)
        with self._lock:
            with self.open(zinfo, mode='w') as dest:
                dest.write(data)

    def mkdir(self, zinfo_or_directory_name, mode=511):
        """Add a directory member."""
        if isinstance(zinfo_or_directory_name, ZipInfo):
            zinfo = zinfo_or_directory_name
            if not zinfo.is_dir():
                raise ValueError("The given ZipInfo does not describe a directory")
        elif isinstance(zinfo_or_directory_name, str):
            directory_name = zinfo_or_directory_name
            if not directory_name.endswith("/"):
                directory_name += "/"
            zinfo = ZipInfo(directory_name)
            zinfo.compress_size = 0
            zinfo.CRC = 0
            zinfo.external_attr = ((0o40000 | mode) & 0xFFFF) << 16
            zinfo.file_size = 0
            zinfo.external_attr |= 0x10
        else:
            raise TypeError("Expected type str or ZipInfo")

        if not self.fp:
            raise ValueError(
                "Attempt to write to ZIP archive that was already closed")
        if self._writing:
            raise ValueError("Can't write to ZIP archive while an open "
                             "writing handle exists")

        with self._lock:
            if self._seekable:
                self.fp.seek(self.start_dir)
            zinfo.header_offset = self.fp.tell()
            if zinfo.compress_type == ZIP_LZMA:
                zinfo.flag_bits |= _MASK_COMPRESS_OPTION_1

            self._writecheck(zinfo)
            self._didModify = True

            self.filelist.append(zinfo)
            self.NameToInfo[zinfo.filename] = zinfo
            self.fp.write(zinfo.FileHeader(False))
            self.start_dir = self.fp.tell()

    def __del__(self):
        self.close()

    def close(self):
        """Write the central directory (if modified) and close the file."""
        if self.fp is None:
            return

        if self._writing:
            raise ValueError("Can't close the ZIP file while there is "
                             "an open writing handle on it. "
                             "Close the writing handle before closing the zip.")

        try:
            if self.mode in ('w', 'x', 'a') and self._didModify:
                with self._lock:
                    if self._seekable:
                        self.fp.seek(self.start_dir)
                    self._write_end_record()
        finally:
            fp = self.fp
            self.fp = None
            self._fpclose(fp)

    def _write_end_record(self):
        """Write the central directory (from self.start_dir) and the end
        records."""
        for zinfo in self.filelist:
            self.fp.write(self._central_entry(zinfo))
        dir_end = self.fp.tell()
        self.fp.write(self._directory_trailer(len(self.filelist),
                                              dir_end - self.start_dir,
                                              self.start_dir - self._start_disk,
                                              dir_end - self._start_disk))
        if self.mode == "a":
            self.fp.truncate()
        self.fp.flush()

    def _central_entry(self, zinfo):
        """Central directory record (header, name, extra, comment)."""
        year, month, day, hour, minute, second = zinfo.date_time[:6]
        dos_date = (year - 1980) << 9 | month << 5 | day
        dos_time = hour << 11 | minute << 5 | second // 2

        # Values too large for 32 bits move to a ZIP64 extra field, in the
        # order the format prescribes.
        wide = []
        sizes_wide = (zinfo.file_size > ZIP64_LIMIT
                      or zinfo.compress_size > ZIP64_LIMIT)
        if sizes_wide:
            wide += [zinfo.file_size, zinfo.compress_size]
        header_offset = zinfo.header_offset - self._start_disk
        offset_wide = header_offset > ZIP64_LIMIT
        if offset_wide:
            wide.append(header_offset)

        extra = zinfo.extra
        needed = _METHOD_MIN_VERSION.get(zinfo.compress_type, 0)
        if wide:
            extra = (struct.pack('<HH%dQ' % len(wide), _EXTRA_ZIP64,
                                 8 * len(wide), *wide)
                     + _Extra.strip(extra, (_EXTRA_ZIP64,)))
            needed = max(needed, ZIP64_VERSION)

        name, flags = zinfo._encodeFilenameFlags()
        record = struct.pack(
            structCentralDir, stringCentralDir,
            max(needed, zinfo.create_version), zinfo.create_system,
            max(needed, zinfo.extract_version), zinfo.reserved, flags,
            zinfo.compress_type, dos_time, dos_date, zinfo.CRC,
            0xFFFFFFFF if sizes_wide else zinfo.compress_size,
            0xFFFFFFFF if sizes_wide else zinfo.file_size,
            len(name), len(extra), len(zinfo.comment), 0,
            zinfo.internal_attr, zinfo.external_attr,
            0xFFFFFFFF if offset_wide else header_offset)
        return record + name + extra + zinfo.comment

    def _directory_trailer(self, count, size, offset, position):
        """End of central directory record, preceded by the ZIP64 record
        and locator when a value exceeds the classic limits."""
        if count > ZIP_FILECOUNT_LIMIT:
            too_big = "Files count"
        elif offset > ZIP64_LIMIT:
            too_big = "Central directory offset"
        elif size > ZIP64_LIMIT:
            too_big = "Central directory size"
        else:
            too_big = None
        trailer = b""
        if too_big is not None:
            if not self._allowZip64:
                raise LargeZipFile(too_big + " would require ZIP64 extensions")
            trailer += struct.pack(structEndArchive64, stringEndArchive64,
                                   sizeEndCentDir64 - 12, ZIP64_VERSION,
                                   ZIP64_VERSION, 0, 0, count, count,
                                   size, offset)
            trailer += struct.pack(structEndArchive64Locator,
                                   stringEndArchive64Locator, 0, position, 1)
            count = min(count, 0xFFFF)
            size = min(size, 0xFFFFFFFF)
            offset = min(offset, 0xFFFFFFFF)
        trailer += struct.pack(structEndArchive, stringEndArchive, 0, 0,
                               count, count, size, offset, len(self._comment))
        return trailer + self._comment

    def _fpclose(self, fp):
        assert self._fileRefCnt > 0
        self._fileRefCnt -= 1
        if not self._fileRefCnt and not self._filePassed:
            fp.close()


class PyZipFile(ZipFile):
    """ZipFile whose writepy() stores Python modules as compiled code.

    *optimize* selects the bytecode: -1 uses (or creates) bytecode for the
    interpreter's own optimization level, 0, 1 or 2 force that level.
    """

    def __init__(self, file, mode="r", compression=ZIP_STORED,
                 allowZip64=True, optimize=-1):
        super().__init__(file, mode=mode, compression=compression,
                         allowZip64=allowZip64)
        self._optimize = optimize

    def _trace(self, *args):
        if self.debug:
            print(*args)

    def writepy(self, pathname, basename="", filterfunc=None):
        """Add a .py module, or every module of a package (recursively) or
        of a plain directory, as compiled files under *basename*.

        *filterfunc(path)* returning false excludes a file or directory.
        """
        pathname = os.fspath(pathname)
        is_dir = os.path.isdir(pathname)
        if filterfunc and not filterfunc(pathname):
            self._trace('%s %r skipped by filterfunc'
                        % ('path' if is_dir else 'file', pathname))
            return
        if not is_dir:
            if not pathname.endswith(".py"):
                raise RuntimeError(
                    'Files added with writepy() must end with ".py"')
            self._trace("Adding file", self._add_compiled(pathname, basename))
            return

        package = os.path.isfile(os.path.join(pathname, "__init__.py"))
        if package:
            dirname = os.path.split(pathname)[1]
            basename = f"{basename}/{dirname}" if basename else dirname
            self._trace("Adding package in", pathname, "as", basename)
            self._trace("Adding", self._add_compiled(
                os.path.join(pathname, "__init__.py"), basename))
        else:
            self._trace("Adding files from directory", pathname)

        for entry in sorted(os.listdir(pathname)):
            path = os.path.join(pathname, entry)
            if os.path.isdir(path):
                # Only sub-packages of packages are descended into.
                if package and os.path.isfile(os.path.join(path, "__init__.py")):
                    self.writepy(path, basename, filterfunc=filterfunc)
                continue
            if not entry.endswith(".py") or (package and entry == "__init__.py"):
                continue
            if filterfunc and not filterfunc(path):
                self._trace('file %r skipped by filterfunc' % path)
                continue
            self._trace("Adding", self._add_compiled(path, basename))

    def _add_compiled(self, source, basename):
        fname, arcname = self._get_codename(source[:-3], basename)
        self.write(fname, arcname)
        return arcname

    def _get_codename(self, pathname, basename):
        """(file to store, archive name) for module *pathname* (without
        the .py suffix): up-to-date bytecode is reused, otherwise the
        source is compiled; the source itself is stored if that fails."""
        source = pathname + ".py"
        legacy = pathname + ".pyc"
        cached = {level: importlib.util.cache_from_source(
                      source, optimization='' if level == 0 else level)
                  for level in (0, 1, 2)}
        source_mtime = os.stat(source).st_mtime

        def up_to_date(path):
            return os.path.isfile(path) and os.stat(path).st_mtime >= source_mtime

        def compiled_ok(level):
            import py_compile
            self._trace("Compiling", source)
            try:
                py_compile.compile(source, doraise=True, optimize=level)
            except py_compile.PyCompileError as err:
                print(err.msg)
                return False
            return True

        if self._optimize == -1:
            fname = next((p for p in (legacy, cached[0], cached[1], cached[2])
                          if up_to_date(p)), None)
            if fname is None:
                if compiled_ok(-1):
                    fname = cached[min(sys.flags.optimize, 2)]
                else:
                    fname = source
        else:
            if self._optimize not in cached:
                raise ValueError("invalid value for 'optimize': {!r}"
                                 .format(self._optimize))
            fname = cached[self._optimize]
            if not up_to_date(fname) and not compiled_ok(self._optimize):
                fname = source

        stored = os.path.basename(source if fname == source else legacy)
        if basename:
            stored = f"{basename}/{stored}"
        return fname, stored


def main(args=None):
    """Command-line interface: python -m zipfile -l|-e|-c|-t ..."""
    import argparse

    parser = argparse.ArgumentParser(
        description='List, test, extract or create ZIP archives.',
        color=True)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument('-l', '--list', metavar='<zipfile>',
                        help='list the members of an archive')
    action.add_argument('-e', '--extract', nargs=2,
                        metavar=('<zipfile>', '<output_dir>'),
                        help='extract an archive into a directory')
    action.add_argument('-c', '--create', nargs='+',
                        metavar=('<name>', '<file>'),
                        help='create an archive from files and directories')
    action.add_argument('-t', '--test', metavar='<zipfile>',
                        help='check the CRCs of all members')
    parser.add_argument('--metadata-encoding', metavar='<encoding>',
                        help='encoding of member names (for -l, -e and -t)')
    opts = parser.parse_args(args)
    encoding = opts.metadata_encoding

    if opts.create is not None:
        if encoding:
            print("Non-conforming encodings not supported with -c.",
                  file=sys.stderr)
            sys.exit(1)
        target, *sources = opts.create

        def add_tree(zf, path, arcpath):
            if os.path.isfile(path):
                zf.write(path, arcpath, ZIP_DEFLATED)
            elif os.path.isdir(path):
                if arcpath:
                    zf.write(path, arcpath)
                for entry in sorted(os.listdir(path)):
                    add_tree(zf, os.path.join(path, entry),
                             os.path.join(arcpath, entry))

        with ZipFile(target, 'w') as zf:
            for path in sources:
                arcpath = (os.path.basename(path)
                           or os.path.basename(os.path.dirname(path)))
                if arcpath in (os.curdir, os.pardir):
                    arcpath = ''
                add_tree(zf, path, arcpath)
        return

    source = opts.test or opts.list or (opts.extract and opts.extract[0])
    with ZipFile(source, 'r', metadata_encoding=encoding) as zf:
        if opts.test is not None:
            bad = zf.testzip()
            if bad:
                print("The following enclosed file is corrupted: {!r}".format(bad))
            print("Done testing")
        elif opts.list is not None:
            zf.printdir()
        else:
            zf.extractall(opts.extract[1])


from ._path import (  # noqa: E402
    Path,
    # used privately for tests
    CompleteDirs,  # noqa: F401
)
