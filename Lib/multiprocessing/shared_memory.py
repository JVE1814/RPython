"""Shared memory blocks (POSIX shm) usable from several processes, and a
fixed-layout list of simple values stored in one."""

__all__ = ['SharedMemory', 'ShareableList']

from functools import partial
import mmap
import os
import errno
import struct
import types

if os.name == "nt":
    import _winapi
    _USE_POSIX = False
else:
    import _posixshmem
    _USE_POSIX = True

from . import resource_tracker

_O_CREX = os.O_CREAT | os.O_EXCL

# Some platforms limit shared memory names to 14 characters.
_SHM_SAFE_NAME_LENGTH = 14

if _USE_POSIX:
    _SHM_NAME_PREFIX = '/psm_'
else:
    _SHM_NAME_PREFIX = 'wnsm_'


def _make_filename():
    """A random name for a new segment."""
    nbytes = (_SHM_SAFE_NAME_LENGTH - len(_SHM_NAME_PREFIX)) // 2
    assert nbytes >= 2, '_SHM_NAME_PREFIX too long'
    name = _SHM_NAME_PREFIX + os.urandom(nbytes).hex()
    assert len(name) <= _SHM_SAFE_NAME_LENGTH
    return name


class SharedMemory:
    """A named block of shared memory.

    SharedMemory(create=True, size=n) creates a new block with a random
    name; SharedMemory(name) attaches to an existing one.  With *track*,
    the resource tracker unlinks the block if every process using it dies
    without calling unlink()."""

    _name = None
    _fd = -1
    _mmap = None
    _buf = None
    _flags = os.O_RDWR
    _mode = 0o600
    _prepend_leading_slash = True if _USE_POSIX else False
    _track = True

    def __init__(self, name=None, create=False, size=0, *, track=True):
        if not size >= 0:
            raise ValueError("'size' must be a positive integer")
        if create:
            self._flags = _O_CREX | os.O_RDWR
            if size == 0:
                raise ValueError("'size' must be a positive number different from zero")
        if name is None and not self._flags & os.O_EXCL:
            raise ValueError("'name' can only be None if create=True")

        self._track = track
        if not _USE_POSIX:
            raise NotImplementedError("shared memory needs POSIX shm")

        if name is None:
            while True:
                name = _make_filename()
                try:
                    self._fd = _posixshmem.shm_open(name, self._flags,
                                                    mode=self._mode)
                except FileExistsError:
                    continue
                self._name = name
                break
        else:
            name = "/" + name if self._prepend_leading_slash else name
            self._fd = _posixshmem.shm_open(name, self._flags, mode=self._mode)
            self._name = name
        try:
            if create and size:
                os.ftruncate(self._fd, size)
            size = os.fstat(self._fd).st_size
            self._mmap = mmap.mmap(self._fd, size)
        except OSError:
            self.unlink()
            raise
        if self._track:
            resource_tracker.register(self._name, "shared_memory")

        self._size = size
        self._buf = memoryview(self._mmap)

    def __del__(self):
        try:
            self.close()
        except OSError:
            pass

    def __reduce__(self):
        return (self.__class__, (self.name, False, self.size))

    def __repr__(self):
        return f'{self.__class__.__name__}({self.name!r}, size={self.size})'

    @property
    def buf(self):
        """A memoryview of the block."""
        return self._buf

    @property
    def name(self):
        """The name that other processes attach with."""
        reported = self._name
        if _USE_POSIX and self._prepend_leading_slash:
            if self._name.startswith("/"):
                reported = self._name[1:]
        return reported

    @property
    def size(self):
        """Size of the block in bytes."""
        return self._size

    def close(self):
        """Detach from the block (it stays available to others)."""
        if self._buf is not None:
            self._buf.release()
            self._buf = None
        if self._mmap is not None:
            self._mmap.close()
            self._mmap = None
        if _USE_POSIX and self._fd >= 0:
            os.close(self._fd)
            self._fd = -1

    def unlink(self):
        """Remove the block's name (call once, from one process)."""
        if _USE_POSIX and self._name:
            _posixshmem.shm_unlink(self._name)
            if self._track:
                resource_tracker.unregister(self._name, "shared_memory")


_encoding = "utf8"


