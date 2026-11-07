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


def _dos_stamp(date_time):
    """(time, date) words of the MS-DOS timestamp for *date_time*."""
    year, month, day, hour, minute, second = date_time[:6]
    return (hour << 11 | minute << 5 | second // 2,
            (year - 1980) << 9 | month << 5 | day)


def _from_dos_stamp(dos_time, dos_date):
    return ((dos_date >> 9) + 1980, (dos_date >> 5) & 0xF, dos_date & 0x1F,
            dos_time >> 11, (dos_time >> 5) & 0x3F, (dos_time & 0x1F) * 2)


# ZIP64 extra field entries, in the order the format stores them:
# (attribute, values marking "see the ZIP64 field", name used in errors)
_ZIP64_SLOTS = (
    ('file_size', (0xFFFF_FFFF_FFFF_FFFF, 0xFFFF_FFFF), "File size"),
    ('compress_size', (0xFFFF_FFFF,), "Compress size"),
    ('header_offset', (0xFFFF_FFFF,), "Header offset"),
)

_EXTRA_UNICODE_PATH = 0x7075


class ZipInfo:
    """What the archive records about one member."""

    __slots__ = (
        'orig_filename', 'filename', 'date_time', 'compress_type',
        'compress_level', 'comment', 'extra', 'create_system',
        'create_version', 'extract_version', 'reserved', 'flag_bits',
        'volume', 'internal_attr', 'external_attr', 'header_offset', 'CRC',
        'compress_size', 'file_size', '_raw_time', '_end_offset', '_stored_name',
    )

    def __init__(self, filename="NoName", date_time=(1980, 1, 1, 0, 0, 0)):
        if date_time[0] < 1980:
            raise ValueError('ZIP does not support timestamps before 1980')
        self.orig_filename = filename           # as given
        self.filename = _clean_member_name(filename)
        self.date_time = date_time              # (y, m, d, H, M, S)
        self.compress_type = ZIP_STORED
        self.compress_level = None
        self.comment = b""
        self.extra = b""
        self.create_system = 0 if sys.platform == 'win32' else 3
        self.create_version = self.extract_version = DEFAULT_VERSION
        self.reserved = self.flag_bits = self.volume = 0
        self.internal_attr = self.external_attr = 0
        self.compress_size = self.file_size = 0
        self.header_offset = 0
        self.CRC = 0
        # where the member's data must end (start of the next member or of
        # the central directory); set when an archive is read
        self._end_offset = None
        # (raw name bytes, decoded name) for a non-UTF-8 name read from an
        # archive, so rewriting the directory keeps the original bytes
        self._stored_name = None

    # older spelling of compress_level
    @property
    def _compresslevel(self):
        return self.compress_level

    @_compresslevel.setter
    def _compresslevel(self, value):
        self.compress_level = value

    def __repr__(self):
        text = f'<{type(self).__name__} filename={self.filename!r}'
        if self.compress_type != ZIP_STORED:
            text += ' compress_type=%s' % compressor_names.get(
                self.compress_type, self.compress_type)
        mode, attrs = divmod(self.external_attr, 1 << 16)
        if mode:
            text += ' filemode=%r' % stat.filemode(mode)
        if attrs:
            text += ' external_attr=%#x' % attrs
        directory = self.is_dir()
        if self.file_size or not directory:
            text += ' file_size=%r' % self.file_size
        size_differs = (self.compress_type != ZIP_STORED
                        or self.file_size != self.compress_size)
        if size_differs and (self.compress_size or not directory):
            text += ' compress_size=%r' % self.compress_size
        return text + '>'

    def _needed_version(self, zip64):
        version = _METHOD_MIN_VERSION.get(self.compress_type, 0)
        return max(version, ZIP64_VERSION) if zip64 else version

    def FileHeader(self, zip64=None):
        """The local file header (with name and extra field) as bytes.

        *zip64* None means "only if the sizes need it"."""
        if self.flag_bits & _MASK_USE_DATA_DESCRIPTOR:
            # the values follow the data, in the data descriptor
            crc = compress_size = file_size = 0
        else:
            crc, compress_size, file_size = (self.CRC, self.compress_size,
                                             self.file_size)
        if zip64 is None:
            zip64 = max(file_size, compress_size) > ZIP64_LIMIT
        extra = self.extra
        if zip64:
            extra += struct.pack('<HHQQ', _EXTRA_ZIP64, 16,
                                 file_size, compress_size)
            file_size = compress_size = 0xFFFF_FFFF

        needed = self._needed_version(zip64)
        self.extract_version = max(needed, self.extract_version)
        self.create_version = max(needed, self.create_version)
        name, flag_bits = self._encodeFilenameFlags()
        dos_time, dos_date = _dos_stamp(self.date_time)
        header = struct.pack(structFileHeader, stringFileHeader,
                             self.extract_version, self.reserved, flag_bits,
                             self.compress_type, dos_time, dos_date, crc,
                             compress_size, file_size, len(name), len(extra))
        return header + name + extra

    def _encodeFilenameFlags(self):
        """(name bytes, flag bits): ASCII names as is, others in UTF-8 with
        the UTF-8 flag, legacy names read from an archive unchanged."""
        stored = self._stored_name
        if (stored is not None and stored[1] == self.filename
                and not self.flag_bits & _MASK_UTF_FILENAME):
            return stored[0], self.flag_bits
        if self.filename.isascii():
            return self.filename.encode('ascii'), self.flag_bits
        return (self.filename.encode('utf-8'),
                self.flag_bits | _MASK_UTF_FILENAME)

    def _decodeExtra(self, filename_crc):
        """Apply the extra field records this module understands: ZIP64
        sizes/offset and the Info-ZIP Unicode path."""
        rest = self.extra
        while len(rest) >= 4:
            kind, size = struct.unpack('<HH', rest[:4])
            if size + 4 > len(rest):
                raise BadZipFile(f"Corrupt extra field {kind:04x} (size={size})")
            payload, rest = rest[4:4 + size], rest[4 + size:]
            if kind == _EXTRA_ZIP64:
                self._apply_zip64(payload)
            elif kind == _EXTRA_UNICODE_PATH:
                self._apply_unicode_path(payload, filename_crc)

    def _apply_zip64(self, payload):
        # only the fields whose 32-bit value is the "see ZIP64" marker are
        # present, in this order
        for attr, markers, label in _ZIP64_SLOTS:
            if getattr(self, attr) not in markers:
                continue
            if len(payload) < 8:
                raise BadZipFile(f"Corrupt zip64 extra field. "
                                 f"{label} not found.")
            setattr(self, attr, struct.unpack('<Q', payload[:8])[0])
            payload = payload[8:]

    def _apply_unicode_path(self, payload, filename_crc):
        if len(payload) < 5:
            raise BadZipFile("Corrupt unicode path extra field (0x7075)")
        version, name_crc = struct.unpack('<BL', payload[:5])
        if version != 1 or name_crc != filename_crc:
            return      # stale: the name was changed after it was written
        try:
            name = payload[5:].decode('utf-8')
        except UnicodeDecodeError as exc:
            raise BadZipFile('Corrupt unicode path extra field (0x7075): '
                             'invalid utf-8 bytes') from exc
        if name:
            self.filename = _clean_member_name(name)
        else:
            import warnings
            warnings.warn("Empty unicode path extra field (0x7075)",
                          stacklevel=2)

    @classmethod
    def from_file(cls, filename, arcname=None, *, strict_timestamps=True):
        """A ZipInfo describing the file *filename* on disk, to be stored
        as *arcname* (default: the path without drive and leading
        separators)."""
        if isinstance(filename, os.PathLike):
            filename = os.fspath(filename)
        st = os.stat(filename)      # (a file descriptor works too)
        directory = stat.S_ISDIR(st.st_mode)
        date_time = time.localtime(st.st_mtime)[:6]
        if not strict_timestamps:
            # clamp to what the DOS timestamp can hold
            date_time = max(date_time, (1980, 1, 1, 0, 0, 0))
            date_time = min(date_time, (2107, 12, 31, 23, 59, 59))
        name = os.path.normpath(os.path.splitdrive(
            filename if arcname is None else arcname)[1])
        name = name.lstrip(os.sep + (os.altsep or ''))
        if directory:
            name += '/'
        info = cls(name, date_time)
        info.external_attr = (st.st_mode & 0xFFFF) << 16
        if directory:
            info.file_size = 0
            info.external_attr |= 0x10      # MS-DOS directory attribute
        else:
            info.file_size = st.st_size
        return info

    def is_dir(self):
        return self.filename.endswith('/')

    def _for_archive(self, archive):
        """Give a member created from a bare name the defaults it gets when
        written to *archive*: now (or SOURCE_DATE_EPOCH, in UTC) as its
        time, the archive's compression, and default permissions."""
        epoch = os.environ.get('SOURCE_DATE_EPOCH')
        stamp = time.gmtime(int(epoch)) if epoch else time.localtime()
        self.date_time = stamp[:6]
        self.compress_type = archive.compression
        self.compress_level = archive.compresslevel
        if self.is_dir():
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
    """The file object ZipFile.open(name, 'w') returns.

    Data is compressed straight into the archive.  close() completes the
    member: it writes a data descriptor (unseekable output) or goes back
    and rewrites the local header with the final sizes and CRC, then adds
    the member to the archive's directory.
    """

    def __init__(self, zf, zinfo, zip64):
        self._zipfile = zf
        self._zinfo = zinfo
        self._zip64 = zip64
        self._compressor = _get_compressor(zinfo.compress_type,
                                           zinfo.compress_level)
        self._file_size = 0         # bytes received
        self._compress_size = 0     # bytes written to the archive
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

    def _emit(self, data):
        self._fileobj.write(data)
        self._compress_size += len(data)

    def write(self, data):
        if self.closed:
            raise ValueError('I/O operation on closed file.')
        if not isinstance(data, (bytes, bytearray)):
            data = memoryview(data)
        size = data.nbytes if isinstance(data, memoryview) else len(data)
        self._file_size += size
        self._crc = crc32(data, self._crc)
        if self._compressor:
            data = self._compressor.compress(data)
        self._emit(data)
        return size

    def close(self):
        if self.closed:
            return
        try:
            super().close()
            if self._compressor:
                self._emit(self._compressor.flush())
            info = self._zinfo
            info.compress_size = self._compress_size
            info.file_size = self._file_size
            info.CRC = self._crc
            if not self._zip64:
                if self._file_size > ZIP64_LIMIT:
                    raise RuntimeError(
                        "File size too large, try using force_zip64")
                if self._compress_size > ZIP64_LIMIT:
                    raise RuntimeError(
                        "Compressed size too large, try using force_zip64")
            out = self._fileobj
            if info.flag_bits & _MASK_USE_DATA_DESCRIPTOR:
                layout = '<LLQQ' if self._zip64 else '<LLLL'
                out.write(struct.pack(layout, _DD_SIGNATURE, info.CRC,
                                      info.compress_size, info.file_size))
                self._zipfile.start_dir = out.tell()
            else:
                end = self._zipfile.start_dir = out.tell()
                out.seek(info.header_offset)
                out.write(info.FileHeader(self._zip64))
                out.seek(end)
            self._zipfile._register(info)
        finally:
            self._zipfile._writing = False


