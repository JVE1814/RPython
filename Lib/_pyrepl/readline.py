"""A ``readline``-compatible interface on top of the pyrepl reader.

The REPL (and anything calling ``input()`` once ``_setup`` ran) edits its
lines with ``ReadlineAlikeReader``: multi-line input with auto-indent,
history, completion through a readline-style completer function, and
import completion.  The module-level functions mirror the ``readline``
module's API so that code written for it (site, rlcompleter, pdb) works.
"""

from __future__ import annotations

import os
import sys
import warnings
from dataclasses import dataclass, field

from . import commands, historical_reader
from .completing_reader import CompletingReader, stripcolor
from .console import Console as ConsoleType
from ._module_completer import ImportParser, ModuleCompleter
from .fancycompleter import Completer as FancyCompleter, colorize_matches

try:
    from rlcompleter import Completer as RLCompleter
except ImportError:  # pragma: no cover
    RLCompleter = None

Console: type[ConsoleType]
_error: tuple[type[Exception], ...] | type[Exception]

if os.name == "nt":
    from .windows_console import WindowsConsole as Console, _error
else:
    from .unix_console import UnixConsole as Console, _error

ENCODING = sys.getdefaultencoding() or "latin1"

__all__ = [
    "add_history",
    "clear_history",
    "get_begidx",
    "get_completer",
    "get_completer_delims",
    "get_current_history_length",
    "get_endidx",
    "get_history_item",
    "get_history_length",
    "get_line_buffer",
    "insert_text",
    "parse_and_bind",
    "read_history_file",
    "remove_history_item",
    "replace_history_item",
    "set_auto_history",
    "set_completer",
    "set_completer_delims",
    "set_history_length",
    "set_startup_hook",
    "write_history_file",
    "append_history_file",
    "multiline_input",
]

DEFAULT_DELIMS = " \t\n`~!@#$%^&*()-=+[{]}\\|;:'\",<>/?"


@dataclass
class ReadlineConfig:
    readline_completer: object | None = None
    completer_delims: frozenset = frozenset(DEFAULT_DELIMS)
    module_completer: ModuleCompleter | None = field(default_factory=ModuleCompleter)
    colorize_completions: object | None = None


def _get_this_line_indent(buffer: list[str], pos: int) -> int:
    start = pos
    while start > 0 and buffer[start - 1] != "\n":
        start -= 1
    n = 0
    while start + n < len(buffer) and buffer[start + n] == " ":
        n += 1
    return n


def _line_bounds(buffer: list[str], pos: int) -> tuple[int, int]:
    start = pos
    while start > 0 and buffer[start - 1] != "\n":
        start -= 1
    end = pos
    while end < len(buffer) and buffer[end] != "\n":
        end += 1
    return start, end


def _code_part(line: str) -> str:
    """*line* without a trailing comment (string contents are skipped)."""
    quote = None
    i = 0
    while i < len(line):
        c = line[i]
        if quote:
            if c == "\\":
                i += 2
                continue
            if line.startswith(quote, i):
                i += len(quote)
                quote = None
                continue
        elif c == "#":
            return line[:i]
        elif c in "'\"":
            quote = line[i:i + 3] if line[i:i + 3] in ("'''", '"""') else c
            i += len(quote)
            continue
        i += 1
    return line


def _opens_block(line: str) -> bool:
    return _code_part(line).rstrip().endswith(":")


class maybe_accept(commands.Command):
    """Enter: accept the input if it is complete, otherwise start a new
    (auto-indented) line."""

    def do(self) -> None:
        r = self.reader
        r.dirty = True
        if r.in_bracketed_paste:
            r.insert("\n")
            return
        rest = r.buffer[r.pos:]
        text = r.get_unicode()
        if "\n" in rest:
            if "".join(rest).strip():
                self._newline()
                return
            text = text[:r.pos]
        if r.more_lines is not None and r.more_lines(text):
            self._newline()
        elif r.paste_mode:
            r.insert("\n")
        else:
            self.finish = True

    def _newline(self) -> None:
        r = self.reader
        start, end = _line_bounds(r.buffer, r.pos)
        current = "".join(r.buffer[start:r.pos])
        indent = _get_this_line_indent(r.buffer, r.pos)
        r.update_last_used_indentation()
        r.insert("\n")
        if r.paste_mode:
            return
        text = " " * min(indent, len(current)) if current.strip() else " " * len(current)
        if _opens_block(current):
            text += r.last_used_indentation or " " * 4
        r.insert(text)


