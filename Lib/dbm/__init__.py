"""Common entry point for the dbm-style key/value databases.

``dbm.open(path, flag)`` opens an existing database with whichever submodule
created it (``whichdb`` inspects the files), or creates a new one with the
first available implementation among dbm.sqlite3, dbm.gnu, dbm.ndbm and
dbm.dumb.  All of them behave like a mapping from bytes to bytes (str keys
and values are stored UTF-8 encoded).

flag: 'r' read-only (default), 'w' read-write, 'c' read-write and create if
missing, 'n' always start with a new empty database.
"""

import os
from builtins import open as _builtin_open

__all__ = ["open", "whichdb", "error"]

# Implementations in order of preference for new databases.
_names = ["dbm.sqlite3", "dbm.gnu", "dbm.ndbm", "dbm.dumb"]
_defaultmod = None
_modules = {}

# dbm.error is a tuple: our own exception class plus OSError, which the
# individual implementations raise (their ``error`` classes derive from it).
error = (type("error", (Exception,), {"__module__": __name__}), OSError)

try:
    from dbm import ndbm
except ImportError:
    ndbm = None


def _load_implementations():
    global _defaultmod
    for name in _names:
        try:
            mod = __import__(name, fromlist=["open"])
        except ImportError:
            continue
        _modules[name] = mod
        if _defaultmod is None:
            _defaultmod = mod
    if _defaultmod is None:
        raise ImportError(f"no dbm clone found; tried {_names}")


def open(file, flag="r", mode=0o666):
    """Open the database *file* and return the database object.

    *flag* selects the access mode ('r', 'w', 'c' or 'n', see the module
    documentation); *mode* is the Unix permission used for new files.
    """
    if _defaultmod is None:
        _load_implementations()
    kind = None if "n" in flag else whichdb(file)
    if kind is None:
        if "c" not in flag and "n" not in flag:
            raise error[0]("db file doesn't exist; "
                           "use 'c' or 'n' flag to create a new db")
        impl = _defaultmod
    elif kind == "":
        raise error[0]("db type could not be determined")
    else:
        impl = _modules.get(kind)
        if impl is None:
            try:
                impl = __import__(kind, fromlist=["open"])
            except ImportError:
                raise error[0](f"db type is {kind}, but the module is not available")
            _modules[kind] = impl
    return impl.open(file, flag, mode)


def _exists(path):
    try:
        with _builtin_open(path, "rb"):
            return True
    except OSError:
        return False


_GNU_MAGICS = {0x13579ACD, 0x13579ACE, 0x13579ACF, 0x13579AD0, 0x13579AD1}


def whichdb(filename):
    """Guess which dbm submodule created the database *filename*.

    Returns the submodule name (e.g. "dbm.gnu"), "" when the file exists
    but its format is not recognised, or None when no database file could
    be read.
    """
    base = os.fsencode(filename)

    # ndbm (here: gdbm's ndbm emulation) keeps NAME.pag and NAME.dir.
    if _exists(base + b".pag") and _exists(base + b".dir"):
        return "dbm.ndbm"
    # Berkeley DB based ndbm libraries use a single NAME.db file.
    if _exists(base + b".db"):
        if ndbm is not None:
            try:
                ndbm.open(base).close()
            except OSError:
                pass
            else:
                return "dbm.ndbm"

    # dbm.dumb: NAME.dat plus a text index NAME.dir whose lines start with a
    # quoted key (or which is empty for an empty database).
    if _exists(base + b".dat") and _exists(base + b".dir"):
        try:
            with _builtin_open(base + b".dir", "rb") as f:
                first = f.read(1)
        except OSError:
            first = None
        if first is not None and first in (b"", b"'", b'"'):
            return "dbm.dumb"

    try:
        with _builtin_open(base, "rb") as f:
            head = f.read(16)
    except OSError:
        return None
    if len(head) < 4:
        return ""
    if head == b"SQLite format 3\0":
        return "dbm.sqlite3"
    for order in ("little", "big"):
        if int.from_bytes(head[:4], order) in _GNU_MAGICS:
            return "dbm.gnu"
    return ""


if __name__ == "__main__":
    import sys
    for name in sys.argv[1:]:
        print(whichdb(name) or "UNKNOWN", name)
