"""The REPL loop: read a (possibly multi-line) statement with the pyrepl
reader, run it in an InteractiveColoredConsole, repeat."""

from __future__ import annotations

import _sitebuiltins
import functools
import os
import sys

from .readline import _get_reader, multiline_input, _wrapper

_error: tuple[type[Exception], ...] | type[Exception]
try:
    from .unix_console import _error
except ModuleNotFoundError:  # pragma: no cover
    from .windows_console import _error


def check() -> str:
    """Empty if the pyrepl reader can be used here, else the reason why not."""
    try:
        _get_reader()
    except _error as exc:
        if term := os.environ.get("TERM", ""):
            term = f"; TERM={term}"
        return str(str(exc) or repr(exc) or "unknown error") + term
    return ""


def _strip_final_indent(text: str) -> str:
    """Drop a final line made only of indentation (keeping its newline)."""
    short = text.rstrip(" \t")
    if short != text and short.endswith("\n"):
        return short
    return text


def _clear_screen() -> None:
    reader = _get_reader()
    reader.scheduled_commands.append("clear-screen")


def _show_help() -> None:
    import builtins
    helper = getattr(builtins, "help", None)
    if helper is None:
        helper = _sitebuiltins._Helper()
    helper()


REPL_COMMANDS = {
    "exit": _sitebuiltins.Quitter("exit", ""),
    "quit": _sitebuiltins.Quitter("quit", ""),
    "copyright": _sitebuiltins._Printer("copyright", sys.copyright),
    "help": _show_help,
    "clear": _clear_screen,
}


def _more_lines(console, unicodetext: str) -> bool:
    """Whether the REPL should wait for more lines before running
    *unicodetext*."""
    src = _strip_final_indent(unicodetext)
    try:
        code = console.compile(src, "<stdin>", "single")
    except (OverflowError, SyntaxError, ValueError, MemoryError, RecursionError):
        lines = src.splitlines(keepends=True)
        if len(lines) <= 1:
            return False
        last = lines[-1]
        if last.endswith("\n"):
            return False
        return bool(last.strip()) or last[:1] in (" ", "\t")
    return code is None


def _prompt(name: str, default: str) -> str:
    value = getattr(sys, name, default)
    try:
        return str(value)
    except Exception:
        return default


def _save_history_entry(seen: list[int]) -> None:
    """Append the entries added to the history since the last call to
    the history file (so history survives a crash)."""
    path = getattr(_wrapper, "history_file", None)
    reader = getattr(_wrapper, "reader", None)
    if reader is None:
        return
    length = len(reader.history)
    new = length - seen[0]
    seen[0] = length
    if not path or new <= 0:
        return
    try:
        _wrapper.append_history_file(new, path)
    except Exception:
        pass


def run_multiline_interactive_console(console, *, future_flags: int = 0) -> None:
    from .readline import _setup
    _setup(console.locals)
    if future_flags:
        console.compile.compiler.flags |= future_flags

    more_lines = functools.partial(_more_lines, console)
    input_n = 0
    seen = [0]
    try:
        reader = _wrapper.reader
        if reader is None:
            raise LookupError
        seen = [len(reader.history)]
        if reader.threading_hook is None:
            from ._threading_handler import install_threading_hook
            install_threading_hook(reader)
    except Exception:
        seen = [0]
    show_refcount = (sys._xoptions.get("showrefcount") is not None
                     and hasattr(sys, "gettotalrefcount"))

    def run_repl_command(statement: str) -> bool:
        word = statement.strip()
        if word not in REPL_COMMANDS or word in console.locals:
            return False
        command = REPL_COMMANDS[word]
        if isinstance(command, _sitebuiltins.Quitter):
            _save_history_entry(seen)
            raise SystemExit
        command()
        return True

    while True:
        try:
            try:
                sys.stdout.flush()
            except Exception:
                pass
            ps1 = _prompt("ps1", ">>> ")
            ps2 = _prompt("ps2", "... ")
            try:
                statement = multiline_input(more_lines, ps1, ps2)
            except EOFError:
                break
            if run_repl_command(statement):
                _save_history_entry(seen)
                continue
            filename = f"<python-input-{input_n}>"
            input_n += 1
            try:
                more = console.push(_strip_final_indent(statement),
                                    filename=filename, _symbol="single")
            finally:
                _save_history_entry(seen)
            assert not more
            if show_refcount:
                print(f"[{sys.gettotalrefcount()} refs,"
                      f" {sys.getallocatedblocks()} blocks]", file=sys.stderr)
        except KeyboardInterrupt:
            reader = _get_reader()
            if reader.input_trans is reader.isearch_trans:
                reader.do_cmd(("isearch-end", [""]))
            reader.pos = len(reader.get_unicode())
            reader.dirty = True
            reader.refresh()
            console.write("\nKeyboardInterrupt\n")
            console.resetbuffer()
        except MemoryError:
            console.write("\nMemoryError\n")
            console.resetbuffer()
