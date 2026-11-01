"""Read and write Apple property list (.plist) files.

Property lists hold dictionaries (str keys), lists, str, bytes, int,
float, bool, datetime.datetime and, in the binary format only, UID
values.  Two encodings exist: XML (FMT_XML, the default for writing) and
Apple's binary format (FMT_BINARY); the reader detects which one it gets.

    >>> import plistlib
    >>> data = plistlib.dumps({"name": "demo", "sizes": [1, 2]})
    >>> plistlib.loads(data)
    {'name': 'demo', 'sizes': [1, 2]}
"""
__all__ = [
    "InvalidFileException", "FMT_XML", "FMT_BINARY", "load", "dump",
    "loads", "dumps", "UID",
]

import binascii
import codecs
import datetime
import enum
import itertools
import os
import re
import struct
from io import BytesIO

PlistFormat = enum.Enum('PlistFormat', 'FMT_XML FMT_BINARY',
                        module=__name__)
globals().update(PlistFormat.__members__)


class UID:
    """Reference to another object inside a binary plist (used by keyed
    archives)."""

    def __init__(self, data):
        if not isinstance(data, int):
            raise TypeError("data must be an int")
        if data >= 1 << 64:
            raise ValueError("UIDs cannot be >= 2**64")
        if data < 0:
            raise ValueError("UIDs must be positive")
        self.data = data

    def __index__(self):
        return self.data

    def __repr__(self):
        return f"{type(self).__name__}({self.data!r})"

    def __reduce__(self):
        return self.__class__, (self.data,)

    def __eq__(self, other):
        if not isinstance(other, UID):
            return NotImplemented
        return self.data == other.data

    def __hash__(self):
        return hash(self.data)


class InvalidFileException(ValueError):
    def __init__(self, message="Invalid file"):
        ValueError.__init__(self, message)


# --------------------------------------------------------------------------
# XML format

PLISTHEADER = b"""\
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
"""

# characters XML 1.0 cannot represent (\t, \n and \r are fine)
_ILLEGAL_XML_CHARS = re.compile(r"[\x00-\x08\x0b\x0c\x0e-\x1f]")

_DATE_PATTERN = re.compile(
    r"(?P<year>\d\d\d\d)(?:-(?P<month>\d\d)(?:-(?P<day>\d\d)"
    r"(?:T(?P<hour>\d\d)(?::(?P<minute>\d\d)(?::(?P<second>\d\d))?)?)?)?)?Z",
    re.ASCII)


def _date_from_string(text, aware_datetime):
    found = _DATE_PATTERN.match(text)
    if found is None:
        raise ValueError(f"invalid date: {text!r}")
    parts = []
    for name in ("year", "month", "day", "hour", "minute", "second"):
        value = found.group(name)
        if value is None:
            break
        parts.append(int(value))
    while len(parts) < 3:
        parts.append(1)
    if aware_datetime:
        return datetime.datetime(*parts, tzinfo=datetime.UTC)
    return datetime.datetime(*parts)


def _date_to_string(value, aware_datetime):
    if aware_datetime:
        value = value.astimezone(datetime.UTC)
    return (f"{value.year:04d}-{value.month:02d}-{value.day:02d}T"
            f"{value.hour:02d}:{value.minute:02d}:{value.second:02d}Z")


def _escape(text):
    if _ILLEGAL_XML_CHARS.search(text) is not None:
        raise ValueError("strings can't contain control characters; "
                         "use bytes instead")
    text = text.replace("\r\n", "\n").replace("\r", "\n")
    return (text.replace("&", "&amp;").replace("<", "&lt;")
            .replace(">", "&gt;"))


