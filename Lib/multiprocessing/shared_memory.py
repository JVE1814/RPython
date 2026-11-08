"""Named shared memory blocks that several processes can map, and
ShareableList, a fixed-length list of simple values kept in one."""

__all__ = ['SharedMemory', 'ShareableList']

from functools import partial
import mmap
import os
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

# Generated names stay within the 14 characters some systems allow.
_SHM_SAFE_NAME_LENGTH = 14

_SHM_NAME_PREFIX = '/psm_' if _USE_POSIX else 'wnsm_'


def _make_filename():
    """Return a random name for a new block."""
    random_chars = (_SHM_SAFE_NAME_LENGTH - len(_SHM_NAME_PREFIX)) // 2
    assert random_chars >= 2, '_SHM_NAME_PREFIX too long'
    name = _SHM_NAME_PREFIX + os.urandom(random_chars).hex()
    assert len(name) <= _SHM_SAFE_NAME_LENGTH
    return name


class SharedMemory:
    """A block of shared memory known by name.

    SharedMemory(create=True, size=n) makes a new block with a generated
    name; SharedMemory(name) maps an existing block.  Every process calls
    close() when done with it, and one of them calls unlink() to remove it.
    With track=True the resource tracker removes the block if all its users
    die without unlinking it."""

    # Class-level defaults; subclasses may change _flags (for instance to
    # "create if missing") or _mode.
    _name = None
    _fd = -1
    _mmap = None
    _buf = None
    _flags = os.O_RDWR
    _mode = 0o600
    _prepend_leading_slash = _USE_POSIX
    _track = True

    def __init__(self, name=None, create=False, size=0, *, track=True):
        if not size >= 0:
            raise ValueError("'size' must be a positive integer")
        if create:
            self._flags = _O_CREX | os.O_RDWR
            if size == 0:
                raise ValueError(
                    "'size' must be a positive number different from zero")
        if name is None and not self._flags & os.O_EXCL:
            raise ValueError("'name' can only be None if create=True")
        self._track = track
        if not _USE_POSIX:
            raise NotImplementedError("shared memory needs POSIX shm")
        if name is None:
            self._open_new_random()
        else:
            if self._prepend_leading_slash:
                name = '/' + name
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
            try:
                resource_tracker.register(self._name, "shared_memory")
            except BaseException:
                # Do not leave a block behind that nobody tracks.
                self.close()
                if create:
                    _posixshmem.shm_unlink(self._name)
                raise
        self._size = size
        self._buf = memoryview(self._mmap)

    def _open_new_random(self):
        while True:
            candidate = _make_filename()
            try:
                self._fd = _posixshmem.shm_open(candidate, self._flags,
                                                mode=self._mode)
            except FileExistsError:
                continue
            self._name = candidate
            return

    def __del__(self):
        try:
            self.close()
        except OSError:
            pass

    def __reduce__(self):
        return (type(self), (self.name, False, self.size))

    def __repr__(self):
        return f'{type(self).__name__}({self.name!r}, size={self.size})'

    @property
    def buf(self):
        """A memoryview of the whole block."""
        return self._buf

    @property
    def name(self):
        """The name other processes use to attach to the block."""
        if self._prepend_leading_slash and self._name.startswith('/'):
            return self._name[1:]
        return self._name

    @property
    def size(self):
        """Size of the block in bytes."""
        return self._size

    def close(self):
        """Unmap the block from this process; it stays available to others."""
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
        """Remove the block's name.  Call it once, from one process; the
        memory goes away when the last process has closed it."""
        if _USE_POSIX and self._name:
            _posixshmem.shm_unlink(self._name)
            if self._track:
                resource_tracker.unregister(self._name, "shared_memory")


_encoding = "utf8"

# How a slot's bytes turn back into a Python value.
_PLAIN, _TEXT, _BINARY, _NONE = range(4)

_DECODERS = {
    _PLAIN: lambda raw: raw,
    _TEXT: lambda raw: raw.rstrip(b'\x00').decode(_encoding),
    _BINARY: lambda raw: raw.rstrip(b'\x00'),
    _NONE: lambda raw: None,
}

# Slot descriptor: capacity in bytes, struct format of the current value
# (NUL-padded), value kind, padding.
_SLOT = struct.Struct('q8sb7x')
_COUNT = struct.Struct('q')


def _kind_of(value):
    if isinstance(value, str):
        return _TEXT
    if isinstance(value, bytes):
        return _BINARY
    if value is None:
        return _NONE
    return _PLAIN


