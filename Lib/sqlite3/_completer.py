"""Tab completion for the interactive sqlite3 shell (python -m sqlite3).

Offers the shell's dot-commands, SQL keywords, SQL function names and the
names of schemas, tables, indexes, triggers, views and columns found in the
connected database.  Matching ignores case; keywords and functions are
offered in upper case, database names as they were created.
"""

from contextlib import contextmanager

try:
    from _sqlite3 import SQLITE_KEYWORDS
except ImportError:
    SQLITE_KEYWORDS = ()

_COMMANDS = (".help", ".quit", ".version")


def _rows(con, sql):
    try:
        return con.execute(sql).fetchall()
    except Exception:
        return []


def _quote(name):
    return '"' + name.replace('"', '""') + '"'


def _database_words(con):
    """Names defined in the database, plus SQL function names."""
    words = set()
    schemas = [row[1] for row in _rows(con, "PRAGMA database_list")]
    words.update(schemas)
    for schema in schemas:
        q = _quote(schema)
        objects = _rows(con, f"SELECT name, type FROM {q}.sqlite_master")
        for name, kind in objects:
            words.add(name)
            if kind in ("table", "view"):
                for col in _rows(con, f"PRAGMA {q}.table_info({_quote(name)})"):
                    words.add(col[1])
    functions = {row[0].upper() + "("
                 for row in _rows(con, "PRAGMA function_list")}
    return words, functions


def _candidates(con, text):
    if text.startswith("."):
        return [c for c in _COMMANDS if c.startswith(text)]
    folded = text.casefold()
    upper = text.upper()
    found = {k + " " for k in SQLITE_KEYWORDS if k.startswith(upper)}
    words, functions = _database_words(con)
    found.update(f for f in functions if f.startswith(upper))
    found.update(w for w in words if w.casefold().startswith(folded))
    return sorted(found)


@contextmanager
def completer(con):
    """Install a readline completer for `con` for the duration of the block
    (a no-op when readline is unavailable)."""
    try:
        import readline
    except ImportError:
        yield
        return

    matches = []

    def complete(text, state):
        if state == 0:
            matches[:] = _candidates(con, text)
        return matches[state] if state < len(matches) else None

    previous = readline.get_completer()
    readline.set_completer(complete)
    if getattr(readline, "backend", "readline") == "editline":
        readline.parse_and_bind("bind ^I rl_complete")
    else:
        readline.parse_and_bind("tab: complete")
    try:
        yield
    finally:
        readline.set_completer(previous)
