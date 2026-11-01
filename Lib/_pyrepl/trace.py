"""Optional debug log for the REPL internals.

Set PYREPL_TRACE to a file name to append trace lines there."""

from __future__ import annotations

import os

_path = os.environ.get("PYREPL_TRACE")
trace_file = open(_path, "a", encoding="utf-8") if _path else None


def trace(line: str, *args, **kwargs) -> None:
    if trace_file is None:
        return
    if args or kwargs:
        line = line.format(*args, **kwargs)
    trace_file.write(line + "\n")
    trace_file.flush()