class ShareableList:
    """A list of int, float, bool, str, bytes and None values with a fixed
    number of slots, stored in a SharedMemory block that other processes
    can attach to by name.

    Numbers, booleans and None take 8 bytes; a str or bytes slot gets the
    room its initial value needed (rounded up to a multiple of 8, with at
    least one spare byte).  A slot may later hold a value of another type
    if it fits.

    Block layout: the slot count, one descriptor per slot (capacity, struct
    format, kind), then the slots' data back to back."""

    _types_mapping = {
        int: "q",
        float: "d",
        bool: "xxxxxxx?",
        str: "%ds",
        bytes: "%ds",
        None.__class__: "xxxxxx?x",
    }
    _alignment = 8

    def __init__(self, sequence=None, *, name=None):
        if name is None or sequence is not None:
            values = list(sequence or ())
            self._create(values, name)
        else:
            self.shm = SharedMemory(name)
            self._load_layout()

    # -- layout --------------------------------------------------------------

    def _capacity_for(self, value):
        if isinstance(value, (str, bytes)):
            # One spare byte beyond the length in characters (or bytes), but
            # never less than the encoded value needs.
            encoded = value.encode(_encoding) if isinstance(value, str) else value
            spare = self._alignment * (len(value) // self._alignment + 1)
            return max(spare, -(-len(encoded) // self._alignment) * self._alignment)
        return self._alignment

    def _format_for(self, value, capacity):
        fmt = self._types_mapping[type(value)]
        return fmt % capacity if fmt.endswith('s') else fmt

    def _create(self, values, name):
        capacities = [self._capacity_for(v) for v in values]
        size = _COUNT.size + _SLOT.size * len(values) + sum(capacities)
        self.shm = SharedMemory(name, create=True, size=size)
        buf = self.shm.buf
        _COUNT.pack_into(buf, 0, len(values))
        self._list_len = len(values)
        self._compute_offsets(capacities)
        for position, (value, capacity) in enumerate(zip(values, capacities)):
            self._store(position, value, self._format_for(value, capacity))

    def _load_layout(self):
        self._list_len = _COUNT.unpack_from(self.shm.buf, 0)[0]
        capacities = [self._descriptor(i)[0] for i in range(self._list_len)]
        self._compute_offsets(capacities)

    def _compute_offsets(self, capacities):
        # Byte offset of each slot's data within the block, plus the end.
        self._capacities = capacities
        offset = _COUNT.size + _SLOT.size * len(capacities)
        starts = []
        for capacity in capacities:
            starts.append(offset)
            offset += capacity
        self._data_starts = starts

    def _descriptor(self, position):
        capacity, fmt, kind = _SLOT.unpack_from(
            self.shm.buf, _COUNT.size + position * _SLOT.size)
        return capacity, fmt.rstrip(b'\x00').decode(_encoding), kind

    def _store(self, position, value, fmt):
        buf = self.shm.buf
        _SLOT.pack_into(buf, _COUNT.size + position * _SLOT.size,
                        self._capacities[position], fmt.encode(_encoding),
                        _kind_of(value))
        if isinstance(value, str):
            value = value.encode(_encoding)
        struct.pack_into(fmt, buf, self._data_starts[position], value)

    def _normalize(self, position, message):
        if position < 0:
            position += self._list_len
        if not 0 <= position < self._list_len:
            raise IndexError(message)
        return position

    def _get_packing_format(self, position):
        """The struct format the slot at *position* currently uses."""
        position = self._normalize(position, "Requested position out of range.")
        return self._descriptor(position)[1]

    # -- the sequence protocol -----------------------------------------------

    def __getitem__(self, position):
        position = self._normalize(position, "index out of range")
        _capacity, fmt, kind = self._descriptor(position)
        (raw,) = struct.unpack_from(fmt, self.shm.buf,
                                    self._data_starts[position])
        return _DECODERS[kind](raw)

    def __setitem__(self, position, value):
        position = self._normalize(position, "assignment index out of range")
        capacity = self._capacities[position]
        if isinstance(value, (str, bytes)):
            encoded = value.encode(_encoding) if isinstance(value, str) else value
            if len(encoded) > capacity:
                raise ValueError("bytes/str item exceeds available storage")
            current = self._descriptor(position)[1]
            fmt = current if current.endswith('s') else '%ds' % capacity
        else:
            fmt = self._types_mapping[type(value)]
        self._store(position, value, fmt)

    def __len__(self):
        return _COUNT.unpack_from(self.shm.buf, 0)[0]

    def __reduce__(self):
        return partial(type(self), name=self.shm.name), ()

    def __repr__(self):
        return f'{type(self).__name__}({list(self)}, name={self.shm.name!r})'

    @property
    def format(self):
        """The struct formats of all slots, concatenated."""
        return ''.join(self._descriptor(i)[1] for i in range(self._list_len))

    def count(self, value):
        """Return how many slots equal *value*."""
        return sum(1 for item in self if item == value)

    def index(self, value):
        """Return the position of the first slot equal to *value*."""
        for position, item in enumerate(self):
            if item == value:
                return position
        raise ValueError("ShareableList.index(x): x not in list")

    __class_getitem__ = classmethod(types.GenericAlias)
