"""A slow but portable dbm implementation written in pure Python.

A database named NAME lives in two files:

* ``NAME.dat`` holds the values, each one starting on a 512-byte boundary;
* ``NAME.dir`` is the index: one line per key, ``<key literal>, (<offset>, <size>)``,
  where the key is the latin-1 decoding of the key bytes.

When the index is rewritten the previous version is kept as ``NAME.bak``.
Nothing is ever ``eval``'d: index lines are parsed with ``ast.literal_eval``.
"""

import ast as _ast
import collections.abc
import io as _io
import os as _os

__all__ = ["error", "open"]

error = OSError

_BLOCK = 512


def _as_key(key):
    if isinstance(key, str):
        return key.encode("utf-8")
    if isinstance(key, bytes):
        return key
    raise TypeError("keys must be bytes or strings")


def _as_value(val):
    if isinstance(val, str):
        return val.encode("utf-8")
    if isinstance(val, (bytes, bytearray)):
        return bytes(val)
    raise TypeError("values must be bytes or strings")


class _Database(collections.abc.MutableMapping):

    def __init__(self, base, mode, flag):
        self._index = None
        if flag not in ("r", "w", "c", "n"):
            raise ValueError("Flag must be one of 'r', 'w', 'c', or 'n'")
        base = _os.fspath(base)
        self._mode = mode
        self._readonly = (flag == "r")
        self._dirty = False
        self._dirfile = base + (b".dir" if isinstance(base, bytes) else ".dir")
        self._datfile = base + (b".dat" if isinstance(base, bytes) else ".dat")
        self._bakfile = base + (b".bak" if isinstance(base, bytes) else ".bak")
        self._prepare_files(flag)
        self._index = self._read_index()

    # ---------------------------------------------------------------- files

    def _create(self, path, truncate):
        flags = _os.O_WRONLY | _os.O_CREAT
        if truncate:
            flags |= _os.O_TRUNC
        fd = _os.open(path, flags, self._mode)
        _os.close(fd)

    def _prepare_files(self, flag):
        if flag == "n":
            for path in (self._datfile, self._dirfile):
                self._create(path, truncate=True)
            return
        if flag in ("r", "w"):
            # Both files must already exist; never create anything.
            for path in (self._datfile, self._dirfile):
                if not _os.path.exists(path):
                    raise FileNotFoundError(2, "No such file or directory", path)
            return
        # 'c': create whichever file is missing (an empty index is valid).
        if not _os.path.exists(self._datfile):
            self._create(self._datfile, truncate=False)
        if not _os.path.exists(self._dirfile):
            self._create(self._dirfile, truncate=False)

    def _read_index(self):
        index = {}
        try:
            stream = _io.open(self._dirfile, "r", encoding="latin-1")
        except FileNotFoundError:
            return index
        with stream:
            for line in stream:
                line = line.strip()
                if not line:
                    continue
                key, location = _ast.literal_eval(line)
                if (not isinstance(key, str) or not isinstance(location, tuple)
                        or len(location) != 2):
                    raise ValueError("malformed dbm.dumb index entry")
                index[key.encode("latin-1")] = location
        return index

    def _write_index(self):
        # Keep the previous index as the backup copy.
        try:
            _os.unlink(self._bakfile)
        except FileNotFoundError:
            pass
        try:
            _os.rename(self._dirfile, self._bakfile)
        except FileNotFoundError:
            pass
        fd = _os.open(self._dirfile, _os.O_WRONLY | _os.O_CREAT | _os.O_TRUNC,
                      self._mode)
        with _io.open(fd, "w", encoding="latin-1") as stream:
            for key, location in self._index.items():
                stream.write("%r, %r\n" % (key.decode("latin-1"), location))

    def _append_value(self, data):
        """Write *data* at the end of the data file (block aligned)."""
        with _io.open(self._datfile, "rb+") as f:
            end = f.seek(0, 2)
            start = -(-end // _BLOCK) * _BLOCK
            if start > end:
                f.write(b"\0" * (start - end))
            f.write(data)
        return start

    def _overwrite_value(self, start, data):
        with _io.open(self._datfile, "rb+") as f:
            f.seek(start)
            f.write(data)

    # ---------------------------------------------------------------- checks

    def _require_open(self):
        if self._index is None:
            raise error("DBM object has already been closed")

    def _require_writable(self):
        self._require_open()
        if self._readonly:
            raise error("The database is opened for reading only")

    # ---------------------------------------------------------------- mapping

    def __getitem__(self, key):
        self._require_open()
        key = _as_key(key)
        start, size = self._index[key]
        with _io.open(self._datfile, "rb") as f:
            f.seek(start)
            return f.read(size)

    def __setitem__(self, key, value):
        self._require_writable()
        key = _as_key(key)
        data = _as_value(value)
        location = self._index.get(key)
        if location is not None:
            start, old = location
            if -(-old // _BLOCK) >= -(-len(data) // _BLOCK) and old:
                # The new value fits in the blocks already reserved.
                self._overwrite_value(start, data)
                self._index[key] = (start, len(data))
                self._dirty = True
                return
        start = self._append_value(data)
        self._index[key] = (start, len(data))
        self._dirty = True

    def __delitem__(self, key):
        self._require_writable()
        key = _as_key(key)
        del self._index[key]
        self._dirty = True
        self._write_index()
        self._dirty = False

    def __contains__(self, key):
        self._require_open()
        try:
            key = _as_key(key)
        except TypeError:
            return False
        return key in self._index

    def __iter__(self):
        self._require_open()
        return iter(list(self._index))

    def __len__(self):
        self._require_open()
        return len(self._index)

    def keys(self):
        self._require_open()
        return list(self._index)

    def iterkeys(self):
        self._require_open()
        return iter(list(self._index))

    def items(self):
        self._require_open()
        return [(key, self[key]) for key in list(self._index)]

    # ---------------------------------------------------------------- maintenance

    def sync(self):
        if self._index is None or not self._dirty:
            return
        self._write_index()
        self._dirty = False

    def reorganize(self):
        self._require_writable()
        values = [(key, self[key]) for key in self._index]
        with _io.open(self._datfile, "wb") as f:
            pos = 0
            for key, data in values:
                start = -(-pos // _BLOCK) * _BLOCK
                if start > pos:
                    f.write(b"\0" * (start - pos))
                f.write(data)
                pos = start + len(data)
                self._index[key] = (start, len(data))
        self._dirty = True
        self.sync()

    def close(self):
        try:
            self.sync()
        finally:
            self._index = None

    __del__ = close

    def __enter__(self):
        return self

    def __exit__(self, *exc_info):
        self.close()


def open(file, flag="c", mode=0o666):
    """Open (or create) the dumb database *file* and return it.

    *flag* is 'r' (read-only), 'w' (read-write), 'c' (read-write, created if
    needed; the default) or 'n' (always a new, empty database).  *mode* is the
    permission used for created files (subject to the umask).
    """
    return _Database(file, mode, flag)
