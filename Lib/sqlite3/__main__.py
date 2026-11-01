"""A small interactive shell (and one-shot executor) for SQLite databases.

Run "python -m sqlite3 [filename] [sql]".  Without sql an interactive session
is started; statements are executed with Cursor.execute() and the resulting
rows are printed.
"""

import sqlite3
import sys

from argparse import ArgumentParser
from code import InteractiveConsole
from _colorize import get_theme, theme_no_color


def _report(exc, theme):
    t = theme.traceback
    label = type(exc).__name__
    name = getattr(exc, "sqlite_errorname", None)
    if name is not None:
        label = f"{label} ({name})"
    print(f"{t.type}{label}{t.reset}: {t.message}{exc}{t.reset}", file=sys.stderr)


def execute(cursor, sql, *, suppress_errors=True, theme=theme_no_color):
    """Run one statement on `cursor` and print the rows it returns.

    Errors are printed to stderr; unless suppress_errors is true the process
    then exits with status 1.
    """
    try:
        for row in cursor.execute(sql):
            print(row)
    except sqlite3.Error as exc:
        _report(exc, theme)
        if not suppress_errors:
            sys.exit(1)


class SqliteInteractiveConsole(InteractiveConsole):
    """Read SQL statements until they are complete, then run them."""

    def __init__(self, connection, use_color=False):
        super().__init__()
        self._cx = connection
        self._cur = connection.cursor()
        self._use_color = use_color

    def runsource(self, source, filename="<input>", symbol="single"):
        theme = get_theme(force_no_color=not self._use_color)
        text = source.strip()
        if not text:
            return False
        if text.startswith("."):
            command = text[1:].strip()
            if not command:
                return False
            if command == "version":
                print(sqlite3.sqlite_version)
            elif command == "help":
                print("Enter SQL code or one of the below commands, and press enter.\n\n"
                      ".version    Print underlying SQLite library version\n"
                      ".help       Print this help message\n"
                      ".quit       Exit the CLI, equivalent to CTRL-D\n")
            elif command == "quit":
                sys.exit(0)
            else:
                t = theme.traceback
                print(f"{t.type}Error{t.reset}: {t.message}unknown command: "
                      f'"{command}"{t.reset}', file=sys.stderr)
            return False
        if not sqlite3.complete_statement(source):
            return True
        execute(self._cur, source, theme=theme)
        return False


def main(*args):
    parser = ArgumentParser(
        description="Python sqlite3 CLI",
        prog="python -m sqlite3",
    )
    parser.add_argument(
        "filename", type=str, default=":memory:", nargs="?",
        help=("SQLite database to open (defaults to ':memory:'). "
              "A new database is created if the file does not previously exist."),
    )
    parser.add_argument(
        "sql", type=str, nargs="?",
        help=("An SQL query to execute. Any returned rows are printed to stdout."),
    )
    parser.add_argument(
        "-v", "--version", action="version",
        version=f"SQLite version {sqlite3.sqlite_version}",
        help="Print underlying SQLite library version",
    )
    args = parser.parse_args(*args)

    if args.filename == ":memory:":
        db_name = "a transient in-memory database"
    else:
        db_name = repr(args.filename)

    eofkey = "CTRL-Z" if sys.platform == "win32" else "CTRL-D"
    banner = (
        f"sqlite3 shell, running on SQLite version {sqlite3.sqlite_version}\n"
        f"Connected to {db_name}\n\n"
        "Each command will be run using execute() on the cursor.\n"
        f'Type ".help" for more information; type ".quit" or {eofkey} to quit.'
    )

    theme = get_theme()
    sys.ps1 = f"\001{theme.syntax.prompt}\002sqlite> \001{theme.syntax.reset}\002"
    sys.ps2 = f"\001{theme.syntax.prompt}\002    ... \001{theme.syntax.reset}\002"

    con = sqlite3.connect(args.filename, isolation_level=None)
    try:
        if args.sql:
            execute(con, args.sql, suppress_errors=False, theme=theme)
        else:
            try:
                import readline  # noqa: F401  (line editing when available)
            except ImportError:
                pass
            use_color = bool(theme.syntax.prompt)
            console = SqliteInteractiveConsole(con, use_color=use_color)
            console.interact(banner, exitmsg="")
    finally:
        con.close()

    sys.exit(0)


if __name__ == "__main__":
    main(sys.argv[1:])
