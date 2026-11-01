"""The terminal abstraction used by the reader, and the interpreter
console the REPL feeds statements to."""

from __future__ import annotations

import _colorize
import ast
import code
import linecache
import os
import sys
import traceback
from abc import ABC, abstractmethod
from dataclasses import dataclass, field

TYPE_CHECKING = False
if TYPE_CHECKING:
    from .render import RenderedScreen


@dataclass
class Event:
    """An input event: ``evt`` is "key", "resize", "scroll", ...; for keys
    ``data`` is the key name and ``raw`` the bytes that produced it."""
    evt: str
    data: str
    raw: bytes = b""


class Console(ABC):
    """What the reader needs from a terminal."""

    posxy: tuple[int, int]
    screen: list[str]
    height: int = 25
    width: int = 80

    def __init__(self, f_in: int | object = 0, f_out: int | object = 1,
                 term: str = "", encoding: str = "") -> None:
        self.encoding = encoding or sys.getdefaultencoding()
        self.input_fd = f_in if isinstance(f_in, int) else f_in.fileno()
        self.output_fd = f_out if isinstance(f_out, int) else f_out.fileno()
        self.term = term

    @abstractmethod
    def refresh(self, rendered_screen: "RenderedScreen") -> None: ...

    @abstractmethod
    def prepare(self) -> None: ...

    @abstractmethod
    def restore(self) -> None: ...

    @abstractmethod
    def move_cursor(self, x: int, y: int) -> None: ...

    @abstractmethod
    def set_cursor_vis(self, visible: bool) -> None: ...

    @abstractmethod
    def getheightwidth(self) -> tuple[int, int]:
        """(height, width) of the terminal."""

    @abstractmethod
    def get_event(self, block: bool = True) -> Event | None:
        """The next input event; None if *block* is false and none is ready."""

    @abstractmethod
    def push_char(self, char: int | bytes) -> None: ...

    @abstractmethod
    def beep(self) -> None: ...

    @abstractmethod
    def clear(self) -> None: ...

    @abstractmethod
    def finish(self) -> None:
        """Move below the edited text, ready for program output."""

    @abstractmethod
    def flushoutput(self) -> None: ...

    @abstractmethod
    def forgetinput(self) -> None: ...

    @abstractmethod
    def getpending(self) -> Event: ...

    @abstractmethod
    def wait(self, timeout: float | None) -> bool:
        """Wait up to *timeout* milliseconds for input; True if some came."""

    @abstractmethod
    def repaint(self) -> None: ...

    @property
    def input_hook(self):
        """A callable to run while waiting for input (e.g. Tk's event
        loop), or None."""
        return None


class InteractiveColoredConsole(code.InteractiveConsole):
    """An InteractiveConsole that colours tracebacks and runs every
    statement of a multi-statement input on its own (only the last one
    echoes its value)."""

    STATEMENT_FAILED = object()

    def __init__(self, locals=None, filename="<console>", *, local_exit=False):
        super().__init__(locals=locals, filename=filename, local_exit=local_exit)
        self.can_colorize = _colorize.can_colorize()

    def showsyntaxerror(self, filename=None, **kwargs):
        super().showsyntaxerror(filename=filename, colorize=self.can_colorize, **kwargs)

    def showtraceback(self):
        typ, value, tb = sys.exc_info()
        tb = tb.tb_next if tb is not None else None
        value.__traceback__ = tb
        self._report(typ, value, tb, colorize=self.can_colorize)

    def runcode(self, code):
        try:
            exec(code, self.locals)
        except SystemExit:
            raise
        except BaseException:
            self.showtraceback()
            return self.STATEMENT_FAILED
        return None

    def _register(self, codeobj, source, filename):
        register = getattr(linecache, "_register_code", None)
        if register is not None:
            try:
                register(codeobj, source, filename)
            except Exception:
                pass

    def runsource(self, source, filename="<input>", symbol="single"):
        compiler = self.compile.compiler
        try:
            tree = compiler(source, filename, "exec", ast.PyCF_ONLY_AST,
                            incomplete_input=False)
        except (SyntaxError, OverflowError, ValueError):
            self.showsyntaxerror(filename, source=source)
            return False
        except (MemoryError, RecursionError):
            self._show_compile_failure()
            return False
        statements = tree.body
        for index, stmt in enumerate(statements):
            last = index == len(statements) - 1
            if last:
                unit = ast.Interactive([stmt])
                mode = symbol
            else:
                unit = ast.Module([stmt], [])
                mode = "exec"
            try:
                codeobj = compiler(unit, filename, mode)
            except SyntaxError as exc:
                self._adjust_await_error(exc)
                self.showsyntaxerror(filename, source=source)
                return False
            except (OverflowError, ValueError):
                self.showsyntaxerror(filename, source=source)
                return False
            except (MemoryError, RecursionError):
                self._show_compile_failure()
                return False
            if codeobj is None:
                return True
            self._register(codeobj, source, filename)
            if self.runcode(codeobj) is self.STATEMENT_FAILED:
                break
        return False

    def _adjust_await_error(self, exc):
        if exc.args and exc.args[0] == "'await' outside function":
            hint = "asyncio REPL"
            if hint not in exc.msg:
                exc.msg = (f"{exc.msg}; use 'python -m asyncio' "
                           "to start the asyncio REPL")
                exc.args = (exc.msg,) + exc.args[1:]

    def _show_compile_failure(self):
        typ, value, tb = sys.exc_info()
        lines = traceback.format_exception_only(typ, value,
                                                colorize=self.can_colorize)
        self.write("".join(lines))