class backspace_dedent(commands.backspace):
    pass


class ReadlineAlikeReader(CompletingReader):
    """The reader used by the REPL and by ``input()``."""

    assume_immutable_completions = False
    use_brackets = False
    sort_in_column = True

    def __init__(self, console, config: ReadlineConfig | None = None) -> None:
        super().__init__(console)
        self.config = config if config is not None else ReadlineConfig()
        self.last_used_indentation: str | None = None
        self.ps1 = ">>> "
        self.ps2 = ">>> "
        self.ps3 = "... "
        self.ps4 = "... "

    def collect_keymap(self):
        return super().collect_keymap() + (
            (r"\n", "maybe-accept"),
            (r"\r", "maybe-accept"),
            (r"\C-m", "maybe-accept"),
            (r"\C-j", "maybe-accept"),
            (r"\M-\r", "insert-nl"),
            (r"\M-\n", "insert-nl"),
        )

    def collect_commands(self):
        table = super().collect_commands()
        table["maybe-accept"] = maybe_accept
        table["maybe_accept"] = maybe_accept
        return table

    def paste_safe_commands(self) -> set:
        return super().paste_safe_commands() | {maybe_accept}

    def error(self, msg: str = "none") -> None:
        self.console.beep()

    def update_last_used_indentation(self) -> None:
        """Remember the indentation the user chose for the body of the
        block the cursor is in (relative to the line that opened it)."""
        start, _ = _line_bounds(self.buffer, self.pos)
        if start == 0:
            return
        prev_start, _ = _line_bounds(self.buffer, start - 1)
        prev = "".join(self.buffer[prev_start:start - 1])
        current = "".join(self.buffer[start:self.pos])
        if not _opens_block(prev) or not current.strip():
            return
        prev_indent = len(prev) - len(prev.lstrip(" "))
        cur_indent = len(current) - len(current.lstrip(" "))
        if cur_indent > prev_indent:
            self.last_used_indentation = " " * (cur_indent - prev_indent)

    def get_stem(self) -> str:
        delims = self.config.completer_delims
        b = self.buffer
        p = self.pos - 1
        while p >= 0 and b[p] not in delims:
            p -= 1
        return "".join(b[p + 1:self.pos])

    def get_module_completions(self):
        completer = self.config.module_completer
        if completer is None:
            return None
        line = "".join(self.buffer[:self.pos])
        try:
            result = completer.get_completions(line)
        except Exception:
            return None
        if result is None:
            return None
        names, values, action = result
        parsed = ImportParser(line).parse()
        typed = ""
        if parsed is not None:
            from_name, name = parsed
            typed = name if name is not None else from_name
        stem = self.get_stem()
        cut = len(typed) - len(stem)
        if cut > 0:
            pairs = [(n[cut:], v) for n, v in zip(names, values) if len(n) >= cut]
            names = [n for n, _ in pairs]
            values = [v for _, v in pairs]
        colorize = self.config.colorize_completions
        if colorize is not None and names:
            names = colorize(names, values)
        return list(names), action

    def get_completions(self, stem: str):
        module = self.get_module_completions()
        if module is not None:
            return module
        completer = self.config.readline_completer
        if completer is None:
            return [], None
        results = []
        state = 0
        with warnings.catch_warnings(action="ignore"):
            while True:
                try:
                    match = completer(stem, state)
                except Exception:
                    break
                if not isinstance(match, str):
                    break
                results.append(match)
                state += 1
        seen = set()
        unique = []
        for m in results:
            if m not in seen:
                seen.add(m)
                unique.append(m)
        unique.sort(key=stripcolor)
        return unique, None

    def get_trimmed_history(self, maxlength: int) -> list[str]:
        if maxlength >= 0:
            cut = len(self.history) - maxlength
            if cut < 0:
                cut = 0
        else:
            cut = 0
        return self.history[cut:]


# --- the module-level readline replacement ----------------------------------------