class ShareableList:
    """A list of int/float/bool/str/bytes/None values with a fixed number
    of slots, stored in a SharedMemory block.

    str and bytes slots keep the capacity they were created with; the type
    stored in a slot may change.  Block layout:

        n                       (q)
        n slot end offsets      (q each, relative to the data area)
        data area               (8 bytes per number, capacity per str/bytes)
        n struct formats        (8s each)
        n value kinds           (b each: how to rebuild the value)
    """

    _types_mapping = {
        int: "q",
        float: "d",
        bool: "xxxxxxx?",
        str: "%ds",
        bytes: "%ds",
        None.__class__: "xxxxxx?x",
    }
    _alignment = 8
    # value kind -> function rebuilding the Python value
    _back_transforms_mapping = {
        0: lambda value: value,
        1: lambda value: value.rstrip(b'\x00').decode(_encoding),
        2: lambda value: value.rstrip(b'\x00'),
        3: lambda _value: None,
    }

    @staticmethod
    def _extract_recreation_code(value):
        if not isinstance(value, (str, bytes, None.__class__)):
            return 0
        elif isinstance(value, str):
            return 1
        elif isinstance(value, bytes):
            return 2
        else:
            return 3

    def _slot_format(self, item):
        fmt = self._types_mapping[type(item)]
        if isinstance(item, (str, bytes)):
            raw = item.encode(_encoding) if isinstance(item, str) else item
            capacity = self._alignment * (len(raw) // self._alignment + 1)
            fmt = fmt % (capacity,)
        return fmt

    def __init__(self, sequence=None, *, name=None):
        if name is None or sequence is not None:
            sequence = sequence or ()
            sequence = list(sequence)
            formats = [self._slot_format(item) for item in sequence]
            self._list_len = len(formats)
            assert sum(len(fmt) <= 8 for fmt in formats) == self._list_len
            ends = []
            offset = 0
            for fmt in formats:
                offset += self._alignment if fmt[-1] != "s" else int(fmt[:-1])
                ends.append(offset)
            self._allocated_offsets = [0] + ends
            codes = [self._extract_recreation_code(item) for item in sequence]
            requested_size = struct.calcsize(
                "q" + self._format_size_metainfo +
                "".join(formats) +
                self._format_packing_metainfo +
                self._format_back_transform_codes
            )
            self.shm = SharedMemory(name, create=True, size=requested_size)
        else:
            self.shm = SharedMemory(name)

        if sequence is not None:
            buf = self.shm.buf
            struct.pack_into("q" + self._format_size_metainfo, buf, 0,
                             self._list_len, *ends)
            struct.pack_into("".join(formats), buf, self._offset_data_start,
                             *(v.encode(_encoding) if isinstance(v, str) else v
                               for v in sequence))
            struct.pack_into(self._format_packing_metainfo, buf,
                             self._offset_packing_formats,
                             *(f.encode(_encoding) for f in formats))
            struct.pack_into(self._format_back_transform_codes, buf,
                             self._offset_back_transform_codes, *codes)
        else:
            self._list_len = len(self)
            self._allocated_offsets = [0] + list(struct.unpack_from(
                self._format_size_metainfo, self.shm.buf, 8))

    def _get_packing_format(self, position):
        """The struct format of the slot *position*."""
        position = position if position >= 0 else position + self._list_len
        if position >= self._list_len or self._list_len < 0:
            raise IndexError("Requested position out of range.")
        v = struct.unpack_from(
            "8s", self.shm.buf, self._offset_packing_formats + position * 8)[0]
        return v.rstrip(b'\x00').decode(_encoding)

    def _get_back_transform(self, position):
        if position >= self._list_len or self._list_len < 0:
            raise IndexError("Requested position out of range.")
        code = struct.unpack_from(
            "b", self.shm.buf, self._offset_back_transform_codes + position)[0]
        return self._back_transforms_mapping[code]

    def _set_packing_format_and_transform(self, position, fmt_as_str, value):
        if position >= self._list_len or self._list_len < 0:
            raise IndexError("Requested position out of range.")
        struct.pack_into("8s", self.shm.buf,
                         self._offset_packing_formats + position * 8,
                         fmt_as_str.encode(_encoding))
        struct.pack_into("b", self.shm.buf,
                         self._offset_back_transform_codes + position,
                         self._extract_recreation_code(value))

    def __getitem__(self, position):
        position = position if position >= 0 else position + self._list_len
        try:
            offset = self._offset_data_start + self._allocated_offsets[position]
            (v,) = struct.unpack_from(self._get_packing_format(position),
                                      self.shm.buf, offset)
        except IndexError:
            raise IndexError("index out of range")
        return self._get_back_transform(position)(v)

    def __setitem__(self, position, value):
        position = position if position >= 0 else position + self._list_len
        try:
            item_offset = self._allocated_offsets[position]
            offset = self._offset_data_start + item_offset
            current_format = self._get_packing_format(position)
        except IndexError:
            raise IndexError("assignment index out of range")

        if not isinstance(value, (str, bytes)):
            new_format = self._types_mapping[type(value)]
            encoded = value
        else:
            capacity = self._allocated_offsets[position + 1] - item_offset
            encoded = value.encode(_encoding) if isinstance(value, str) else value
            if len(encoded) > capacity:
                raise ValueError("bytes/str item exceeds available storage")
            if current_format[-1] == "s":
                new_format = current_format
            else:
                new_format = self._types_mapping[str] % (capacity,)

        self._set_packing_format_and_transform(position, new_format, value)
        struct.pack_into(new_format, self.shm.buf, offset, encoded)

    def __reduce__(self):
        return partial(self.__class__, name=self.shm.name), ()

    def __len__(self):
        return struct.unpack_from("q", self.shm.buf, 0)[0]

    def __repr__(self):
        return f'{self.__class__.__name__}({list(self)}, name={self.shm.name!r})'

    @property
    def format(self):
        """The struct format of all the slots."""
        return "".join(self._get_packing_format(i) for i in range(self._list_len))

    @property
    def _format_size_metainfo(self):
        return "q" * self._list_len

    @property
    def _format_packing_metainfo(self):
        return "8s" * self._list_len

    @property
    def _format_back_transform_codes(self):
        return "b" * self._list_len

    @property
    def _offset_data_start(self):
        return (self._list_len + 1) * 8

    @property
    def _offset_packing_formats(self):
        return self._offset_data_start + self._allocated_offsets[-1]

    @property
    def _offset_back_transform_codes(self):
        return self._offset_packing_formats + self._list_len * 8

    def count(self, value):
        """Number of occurrences of *value*."""
        return sum(value == entry for entry in self)

    def index(self, value):
        """Position of the first occurrence of *value*."""
        for position, entry in enumerate(self):
            if value == entry:
                return position
        raise ValueError("ShareableList.index(x): x not in list")

    __class_getitem__ = classmethod(types.GenericAlias)