class _PlistParser:
    """Builds Python objects from XML plist events (via expat)."""

    def __init__(self, dict_type, aware_datetime=False):
        self.stack = []
        self.current_key = None
        self.root = None
        self._dict_type = dict_type
        self._aware_datetime = aware_datetime

    def parse(self, fileobj):
        from xml.parsers.expat import ParserCreate
        self.parser = ParserCreate()
        self.parser.StartElementHandler = self.handle_begin_element
        self.parser.EndElementHandler = self.handle_end_element
        self.parser.CharacterDataHandler = self.handle_data
        self.parser.EntityDeclHandler = self.handle_entity_decl
        self.parser.ParseFile(fileobj)
        return self.root

    def handle_entity_decl(self, entity_name, is_parameter_entity, value,
                           base, system_id, public_id, notation_name):
        # entity expansion is a classic attack vector and not part of the
        # plist format
        raise InvalidFileException(
            "XML entity declarations are not supported in plist files")

    def handle_begin_element(self, element, attrs):
        self.data = []
        handler = getattr(self, "begin_" + element, None)
        if handler is not None:
            handler(attrs)

    def handle_end_element(self, element):
        handler = getattr(self, "end_" + element, None)
        if handler is not None:
            handler()

    def handle_data(self, data):
        self.data.append(data)

    def _line(self):
        return self.parser.CurrentLineNumber

    def add_object(self, value):
        if self.current_key is not None:
            if not isinstance(self.stack[-1], dict):
                raise ValueError("unexpected element at line %d"
                                 % self._line())
            self.stack[-1][self.current_key] = value
            self.current_key = None
        elif not self.stack:
            self.root = value
        else:
            if not isinstance(self.stack[-1], list):
                raise ValueError("unexpected element at line %d"
                                 % self._line())
            self.stack[-1].append(value)

    def get_data(self):
        data = ''.join(self.data)
        self.data = []
        return data

    def begin_dict(self, attrs):
        container = self._dict_type()
        self.add_object(container)
        self.stack.append(container)

    def end_dict(self):
        if self.current_key:
            raise ValueError("missing value for key '%s' at line %d"
                             % (self.current_key, self._line()))
        self.stack.pop()

    def end_key(self):
        if self.current_key or not isinstance(self.stack[-1], dict):
            raise ValueError("unexpected key at line %d" % self._line())
        self.current_key = self.get_data()

    def begin_array(self, attrs):
        container = []
        self.add_object(container)
        self.stack.append(container)

    def end_array(self):
        self.stack.pop()

    def end_true(self):
        self.add_object(True)

    def end_false(self):
        self.add_object(False)

    def end_integer(self):
        raw = self.get_data()
        if raw.startswith(("0x", "0X")):
            self.add_object(int(raw, 16))
        else:
            self.add_object(int(raw))

    def end_real(self):
        self.add_object(float(self.get_data()))

    def end_string(self):
        self.add_object(self.get_data())

    def end_data(self):
        self.add_object(binascii.a2b_base64(self.get_data().encode("utf-8")))

    def end_date(self):
        self.add_object(_date_from_string(self.get_data(),
                                          self._aware_datetime))


class _DumbXMLWriter:
    def __init__(self, file, indent_level=0, indent=b"\t"):
        self.file = file
        self.stack = []
        self._indent_level = indent_level
        self.indent = indent

    def begin_element(self, element):
        self.stack.append(element)
        self.writeln(f"<{element}>")
        self._indent_level += 1

    def end_element(self, element):
        assert self._indent_level > 0
        assert self.stack.pop() == element
        self._indent_level -= 1
        self.writeln(f"</{element}>")

    def simple_element(self, element, value=None):
        if value is None:
            self.writeln(f"<{element}/>")
        else:
            self.writeln(f"<{element}>{_escape(value)}</{element}>")

    def writeln(self, line):
        if line:
            if isinstance(line, str):
                line = line.encode("utf-8")
            self.file.write(self._indent_level * self.indent)
            self.file.write(line)
        self.file.write(b"\n")