@dataclass(slots=True)
class _ReadlineWrapper:
    f_in: int = -1
    f_out: int = -1
    reader: ReadlineAlikeReader | None = field(default=None, repr=False)
    saved_history_length: int = -1
    startup_hook: object | None = None
    config: ReadlineConfig = field(default_factory=ReadlineConfig, repr=False)
    history_file: str | None = None

    def __post_init__(self) -> None:
        if self.f_in == -1:
            self.f_in = os.dup(0)
        if self.f_out == -1:
            self.f_out = os.dup(1)

    def get_reader(self) -> ReadlineAlikeReader:
        if self.reader is None:
            console = Console(self.f_in, self.f_out, encoding=ENCODING)
            self.reader = ReadlineAlikeReader(console=console, config=self.config)
        return self.reader

    def input(self, prompt: object = "") -> str:
        try:
            reader = self.get_reader()
        except _error:
            assert raw_input is not None
            return raw_input(prompt)
        prompt_str = str(prompt)
        reader.ps1 = prompt_str
        sys.audit("builtins.input", prompt_str)
        saved_auto = reader.should_auto_add_history
        saved_more = reader.more_lines
        reader.should_auto_add_history = False
        reader.more_lines = None
        try:
            result = reader.readline(startup_hook=self.startup_hook)
        finally:
            reader.should_auto_add_history = saved_auto
            reader.more_lines = saved_more
        sys.audit("builtins.input/result", result)
        return result

    def multiline_input(self, more_lines, ps1, ps2) -> str:
        """Read an input made of one or more lines; *more_lines(text)*
        says whether *text* needs more lines."""
        reader = self.get_reader()
        saved = reader.more_lines
        try:
            reader.more_lines = more_lines
            reader.ps1 = ps1
            reader.ps2 = ps1
            reader.ps3 = ps2
            reader.ps4 = ""
            with warnings.catch_warnings(action="ignore"):
                return reader.readline()
        finally:
            reader.more_lines = saved
            reader.paste_mode = False

    def parse_and_bind(self, string: str) -> None:
        pass

    def set_completer(self, function=None) -> None:
        self.config.readline_completer = function

    def get_completer(self):
        return self.config.readline_completer

    def set_completer_delims(self, delimiters) -> None:
        self.config.completer_delims = frozenset(delimiters)

    def get_completer_delims(self) -> str:
        return "".join(sorted(self.config.completer_delims))

    def _histline(self, line: str) -> str:
        line = line.rstrip("\n")
        return line

    def get_history_length(self) -> int:
        return self.saved_history_length

    def set_history_length(self, length: int) -> None:
        self.saved_history_length = length

    def get_current_history_length(self) -> int:
        return len(self.get_reader().history)

    def read_history_file(self, filename: str = "~/.history") -> None:
        # the REPL keeps appending new entries to the file history came from
        self.history_file = filename
        history = self.get_reader().history
        with open(os.path.expanduser(filename), "rb") as f:
            data = f.read()
        is_editline = data.startswith(b"_HiStOrY_V2_")
        if is_editline:
            data = data[len(b"_HiStOrY_V2_"):]
        pending: list[str] = []
        for raw in data.split(b"\n"):
            line = raw.decode("utf-8", errors="replace").replace("\0", "")
            if is_editline:
                line = line.replace("\\040", " ")
            if line.endswith("\r"):
                pending.append(line[:-1])
                continue
            if not line and not pending:
                continue
            pending.append(line)
            history.append("\n".join(pending))
            pending = []
        if pending:
            history.append("\n".join(pending))

    def write_history_file(self, filename: str = "~/.history") -> None:
        maxlength = self.saved_history_length
        history = self.get_reader().get_trimmed_history(maxlength)
        entries = "".join(entry.replace("\n", "\r\n") + "\n" for entry in history)
        with open(os.path.expanduser(filename), "w", encoding="utf-8",
                  newline="\n") as f:
            f.write(entries)

    def append_history_file(self, nelements: int = 1, filename: str = "~/.history") -> None:
        reader = self.get_reader()
        if nelements <= 0:
            return
        entries = reader.history[-nelements:]
        if not entries:
            return
        text = "".join(e.replace("\n", "\r\n") + "\n" for e in entries)
        with open(os.path.expanduser(filename), "a", encoding="utf-8",
                  newline="\n") as f:
            f.write(text)

    def clear_history(self) -> None:
        del self.get_reader().history[:]

    def get_history_item(self, index: int) -> str | None:
        history = self.get_reader().history
        if 1 <= index <= len(history):
            return history[index - 1]
        return None

    def remove_history_item(self, index: int) -> None:
        history = self.get_reader().history
        if 0 <= index < len(history):
            del history[index]
        else:
            raise ValueError(f"No history item at position {index}")

    def replace_history_item(self, index: int, line: str) -> None:
        history = self.get_reader().history
        if 0 <= index < len(history):
            history[index] = self._histline(line)
        else:
            raise ValueError(f"No history item at position {index}")

    def add_history(self, line: str) -> None:
        self.get_reader().history.append(self._histline(line))

    def set_auto_history(self, enabled: bool) -> None:
        self.get_reader().should_auto_add_history = bool(enabled)

    def set_startup_hook(self, function=None) -> None:
        self.startup_hook = function

    def get_line_buffer(self) -> str:
        return self.get_reader().get_unicode()

    def _get_idxs(self) -> tuple[int, int]:
        reader = self.get_reader()
        start = cursor = reader.pos
        buf = reader.buffer
        while start > 0 and buf[start - 1] not in self.config.completer_delims:
            start -= 1
        return start, cursor

    def get_begidx(self) -> int:
        return self._get_idxs()[0]

    def get_endidx(self) -> int:
        return self._get_idxs()[1]

    def insert_text(self, text: str) -> None:
        self.get_reader().insert(text)