_FILE_MODES = {
    # archive mode -> modes to try in turn when opening a path
    'r': ('rb',),
    'w': ('w+b', 'wb'),
    'x': ('x+b', 'xb'),
    # appending to a file that does not exist yet creates it
    'a': ('r+b', 'w+b', 'wb'),
}


class ZipFile:
    """A ZIP archive.

    ZipFile(file, mode='r', compression=ZIP_STORED, allowZip64=True,
            compresslevel=None, *, strict_timestamps=True,
            metadata_encoding=None)

    *file* is a path or a file object; *mode* is 'r' (read), 'w'
    (truncate and write), 'x' (create a new file) or 'a' (append to an
    archive, or to any other file).  *compression* and *compresslevel*
    are the defaults for new members; *allowZip64* permits archives and
    members beyond the classic 4 GiB / 65535-entry limits;
    *strict_timestamps* refuses files dated outside 1980-2107 (instead of
    clamping); *metadata_encoding* decodes legacy member names when
    reading.
    """

    fp = None                                   # None once closed
    _windows_illegal_name_trans_table = None

    def __init__(self, file, mode="r", compression=ZIP_STORED, allowZip64=True,
                 compresslevel=None, *, strict_timestamps=True,
                 metadata_encoding=None):
        if mode not in _FILE_MODES:
            raise ValueError("ZipFile requires mode 'r', 'w', 'x', or 'a'")
        _check_compression(compression)
        if metadata_encoding and mode != 'r':
            raise ValueError(
                "metadata_encoding is only supported for reading files")

        self._allowZip64 = allowZip64
        self._didModify = False
        self.debug = 0
        self.NameToInfo = {}        # member name -> ZipInfo
        self.filelist = []          # ZipInfo objects in directory order
        self.compression = compression
        self.compresslevel = compresslevel
        self.mode = mode
        self._pwd = None
        self._comment = b''
        self._strict_timestamps = strict_timestamps
        self.metadata_encoding = metadata_encoding
        self._start_disk = 0        # bytes before the archive proper

        if isinstance(file, os.PathLike):
            file = os.fspath(file)
        if isinstance(file, str):
            self._filePassed = 0
            self.filename = file
            self.fp = self._open_path(file, _FILE_MODES[mode])
        else:
            self._filePassed = 1
            self.fp = file
            self.filename = getattr(file, 'name', None)
        self._fileRefCnt = 1        # the archive plus open member readers
        self._lock = threading.RLock()
        self._seekable = True
        self._writing = False

        try:
            if mode == 'r':
                self._RealGetContents()
            elif mode == 'a':
                self._prepare_append()
            else:
                self._prepare_write()
        except BaseException:
            fp, self.fp = self.fp, None
            self._fpclose(fp)
            raise

    @staticmethod
    def _open_path(path, modes):
        for i, filemode in enumerate(modes):
            try:
                return io.open(path, filemode)
            except OSError:
                if i == len(modes) - 1:
                    raise

    def _prepare_write(self):
        self._didModify = True
        try:
            # offsets in the archive count from where it starts
            self.start_dir = self._start_disk = self.fp.tell()
        except (AttributeError, OSError):
            # a pipe or socket: count the bytes ourselves
            self.fp = _CountingWriter(self.fp)
            self.start_dir = 0
            self._seekable = False
            return
        try:
            self.fp.seek(self.start_dir)
        except (AttributeError, OSError):
            self._seekable = False

    def _prepare_append(self):
        try:
            self._RealGetContents()
        except BadZipFile:
            # not an archive (e.g. an executable): put one at its end
            self.fp.seek(0, 2)
            self._didModify = True
            self.start_dir = self._start_disk = self.fp.tell()
        else:
            # new members replace the old central directory
            self.fp.seek(self.start_dir)

    def __enter__(self):
        return self

    def __exit__(self, type, value, traceback):
        self.close()

    def __repr__(self):
        cls = type(self)
        text = f'<{cls.__module__}.{cls.__qualname__}'
        if self.fp is None:
            return text + ' [closed]>'
        if self._filePassed:
            text += ' file=%r' % self.fp
        elif self.filename is not None:
            text += ' filename=%r' % self.filename
        return text + ' mode=%r>' % self.mode

    # -- reading the directory ---------------------------------------------

    def _RealGetContents(self):
        """Load the central directory into filelist/NameToInfo."""
        try:
            endrec = _find_end_record(self.fp)
        except OSError:
            endrec = None
        if not endrec:
            raise BadZipFile("File is not a zip file")
        if self.debug > 1:
            print(endrec)
        self._comment = endrec[_END_COMMENT]
        dir_size = endrec[_END_SIZE]
        dir_offset = endrec[_END_OFFSET]

        # Bytes in front of the archive (a self-extractor stub, say) shift
        # every stored offset: the directory really ends where the end
        # record (or the ZIP64 record) begins.
        prefix = endrec[_END_LOCATION] - dir_size - dir_offset
        if prefix < 0:
            raise BadZipFile("Bad offset for central directory")
        if self.debug > 2:
            print("given, inferred, offset",
                  dir_offset, prefix, dir_offset + prefix)
        self.start_dir = dir_offset + prefix
        self._start_disk = prefix

        self.fp.seek(self.start_dir)
        directory = self.fp.read(dir_size)
        pos = 0
        while pos < dir_size:
            info, pos = self._parse_directory_entry(directory, pos)
            info.header_offset += prefix
            self.filelist.append(info)
            self.NameToInfo[info.filename] = info

        # Each member's data must end before the next local header (or the
        # directory); open() uses this to spot overlapping members.
        # (Reversed rather than reverse=True: of several entries sharing one
        # local header, the first gets the zero-length limit and a warning.)
        limit = self.start_dir
        for info in reversed(sorted(self.filelist,
                                    key=lambda zi: zi.header_offset)):
            info._end_offset = limit
            limit = info.header_offset

    def _parse_directory_entry(self, directory, pos):
        """Decode the central directory record at *pos*; returns the
        ZipInfo and the position of the next record."""
        fixed = directory[pos:pos + sizeCentralDir]
        if len(fixed) != sizeCentralDir:
            raise BadZipFile("Truncated central directory")
        rec = struct.unpack(structCentralDir, fixed)
        if rec[_CEN_SIGNATURE] != stringCentralDir:
            raise BadZipFile("Bad magic number for central directory")
        if self.debug > 2:
            print(rec)
        pos += sizeCentralDir
        lengths = (rec[_CEN_FILENAME_LENGTH], rec[_CEN_EXTRA_FIELD_LENGTH],
                   rec[_CEN_COMMENT_LENGTH])
        raw_name = directory[pos:pos + lengths[0]]
        extra = directory[pos + lengths[0]:pos + lengths[0] + lengths[1]]
        comment = directory[pos + lengths[0] + lengths[1]:pos + sum(lengths)]
        pos += sum(lengths)

        flags = rec[_CEN_FLAG_BITS]
        if flags & _MASK_UTF_FILENAME:
            name = raw_name.decode('utf-8')
        else:
            name = raw_name.decode(self.metadata_encoding or 'cp437')
        info = ZipInfo(name)
        if not flags & _MASK_UTF_FILENAME:
            info._stored_name = (raw_name, info.filename)
        info.extra = extra
        info.comment = comment
        (info.create_version, info.create_system, info.extract_version,
         info.reserved, info.flag_bits, info.compress_type, dos_time,
         dos_date, info.CRC, info.compress_size,
         info.file_size) = rec[1:12]
        if info.extract_version > MAX_EXTRACT_VERSION:
            raise NotImplementedError("zip file version %.1f"
                                      % (info.extract_version / 10))
        info.volume, info.internal_attr, info.external_attr = rec[15:18]
        info.header_offset = rec[_CEN_LOCAL_HEADER_OFFSET]
        info._raw_time = dos_time
        info.date_time = _from_dos_stamp(dos_time, dos_date)
        info._decodeExtra(crc32(raw_name))
        if self.debug > 2:
            print("total", pos)
        return info, pos

    def _register(self, info):
        self.filelist.append(info)
        self.NameToInfo[info.filename] = info

    # -- queries -----------------------------------------------------------------

    def namelist(self):
        return [info.filename for info in self.filelist]

    def infolist(self):
        return self.filelist

    def printdir(self, file=None):
        """Print a table of the members to *file* (default stdout)."""
        print("%-46s %19s %12s" % ("File Name", "Modified    ", "Size"),
              file=file)
        for info in self.filelist:
            stamp = "%d-%02d-%02d %02d:%02d:%02d" % info.date_time[:6]
            print("%-46s %s %12d" % (info.filename, stamp, info.file_size),
                  file=file)

    def testzip(self):
        """Read every member to check its CRC; return the name of the
        first bad one, or None."""
        for info in self.filelist:
            try:
                with self.open(info.filename, "r") as member:
                    while member.read(1 << 20):
                        pass
            except BadZipFile:
                return info.filename
        return None

    def getinfo(self, name):
        info = self.NameToInfo.get(name)
        if info is None:
            raise KeyError('There is no item named %r in the archive' % name)
        return info

    @staticmethod
    def _check_pwd(pwd):
        if pwd and not isinstance(pwd, bytes):
            raise TypeError("pwd: expected bytes, got %s" % type(pwd).__name__)

    def setpassword(self, pwd):
        """Default password for encrypted members."""
        self._check_pwd(pwd)
        self._pwd = pwd or None

    @property
    def pwd(self):
        return self._pwd

    @pwd.setter
    def pwd(self, value):
        self._check_pwd(value)
        self._pwd = value

    @property
    def comment(self):
        """The archive comment (bytes)."""
        return self._comment

    @comment.setter
    def comment(self, comment):
        if not isinstance(comment, bytes):
            raise TypeError("comment: expected bytes, got %s"
                            % type(comment).__name__)
        if len(comment) > ZIP_MAX_COMMENT:
            import warnings
            warnings.warn('Archive comment is too long; truncating to %d bytes'
                          % ZIP_MAX_COMMENT, stacklevel=2)
            comment = comment[:ZIP_MAX_COMMENT]
        self._comment = comment
        self._didModify = True

    def read(self, name, pwd=None):
        """The contents of member *name*."""
        with self.open(name, "r", pwd) as member:
            return member.read()

    # -- opening members -------------------------------------------------------

    def open(self, name, mode="r", pwd=None, *, force_zip64=False):
        """A file object for member *name* (a name or a ZipInfo).

        Mode 'r' reads an existing member (*pwd* decrypts it); mode 'w'
        writes a new one (*force_zip64* for data that may grow beyond
        4 GiB when its size is not known in advance).
        """
        if mode not in ("r", "w"):
            raise ValueError('open() requires mode "r" or "w"')
        if pwd and mode == "w":
            raise ValueError("pwd is only supported for reading files")
        self._check_pwd(pwd)
        if not self.fp:
            raise ValueError(
                "Attempt to use ZIP archive that was already closed")

        if isinstance(name, ZipInfo):
            info = name
        elif mode == 'w':
            info = ZipInfo(name)
            info.compress_type = self.compression
            info.compress_level = self.compresslevel
        else:
            info = self.getinfo(name)

        if mode == 'w':
            return self._open_to_write(info, force_zip64=force_zip64)
        if self._writing:
            raise ValueError("Can't open %s while there is an open writing "
                             "handle on it. Close the writing handle before "
                             "trying to read." % (name,))

        self._fileRefCnt += 1
        view = _ArchiveView(self.fp, info.header_offset, self._fpclose,
                            self._lock, lambda: self._writing)
        try:
            self._check_local_header(view, info)
            return ZipExtFile(view, 'rb', info,
                              self._password_for(info, name, pwd), True)
        except BaseException:
            view.close()
            raise

    def _check_local_header(self, view, info):
        """Read the member's local header from *view* (leaving it at the
        data) and check it against the directory."""
        raw = view.read(sizeFileHeader)
        if len(raw) != sizeFileHeader:
            raise BadZipFile("Truncated file header")
        header = struct.unpack(structFileHeader, raw)
        if header[_LOC_SIGNATURE] != stringFileHeader:
            raise BadZipFile("Bad magic number for file header")
        raw_name = view.read(header[_LOC_FILENAME_LENGTH])
        if header[_LOC_EXTRA_FIELD_LENGTH]:
            view.seek(header[_LOC_EXTRA_FIELD_LENGTH], whence=1)

        if info.flag_bits & _MASK_COMPRESSED_PATCH:
            raise NotImplementedError("compressed patched data (flag bit 5)")
        if info.flag_bits & _MASK_STRONG_ENCRYPTION:
            raise NotImplementedError("strong encryption (flag bit 6)")

        if header[_LOC_GENERAL_PURPOSE_FLAG_BITS] & _MASK_UTF_FILENAME:
            name = raw_name.decode("utf-8")
        else:
            name = raw_name.decode(self.metadata_encoding or "cp437")
        if name != info.orig_filename:
            raise BadZipFile('File name in directory %r and header %r differ.'
                             % (info.orig_filename, raw_name))

        end = info._end_offset
        if end is not None and view.tell() + info.compress_size > end:
            text = (f"Overlapped entries: {info.orig_filename!r} "
                    f"(possible zip bomb)")
            if end != info.header_offset:
                raise BadZipFile(text)
            # several directory entries for one local header: suspicious
            # but readable
            import warnings
            warnings.warn(text, skip_file_prefixes=(os.path.dirname(__file__),))

    def _password_for(self, info, name, pwd):
        if not info.flag_bits & _MASK_ENCRYPTED:
            return None
        pwd = pwd or self._pwd
        self._check_pwd(pwd)
        if not pwd:
            raise RuntimeError("File %r is encrypted, password "
                               "required for extraction" % name)
        return pwd

    def _open_to_write(self, zinfo, force_zip64=False):
        if force_zip64 and not self._allowZip64:
            raise ValueError("force_zip64 is True, but allowZip64 was False "
                             "when opening the ZIP file.")
        if self._writing:
            raise ValueError("Can't write to the ZIP file while there is "
                             "another write handle open on it. "
                             "Close the first handle before opening another.")

        # Sizes and CRC are filled in when the member is closed.
        zinfo.compress_size = 0
        zinfo.CRC = 0
        zinfo.flag_bits = 0
        if zinfo.compress_type == ZIP_LZMA:
            zinfo.flag_bits |= _MASK_COMPRESS_OPTION_1
        if not self._seekable:
            zinfo.flag_bits |= _MASK_USE_DATA_DESCRIPTOR
        if not zinfo.external_attr:
            zinfo.external_attr = 0o600 << 16       # rw-------

        # A known size close to the limit gets ZIP64 headers up front (the
        # compressed form may be a little larger than the data).
        zip64 = force_zip64 or zinfo.file_size * 1.05 > ZIP64_LIMIT
        if zip64 and not self._allowZip64:
            raise LargeZipFile("Filesize would require ZIP64 extensions")

        self._seek_to_append()
        zinfo.header_offset = self.fp.tell()
        self._writecheck(zinfo)
        self._didModify = True
        self.fp.write(zinfo.FileHeader(zip64))
        self._writing = True
        return _MemberWriter(self, zinfo, zip64)

    def _seek_to_append(self):
        # readers may have moved the shared file position
        if self._seekable:
            self.fp.seek(self.start_dir)

    # -- extracting --------------------------------------------------------------

    def extract(self, member, path=None, pwd=None):
        """Extract *member* below *path* (default: the current directory);
        returns the path created."""
        path = os.getcwd() if path is None else os.fspath(path)
        return self._extract_member(member, path, pwd)

    def extractall(self, path=None, members=None, pwd=None):
        """Extract *members* (default: all) below *path*."""
        if members is None:
            members = self.namelist()
        path = os.getcwd() if path is None else os.fspath(path)
        for member in members:
            self._extract_member(member, path, pwd)

    @classmethod
    def _sanitize_windows_name(cls, arcname, pathsep):
        """Make *arcname* usable on Windows: reserved characters become
        '_', trailing dots and spaces go, empty components disappear."""
        table = cls._windows_illegal_name_trans_table
        if table is None:
            reserved = ':<>|"?*'
            table = str.maketrans(reserved, '_' * len(reserved))
            cls._windows_illegal_name_trans_table = table
        parts = (part.rstrip(' .')
                 for part in arcname.translate(table).split(pathsep))
        return pathsep.join(part for part in parts if part)

    @staticmethod
    def _safe_relative_path(filename):
        """The member name as a relative path that cannot leave the target
        directory: no drive, no root, no '.' or '..' components."""
        path = filename.replace('/', os.path.sep)
        if os.path.altsep:
            path = path.replace(os.path.altsep, os.path.sep)
        path = os.path.splitdrive(path)[1]
        skip = ('', os.path.curdir, os.path.pardir)
        return os.path.sep.join(part for part in path.split(os.path.sep)
                                if part not in skip)

    def _extract_member(self, member, targetpath, pwd):
        if not isinstance(member, ZipInfo):
            member = self.getinfo(member)
        relative = self._safe_relative_path(member.filename)
        if os.path.sep == '\\':
            relative = self._sanitize_windows_name(relative, os.path.sep)
        if not relative and not member.is_dir():
            raise ValueError("Empty filename.")

        target = os.path.normpath(os.path.join(targetpath, relative))
        parent = os.path.dirname(target)
        if parent and not os.path.exists(parent):
            os.makedirs(parent, exist_ok=True)

        if member.is_dir():
            if not os.path.isdir(target):
                try:
                    os.mkdir(target)
                except FileExistsError:
                    if not os.path.isdir(target):
                        raise
            return target

        with self.open(member, pwd=pwd) as source, \
                open(target, "wb") as sink:
            shutil.copyfileobj(source, sink)
        return target

    # -- writing ---------------------------------------------------------------

    def _writecheck(self, zinfo):
        """Refuse to add *zinfo* when that is impossible."""
        if zinfo.filename in self.NameToInfo:
            import warnings
            warnings.warn('Duplicate name: %r' % zinfo.filename, stacklevel=3)
        if self.mode not in ('w', 'x', 'a'):
            raise ValueError("write() requires mode 'w', 'x', or 'a'")
        if not self.fp:
            raise ValueError(
                "Attempt to write ZIP archive that was already closed")
        _check_compression(zinfo.compress_type)
        if self._allowZip64:
            return
        if len(self.filelist) >= ZIP_FILECOUNT_LIMIT:
            what = "Files count"
        elif zinfo.file_size > ZIP64_LIMIT:
            what = "Filesize"
        elif zinfo.header_offset > ZIP64_LIMIT:
            what = "Zipfile size"
        else:
            return
        raise LargeZipFile(what + " would require ZIP64 extensions")

    def _check_can_add(self, closed_message):
        if not self.fp:
            raise ValueError(closed_message)
        if self._writing:
            raise ValueError("Can't write to ZIP archive while an open "
                             "writing handle exists")

    def write(self, filename, arcname=None,
              compress_type=None, compresslevel=None):
        """Add the file *filename* as *arcname* (default: its path)."""
        self._check_can_add(
            "Attempt to write to ZIP archive that was already closed")
        zinfo = ZipInfo.from_file(filename, arcname,
                                  strict_timestamps=self._strict_timestamps)
        if zinfo.is_dir():
            zinfo.compress_size = 0
            zinfo.CRC = 0
            self.mkdir(zinfo)
            return
        zinfo.compress_type = (self.compression if compress_type is None
                               else compress_type)
        zinfo.compress_level = (self.compresslevel if compresslevel is None
                                else compresslevel)
        with open(filename, "rb") as source, self.open(zinfo, 'w') as sink:
            shutil.copyfileobj(source, sink, 1024 * 8)

    def writestr(self, zinfo_or_arcname, data,
                 compress_type=None, compresslevel=None):
        """Add a member holding *data* (bytes, or str stored as UTF-8)."""
        if isinstance(data, str):
            data = data.encode("utf-8")
        if isinstance(zinfo_or_arcname, ZipInfo):
            zinfo = zinfo_or_arcname
        else:
            zinfo = ZipInfo(zinfo_or_arcname)._for_archive(self)
        self._check_can_add(
            "Attempt to write to ZIP archive that was already closed")
        if compress_type is not None:
            zinfo.compress_type = compress_type
        if compresslevel is not None:
            zinfo.compress_level = compresslevel
        zinfo.file_size = len(data)
        with self._lock:
            with self.open(zinfo, mode='w') as sink:
                sink.write(data)

    def mkdir(self, zinfo_or_directory_name, mode=511):
        """Add a directory member (a ZipInfo, or a name and permissions)."""
        if isinstance(zinfo_or_directory_name, ZipInfo):
            zinfo = zinfo_or_directory_name
            if not zinfo.is_dir():
                raise ValueError(
                    "The given ZipInfo does not describe a directory")
        elif isinstance(zinfo_or_directory_name, str):
            name = zinfo_or_directory_name
            if not name.endswith('/'):
                name += '/'
            zinfo = ZipInfo(name)       # the default (1980) timestamp
            zinfo.compress_size = zinfo.CRC = zinfo.file_size = 0
            zinfo.external_attr = (((stat.S_IFDIR | mode) & 0xFFFF) << 16
                                   | 0x10)
        else:
            raise TypeError("Expected type str or ZipInfo")

        with self._lock:
            self._check_can_add(
                "Attempt to write to ZIP archive that was already closed")
            self._seek_to_append()
            zinfo.header_offset = self.fp.tell()
            if zinfo.compress_type == ZIP_LZMA:
                zinfo.flag_bits |= _MASK_COMPRESS_OPTION_1
            self._writecheck(zinfo)
            self._didModify = True
            self._register(zinfo)
            self.fp.write(zinfo.FileHeader(False))
            self.start_dir = self.fp.tell()

    # -- closing -----------------------------------------------------------------

    def __del__(self):
        self.close()

    def close(self):
        """Write the central directory if anything changed, then close the
        file (unless it was passed in, or member readers still use it)."""
        if self.fp is None:
            return
        if self._writing:
            raise ValueError("Can't close the ZIP file while there is "
                             "an open writing handle on it. "
                             "Close the writing handle before closing the zip.")
        try:
            if self.mode in ('w', 'x', 'a') and self._didModify:
                with self._lock:
                    self._seek_to_append()
                    self._write_end_record()
        finally:
            fp, self.fp = self.fp, None
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