class _PlistWriter(_DumbXMLWriter):
    def __init__(self, file, indent_level=0, indent=b"\t", writeHeader=1,
                 sort_keys=True, skipkeys=False, aware_datetime=False):
        if writeHeader:
            file.write(PLISTHEADER)
        _DumbXMLWriter.__init__(self, file, indent_level, indent)
        self._sort_keys = sort_keys
        self._skipkeys = skipkeys
        self._aware_datetime = aware_datetime

    def write(self, value):
        self.writeln('<plist version="1.0">')
        self.write_value(value)
        self.writeln("</plist>")

    def write_value(self, value):
        if isinstance(value, str):
            self.simple_element("string", value)
        elif value is True:
            self.simple_element("true")
        elif value is False:
            self.simple_element("false")
        elif isinstance(value, int):
            if -1 << 63 <= value < 1 << 64:
                self.simple_element("integer", "%d" % value)
            else:
                raise OverflowError(value)
        elif isinstance(value, float):
            self.simple_element("real", repr(value))
        elif isinstance(value, (dict, frozendict)):
            self.write_dict(value)
        elif isinstance(value, (bytes, bytearray)):
            self.write_bytes(value)
        elif isinstance(value, datetime.datetime):
            self.simple_element("date", _date_to_string(
                value, self._aware_datetime))
        elif isinstance(value, (tuple, list)):
            self.write_array(value)
        else:
            raise TypeError("unsupported type: %s" % type(value))

    def write_bytes(self, data):
        self.begin_element("data")
        self._indent_level -= 1
        # base64 lines fill the width left after the indentation
        width = len((self.indent * self._indent_level).expandtabs())
        line_chars = max(16, 76 - width)
        chunk = (line_chars // 4) * 3
        for start in range(0, len(data), chunk):
            encoded = binascii.b2a_base64(data[start:start + chunk],
                                          newline=False)
            self.writeln(encoded)
        self._indent_level += 1
        self.end_element("data")

    def write_dict(self, d):
        if not d:
            self.simple_element("dict")
            return
        self.begin_element("dict")
        for key, value in _dict_items(d, self._sort_keys, self._skipkeys):
            self.simple_element("key", key)
            self.write_value(value)
        self.end_element("dict")

    def write_array(self, array):
        if not array:
            self.simple_element("array")
            return
        self.begin_element("array")
        for value in array:
            self.write_value(value)
        self.end_element("array")


def _dict_items(d, sort_keys, skipkeys):
    """The (str key, value) pairs to write for a dictionary."""
    items = []
    for key, value in d.items():
        if not isinstance(key, str):
            if skipkeys:
                continue
            raise TypeError("keys must be strings")
        items.append((key, value))
    if sort_keys:
        items.sort(key=lambda item: item[0])
    return items


def _is_fmt_xml(header):
    starts = (b"<?xml", b"<plist")
    if header.startswith(starts):
        return True
    for bom, encoding in ((codecs.BOM_UTF8, "utf-8"),
                          (codecs.BOM_UTF16_BE, "utf-16-be"),
                          (codecs.BOM_UTF16_LE, "utf-16-le")):
        if not header.startswith(bom):
            continue
        for start in starts:
            prefix = bom + start.decode("ascii").encode(encoding)
            if header[:len(prefix)] == prefix:
                return True
    return False


# --------------------------------------------------------------------------
# binary format

_UNDEFINED = object()

# struct format letters for 1-, 2-, 4- and 8-byte unsigned integers
_UINT_FORMATS = {1: "B", 2: "H", 4: "L", 8: "Q"}

# seconds from the Unix epoch to the plist epoch (2001-01-01)
_PLIST_EPOCH = datetime.datetime(2001, 1, 1)


def _is_fmt_binary(header):
    return header[:8] == b"bplist00"


class _BinaryPlistParser:
    """Reader for Apple's bplist00 format.

    Layout: header, objects, offset table (one offset per object), and a
    32-byte trailer giving the offset/reference sizes, the object count,
    the top object and the offset table position.
    """

    def __init__(self, dict_type, aware_datetime=False):
        self._dict_type = dict_type
        self._aware_datetime = aware_datetime

    def parse(self, fp):
        try:
            self._fp = fp
            fp.seek(0, os.SEEK_END)
            self._size = fp.tell()
            if self._size < 8 + 32:
                raise InvalidFileException()
            fp.seek(-32, os.SEEK_END)
            trailer = fp.read(32)
            (offset_size, self._ref_size, count, top,
             table_at) = struct.unpack(">6xBBQQQ", trailer)
            if (offset_size == 0 or self._ref_size == 0 or count == 0
                    or top >= count or table_at < 8
                    or table_at + count * offset_size > self._size - 32):
                raise InvalidFileException()
            self._table_at = table_at
            fp.seek(table_at)
            self._offsets = self._read_ints(count, offset_size)
            if any(not 8 <= offset < table_at for offset in self._offsets):
                raise InvalidFileException()
            self._objects = [_UNDEFINED] * count
            return self._read_object(top)
        except (OSError, IndexError, struct.error, OverflowError,
                ValueError, TypeError) as exc:
            if isinstance(exc, InvalidFileException):
                raise
            raise InvalidFileException() from None

    def _read(self, size):
        if size < 0 or self._fp.tell() + size > self._table_at:
            raise InvalidFileException()
        data = self._fp.read(size)
        if len(data) != size:
            raise InvalidFileException()
        return data

    def _read_ints(self, count, size):
        if count * size > self._size:
            raise InvalidFileException()
        data = self._fp.read(count * size)
        if len(data) != count * size:
            raise InvalidFileException()
        if size in _UINT_FORMATS:
            return struct.unpack(f">{count}{_UINT_FORMATS[size]}", data)
        return tuple(int.from_bytes(data[i:i + size], "big")
                     for i in range(0, count * size, size))

    def _read_refs(self, count):
        if count * self._ref_size > self._table_at:
            raise InvalidFileException()
        return self._read_ints(count, self._ref_size)

    def _read_size(self, low):
        """Element count encoded in the low nibble or, when that is 0xF,
        in a following integer object."""
        if low != 0xF:
            return low
        marker = self._read(1)[0]
        if marker & 0xF0 != 0x10:
            raise InvalidFileException()
        width = 1 << (marker & 0x3)
        return int.from_bytes(self._read(width), "big")

    def _read_object(self, ref):
        cached = self._objects[ref]
        if cached is not _UNDEFINED:
            return cached
        self._fp.seek(self._offsets[ref])
        token = self._read(1)[0]
        high, low = token & 0xF0, token & 0x0F

        if token == 0x00:
            result = None
        elif token == 0x08:
            result = False
        elif token == 0x09:
            result = True
        elif token == 0x0F:
            result = b""
        elif high == 0x10:
            width = 1 << low
            if low > 4:
                raise InvalidFileException()
            # 8- and 16-byte integers are signed, shorter ones unsigned
            result = int.from_bytes(self._read(width), "big",
                                    signed=width >= 8)
        elif token == 0x22:
            result = struct.unpack(">f", self._read(4))[0]
        elif token == 0x23:
            result = struct.unpack(">d", self._read(8))[0]
        elif token == 0x33:
            seconds = struct.unpack(">d", self._read(8))[0]
            result = _PLIST_EPOCH + datetime.timedelta(seconds=seconds)
            if self._aware_datetime:
                result = result.replace(tzinfo=datetime.UTC)
        elif high == 0x40:
            result = self._read(self._read_size(low))
        elif high == 0x50:
            result = self._read(self._read_size(low)).decode("ascii")
        elif high == 0x60:
            size = self._read_size(low)
            result = self._read(size * 2).decode("utf-16be")
        elif high == 0x80:
            result = UID(int.from_bytes(self._read(1 + low), "big"))
        elif high == 0xA0:
            refs = self._read_refs(self._read_size(low))
            result = []
            self._objects[ref] = result
            for item in refs:
                result.append(self._read_object(item))
        elif high == 0xD0:
            size = self._read_size(low)
            keys = self._read_refs(size)
            values = self._read_refs(size)
            result = self._dict_type()
            self._objects[ref] = result
            for key, value in zip(keys, values):
                result[self._read_object(key)] = self._read_object(value)
        else:
            raise InvalidFileException()

        self._objects[ref] = result
        return result


def _width_for(value):
    """Bytes needed for an unsigned integer up to *value*."""
    if value < 1 << 8:
        return 1
    if value < 1 << 16:
        return 2
    if value < 1 << 32:
        return 4
    return 8


# immutable values that are stored only once per plist
_SHARED_TYPES = (str, int, float, datetime.datetime, bytes)


class _BinaryPlistWriter:
    def __init__(self, fp, sort_keys, skipkeys, aware_datetime=False):
        self._fp = fp
        self._sort_keys = sort_keys
        self._skipkeys = skipkeys
        self._aware_datetime = aware_datetime

    def write(self, value):
        self._objects = []          # in output order
        self._by_value = {}         # (type, value) -> index, shared types
        self._by_id = {}            # id -> index, everything else
        self._collect(value)

        count = len(self._objects)
        self._offsets = [0] * count
        self._ref_width = _width_for(count)
        self._ref_format = _UINT_FORMATS[self._ref_width]

        self._fp.write(b"bplist00")
        for obj in self._objects:
            self._write_object(obj)

        top = self._index(value)
        table_at = self._fp.tell()
        offset_width = _width_for(table_at)
        self._fp.write(struct.pack(
            ">" + _UINT_FORMATS[offset_width] * count, *self._offsets))
        self._fp.write(struct.pack(">6xBBQQQ", offset_width, self._ref_width,
                                   count, top, table_at))

    def _index(self, value):
        if isinstance(value, _SHARED_TYPES):
            return self._by_value[(type(value), value)]
        return self._by_id[id(value)]

    def _collect(self, value):
        """Assign object numbers depth-first; dictionaries list all their
        keys before their values."""
        if isinstance(value, _SHARED_TYPES):
            key = (type(value), value)
            if key in self._by_value:
                return
            self._by_value[key] = len(self._objects)
        else:
            if id(value) in self._by_id:
                return
            self._by_id[id(value)] = len(self._objects)
        self._objects.append(value)

        if isinstance(value, (dict, frozendict)):
            items = _dict_items(value, self._sort_keys, self._skipkeys)
            for item in itertools.chain((k for k, _ in items),
                                        (v for _, v in items)):
                self._collect(item)
        elif isinstance(value, (list, tuple)):
            for item in value:
                self._collect(item)

    def _write_size(self, token, size):
        if size < 15:
            self._fp.write(struct.pack(">B", token | size))
        elif size < 1 << 8:
            self._fp.write(struct.pack(">BBB", token | 0xF, 0x10, size))
        elif size < 1 << 16:
            self._fp.write(struct.pack(">BBH", token | 0xF, 0x11, size))
        elif size < 1 << 32:
            self._fp.write(struct.pack(">BBL", token | 0xF, 0x12, size))
        else:
            self._fp.write(struct.pack(">BBQ", token | 0xF, 0x13, size))

    def _write_refs(self, values):
        refs = [self._index(v) for v in values]
        self._fp.write(struct.pack(">" + self._ref_format * len(refs),
                                   *refs))

    def _write_object(self, value):
        self._offsets[self._index(value)] = self._fp.tell()
        write = self._fp.write
        if value is None:
            write(b"\x00")
        elif value is False:
            write(b"\x08")
        elif value is True:
            write(b"\x09")
        elif isinstance(value, int):
            if value < 0:
                try:
                    write(struct.pack(">Bq", 0x13, value))
                except struct.error:
                    raise OverflowError(value) from None
            elif value < 1 << 8:
                write(struct.pack(">BB", 0x10, value))
            elif value < 1 << 16:
                write(struct.pack(">BH", 0x11, value))
            elif value < 1 << 32:
                write(struct.pack(">BL", 0x12, value))
            elif value < 1 << 63:
                write(struct.pack(">BQ", 0x13, value))
            elif value < 1 << 64:
                write(b"\x14" + value.to_bytes(16, "big", signed=True))
            else:
                raise OverflowError(value)
        elif isinstance(value, float):
            write(struct.pack(">Bd", 0x23, value))
        elif isinstance(value, datetime.datetime):
            if self._aware_datetime:
                utc = value.astimezone(datetime.UTC)
                delta = utc - _PLIST_EPOCH.replace(tzinfo=datetime.UTC)
            else:
                delta = value - _PLIST_EPOCH
            write(struct.pack(">Bd", 0x33, delta.total_seconds()))
        elif isinstance(value, (bytes, bytearray)):
            self._write_size(0x40, len(value))
            write(value)
        elif isinstance(value, str):
            try:
                encoded = value.encode("ascii")
                self._write_size(0x50, len(value))
            except UnicodeEncodeError:
                encoded = value.encode("utf-16be")
                self._write_size(0x60, len(encoded) // 2)
            write(encoded)
        elif isinstance(value, UID):
            if value.data < 0:
                raise ValueError("UIDs must be positive")
            elif value.data < 1 << 8:
                write(struct.pack(">BB", 0x80, value))
            elif value.data < 1 << 16:
                write(struct.pack(">BH", 0x81, value))
            elif value.data < 1 << 32:
                write(struct.pack(">BL", 0x83, value))
            elif value.data < 1 << 64:
                write(struct.pack(">BQ", 0x87, value))
            else:
                raise OverflowError(value)
        elif isinstance(value, (list, tuple)):
            self._write_size(0xA0, len(value))
            self._write_refs(value)
        elif isinstance(value, (dict, frozendict)):
            items = _dict_items(value, self._sort_keys, self._skipkeys)
            self._write_size(0xD0, len(items))
            self._write_refs([k for k, _ in items])
            self._write_refs([v for _, v in items])
        else:
            raise TypeError(value)


# --------------------------------------------------------------------------
# public API

_FORMATS = {
    FMT_XML: dict(detect=_is_fmt_xml, parser=_PlistParser,
                  writer=_PlistWriter),
    FMT_BINARY: dict(detect=_is_fmt_binary, parser=_BinaryPlistParser,
                     writer=_BinaryPlistWriter),
}


def load(fp, *, fmt=None, dict_type=dict, aware_datetime=False):
    """Read a plist from the binary file *fp* (format detected unless
    *fmt* is given)."""
    if fmt is None:
        header = fp.read(32)
        fp.seek(0)
        for info in _FORMATS.values():
            if info["detect"](header):
                parser_class = info["parser"]
                break
        else:
            raise InvalidFileException()
    else:
        parser_class = _FORMATS[fmt]["parser"]
    parser = parser_class(dict_type=dict_type, aware_datetime=aware_datetime)
    return parser.parse(fp)


def loads(value, *, fmt=None, dict_type=dict, aware_datetime=False):
    """Read a plist from bytes (or, for XML, a str)."""
    if isinstance(value, str):
        if fmt == FMT_BINARY:
            raise TypeError("value must be bytes-like object when fmt is "
                            "FMT_BINARY")
        value = value.encode()
    return load(BytesIO(value), fmt=fmt, dict_type=dict_type,
                aware_datetime=aware_datetime)


def dump(value, fp, *, fmt=FMT_XML, sort_keys=True, skipkeys=False,
         aware_datetime=False):
    """Write *value* as a plist to the binary file *fp*."""
    if fmt not in _FORMATS:
        raise ValueError(f"Unsupported format: {fmt!r}")
    writer = _FORMATS[fmt]["writer"](fp, sort_keys=sort_keys,
                                     skipkeys=skipkeys,
                                     aware_datetime=aware_datetime)
    writer.write(value)


def dumps(value, *, fmt=FMT_XML, skipkeys=False, sort_keys=True,
          aware_datetime=False):
    """Return *value* as plist bytes."""
    fp = BytesIO()
    dump(value, fp, fmt=fmt, skipkeys=skipkeys, sort_keys=sort_keys,
         aware_datetime=aware_datetime)
    return fp.getvalue()