_wrapper = _ReadlineWrapper()

# the readline API
parse_and_bind = _wrapper.parse_and_bind
set_completer = _wrapper.set_completer
get_completer = _wrapper.get_completer
set_completer_delims = _wrapper.set_completer_delims
get_completer_delims = _wrapper.get_completer_delims
get_history_length = _wrapper.get_history_length
set_history_length = _wrapper.set_history_length
get_current_history_length = _wrapper.get_current_history_length
read_history_file = _wrapper.read_history_file
write_history_file = _wrapper.write_history_file
append_history_file = _wrapper.append_history_file
clear_history = _wrapper.clear_history
get_history_item = _wrapper.get_history_item
remove_history_item = _wrapper.remove_history_item
replace_history_item = _wrapper.replace_history_item
add_history = _wrapper.add_history
set_auto_history = _wrapper.set_auto_history
set_startup_hook = _wrapper.set_startup_hook
get_line_buffer = _wrapper.get_line_buffer
get_begidx = _wrapper.get_begidx
get_endidx = _wrapper.get_endidx
insert_text = _wrapper.insert_text

# pyrepl extensions
multiline_input = _wrapper.multiline_input
_get_reader = _wrapper.get_reader


def _make_stub(name):
    def stub(*args, **kwargs):
        import warnings as _w
        _w.warn(f"readline.{name}() not implemented", stacklevel=2)
    stub.__name__ = name
    return stub


for _name in ("read_init_file", "redisplay", "set_pre_input_hook"):
    globals()[_name] = _make_stub(_name)

raw_input = None


def _setup(namespace) -> None:
    """Replace builtins.input with the pyrepl editor (once) and install
    a completer for *namespace*."""
    global raw_input
    if raw_input is not None:
        return
    try:
        f_in = sys.stdin.fileno()
        f_out = sys.stdout.fileno()
    except (AttributeError, ValueError, OSError):
        return
    if not os.isatty(f_in) or not os.isatty(f_out):
        return
    _wrapper.f_in = f_in
    _wrapper.f_out = f_out
    import builtins
    raw_input = builtins.input
    builtins.input = _wrapper.input
    if not isinstance(namespace, dict):
        namespace = vars(namespace)
    use_basic = False
    if not sys.flags.ignore_environment:
        use_basic = bool(os.getenv("PYTHON_BASIC_COMPLETER"))
    if use_basic and RLCompleter is not None:
        completer = RLCompleter(namespace)
    else:
        completer = FancyCompleter(namespace)
    _wrapper.config.readline_completer = completer.complete
    _wrapper.config.module_completer = ModuleCompleter(namespace)
    use_colors = getattr(completer, "use_colors", False)
    if use_colors:
        theme = getattr(completer, "theme", None)

        def colorize(names, values, _theme=theme):
            return colorize_matches(names, values, _theme)

        _wrapper.config.colorize_completions = colorize
