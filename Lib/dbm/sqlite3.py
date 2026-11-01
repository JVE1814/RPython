"""A dbm-style key/value store kept in an SQLite database file.

Keys and values are stored as BLOBs in a single table; anything SQLite can
cast to a BLOB (str, bytes-like objects, numbers) is accepted and read back
as bytes.
"""

import os
import sqlite3
from collections.abc import MutableMapping
from contextlib import closing, suppress

__all__ = ["error", "open"]


class error(OSError):
    pass


_TABLE = "Dict"
_SCHEMA = f"CREATE TABLE IF NOT EXISTS {_TABLE} (key BLOB UNIQUE NOT NULL, value BLOB NOT NULL)"
_Q_GET = f"SELECT value FROM {_TABLE} WHERE key = CAST(? AS BLOB)"
_Q_PUT = f"REPLACE INTO {_TABLE} (key, value) VALUES (CAST(? AS BLOB), CAST(? AS BLOB))"
_Q_DEL = f"DELETE FROM {_TABLE} WHERE key = CAST(? AS BLOB)"
_Q_KEYS = f"SELECT key FROM {_TABLE}"
_Q_COUNT = f"SELECT COUNT(key) FROM {_TABLE}"
_Q_DROP = f"DROP TABLE IF EXISTS {_TABLE}"

_URI_MODES = {"r": "ro", "w": "rw", "c": "rwc", "n": "rwc"}

# Characters that may appear unescaped in the path part of a file: URI.
_URI_SAFE = frozenset(b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"
                      b"0123456789-._~/")


def _normalize_uri(path):
    """Return a percent-encoded ``file:`` URI naming *path* (made absolute)."""
    full = os.path.abspath(os.fspath(path))
    if isinstance(full, str):
        full = os.fsencode(full)
    while b"//" in full:
        full = full.replace(b"//", b"/")
    parts = []
    for byte in full:
        if byte in _URI_SAFE:
            parts.append(chr(byte))
        else:
            parts.append("%%%02X" % byte)
    return "file:" + "".join(parts)


class _Database(MutableMapping):

    def __init__(self, path, /, *, flag, mode):
        if hasattr(self, "_cx"):
            raise error("database is already open")
        if flag not in _URI_MODES:
            raise ValueError(
                f"Flag must be one of 'r', 'w', 'c', or 'n', not {flag!r}")
        self._cx = None
        if flag == "n":
            # Truncate (or create) the file first so stale content disappears.
            with suppress(OSError):
                os.remove(path)
        uri = f"{_normalize_uri(path)}?mode={_URI_MODES[flag]}"
        try:
            cx = sqlite3.connect(uri, autocommit=True, uri=True)
        except sqlite3.Error as exc:
            raise error(str(exc))
        self._cx = cx
        if flag == "n":
            self._run(_Q_DROP)
        if flag != "r":
            try:
                self._run(_SCHEMA)
            except error:
                if flag != "w":
                    raise

    # -- helpers

    def _conn(self):
        if self._cx is None:
            raise error("DBM object has already been closed")
        return self._cx

    def _run(self, sql, *args):
        cx = self._conn()
        try:
            return closing(cx.execute(sql, args))
        except sqlite3.Error as exc:
            raise error(str(exc))

    # -- mapping protocol

    def __len__(self):
        with self._run(_Q_COUNT) as cu:
            try:
                row = cu.fetchone()
            except sqlite3.Error as exc:
                raise error(str(exc))
        return row[0]

    def __getitem__(self, key):
        with self._run(_Q_GET, key) as cu:
            try:
                row = cu.fetchone()
            except sqlite3.Error as exc:
                raise error(str(exc))
        if row is None:
            raise KeyError(key)
        return row[0]

    def __setitem__(self, key, value):
        with self._run(_Q_PUT, key, value):
            pass

    def __delitem__(self, key):
        with self._run(_Q_DEL, key) as cu:
            if not cu.rowcount:
                raise KeyError(key)

    def __iter__(self):
        try:
            cu = self._run(_Q_KEYS)
        except error:
            raise
        with cu as cu:
            while True:
                try:
                    row = cu.fetchone()
                except sqlite3.Error as exc:
                    raise error(str(exc))
                if row is None:
                    return
                yield row[0]

    def keys(self):
        return list(self)

    def close(self):
        if self._cx is not None:
            self._cx.close()
            self._cx = None

    def __enter__(self):
        return self

    def __exit__(self, *exc_info):
        self.close()


def open(filename, /, flag="r", mode=0o666):
    """Open the SQLite-backed dbm database *filename*.

    *flag* is 'r' (read-only, default), 'w' (read-write), 'c' (read-write,
    created if needed) or 'n' (always a new empty database).  *mode* is
    accepted for compatibility with the other dbm modules and ignored.
    """
    return _Database(filename, flag=flag, mode=mode)
