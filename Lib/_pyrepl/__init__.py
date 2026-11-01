"""The interactive interpreter's line editor and REPL (pure Python).

The pieces: ``reader`` (editing a buffer through key commands, layout and
rendering), ``historical_reader`` and ``completing_reader`` (history and
Tab completion), ``readline`` (a readline-compatible front end), consoles
for terminals (``unix_console``), and ``simple_interact``/``main`` which
run the REPL itself.
"""
