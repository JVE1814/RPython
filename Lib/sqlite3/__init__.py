"""
The sqlite3 package is a DB-API 2.0 (PEP 249) interface to SQLite.

A minimal example:

    >>> import sqlite3
    >>> cx = sqlite3.connect(":memory:")
    >>> cx.execute("CREATE TABLE t(a, b)")
    <sqlite3.Cursor object at ...>
    >>> cx.execute("INSERT INTO t VALUES(?, ?)", (1, "spam"))
    <sqlite3.Cursor object at ...>
    >>> cx.execute("SELECT * FROM t").fetchall()
    [(1, 'spam')]
    >>> cx.close()

See the library reference for transactions, adapters/converters, user-defined
functions and the other features.
"""

from sqlite3.dbapi2 import *
