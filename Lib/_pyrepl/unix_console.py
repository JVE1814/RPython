"""Console implementation for Unix terminals.

The console keeps the lines it last drew (as RenderLines) and, on each
refresh, rewrites only the cells that changed.  Positions are tracked
relative to where the prompt started; the cursor is moved with relative
motions so input may begin in the middle of a terminal line.  When the
input is taller than the terminal, a window of ``height`` rows starting at
``offset`` is shown and moving the cursor outside of it scrolls.
"""

from __future__ import annotations

import errno
import os
import posix
import select
import signal
import struct
import termios
import time
from fcntl import ioctl

from . import terminfo
from .console import Console, Event
from .fancy_termios import tcgetattr, tcsetattr
from .render import RenderLine, diff_render_lines, render_cells, requires_cursor_resync
from .trace import trace
from .unix_eventqueue import EventQueue

RESET_SEQUENCE = "\x1b[0m"
BRACKETED_PASTE_ON = b"\x1b[?2004h"
BRACKETED_PASTE_OFF = b"\x1b[?2004l"


class InvalidTerminal(RuntimeError):
    pass


_error = (termios.error, InvalidTerminal)

_REQUIRED = ("bel", "clear", "cup", "el")
_OPTIONAL = ("civis", "cnorm", "cub", "cub1", "cud", "cud1", "cuf", "cuf1",
             "cuu", "cuu1", "dch", "dch1", "hpa", "ich", "ich1", "ind", "pad",
             "ri", "rmkx", "smkx")


class UnixConsole(Console):
    def __init__(self, f_in: int | object = 0, f_out: int | object = 1,
                 term: str = "", encoding: str = "") -> None:
        super().__init__(f_in, f_out, term, encoding)
        self.pollob = select.poll()
        self.pollob.register(self.input_fd, select.POLLIN)
        self.terminfo = terminfo.TermInfo(term or None)
        self.term = term
        self.caps: dict[str, bytes | None] = {}
        for name in _REQUIRED:
            value = self.terminfo.get(name)
            if not value:
                raise InvalidTerminal(f"terminal doesn't have the required {name} capability")
            self.caps[name] = value
        for name in _OPTIONAL:
            self.caps[name] = self.terminfo.get(name) or None
        self.event_queue = EventQueue(self.input_fd, self.encoding, self.terminfo)
        self.cursor_visible = True
        self.__buffer: list[bytes] = []
        self.__svtermstate = None
        self.__lines: list[RenderLine] = []
        self.__offset = 0
        self.__rows_seen = 1
        self.__posxy_known = True
        self.__needs_full_redraw = False
        self.__last_size: tuple[int, int] | None = None
        self.screen: list[str] = []
        self.posxy = (0, 0)
        try:
            self.height, self.width = self.getheightwidth()
        except Exception:
            self.height, self.width = 25, 80

    # -- low level output ------------------------------------------------------------

    def __write(self, text: str) -> None:
        if text:
            self.__buffer.append(text.encode(self.encoding, "replace"))

    def __write_code(self, cap: bytes | None, *args) -> None:
        if cap:
            self.__buffer.append(terminfo.tparm(cap, *args))

    def flushoutput(self) -> None:
        for chunk in self.__buffer:
            if chunk:
                self.__os_write(chunk)
        self.__buffer = []

    def __os_write(self, data: bytes) -> None:
        while data:
            try:
                written = os.write(self.output_fd, data)
            except InterruptedError:
                continue
            except BlockingIOError:
                time.sleep(0.01)
                continue
            if not isinstance(written, int) or written <= 0:
                return
            data = data[written:]

    # -- cursor movement -----------------------------------------------------------------

    def __visible_row(self, y: int) -> int:
        return y - self.__offset

    def __move(self, x: int, y: int) -> None:
        caps = self.caps
        if not self.__posxy_known:
            self.__write_code(caps["cup"], max(self.__visible_row(y), 0), x)
            self.posxy = (x, y)
            self.__posxy_known = True
            self.__rows_seen = max(self.__rows_seen, y + 1)
            return
        px, py = self.posxy
        if y >= self.__rows_seen:
            # rows below the last one we touched do not exist yet: newlines
            # create them (scrolling the terminal if needed)
            if py != self.__rows_seen - 1:
                self.__move_vertical(py, self.__rows_seen - 1)
            for _ in range(y - (self.__rows_seen - 1)):
                self.__write("\n")
            self.__rows_seen = y + 1
        else:
            self.__move_vertical(py, y)
        self.__move_horizontal(px, x)
        self.posxy = (x, y)

    def __move_vertical(self, py: int, y: int) -> None:
        caps = self.caps
        if y > py:
            if caps["cud"]:
                self.__write_code(caps["cud"], y - py)
            else:
                for _ in range(y - py):
                    self.__write_code(caps["cud1"])
        elif y < py:
            if caps["cuu"]:
                self.__write_code(caps["cuu"], py - y)
            else:
                for _ in range(py - y):
                    self.__write_code(caps["cuu1"])

    def __move_horizontal(self, px: int, x: int) -> None:
        caps = self.caps
        if x > px:
            if caps["cuf"]:
                self.__write_code(caps["cuf"], x - px)
            else:
                for _ in range(x - px):
                    self.__write_code(caps["cuf1"])
        elif x < px:
            if caps["cub"]:
                self.__write_code(caps["cub"], px - x)
            elif caps["cub1"]:
                for _ in range(px - x):
                    self.__write_code(caps["cub1"])
            else:
                self.__write("\r")
                if x:
                    self.__move_horizontal(0, x)

    def move_cursor(self, x: int, y: int) -> None:
        if y < self.__offset or y >= self.__offset + self.height:
            self.event_queue.insert(Event("scroll", None))
            return
        self.__move(x, y)
        self.flushoutput()

    def __hide_cursor(self) -> None:
        if self.cursor_visible:
            self.__write_code(self.caps["civis"])
            self.cursor_visible = False

    def __show_cursor(self) -> None:
        if not self.cursor_visible:
            self.__write_code(self.caps["cnorm"])
            self.cursor_visible = True

    def set_cursor_vis(self, visible: bool) -> None:
        if visible:
            self.__show_cursor()
        else:
            self.__hide_cursor()
        self.flushoutput()

    # -- drawing -------------------------------------------------------------------------------

    def __advance(self, x: int, width: int) -> int:
        """Where the cursor ends after writing *width* cells from *x*."""
        return min(x + width, max(self.width - 1, 0))

    def __draw_cells(self, x: int, y: int, cells) -> None:
        text = render_cells(cells)
        self.__move(x, y)
        self.__write(text)
        width = sum(c.width for c in cells)
        if requires_cursor_resync(cells):
            self.__posxy_known = False
        else:
            self.posxy = (self.__advance(x, width), y)

    def __clear_eol(self) -> None:
        self.__write_code(self.caps["el"])

    def __draw_line(self, y: int, old: RenderLine | None, new: RenderLine) -> None:
        if old is None:
            self.__draw_cells(0, y, new.cells)
            if self.__needs_full_redraw:
                self.__clear_eol()
            return
        diff = diff_render_lines(old, new)
        if diff is None:
            return
        caps = self.caps
        width_changed = diff.old_changed_width != diff.new_changed_width
        tail = new.cells[diff.start_cell + len(diff.new_cells):]
        if not width_changed:
            self.__draw_cells(diff.start_x, y, diff.new_cells)
            return
        if (not tail or not old.cells[diff.start_cell + len(diff.old_cells):]) and not diff.old_cells:
            # appending at the end
            self.__draw_cells(diff.start_x, y, diff.new_cells)
            return
        if (len(diff.new_cells) == 1 and not diff.old_cells
                and diff.new_cells[0].width == 1 and caps["ich1"]
                and new.width < self.width):
            self.__move(diff.start_x, y)
            self.__write_code(caps["ich1"])
            self.__draw_cells(diff.start_x, y, diff.new_cells)
            return
        if (len(diff.old_cells) == 1 and not diff.new_cells
                and diff.old_cells[0].width == 1 and caps["dch1"]
                and old.width < self.width):
            self.__move(diff.start_x, y)
            self.__write_code(caps["dch1"])
            return
        # redraw from the change to the end of the line
        self.__draw_cells(diff.start_x, y, diff.new_cells + tuple(tail))
        if new.width < old.width:
            self.__clear_eol()

    def __scroll(self, new_offset: int) -> None:
        caps = self.caps
        old_offset = self.__offset
        if new_offset < old_offset and caps["ri"]:
            top = old_offset
            if self.__posxy_known:
                self.__move(self.posxy[0], top)
            for _ in range(old_offset - new_offset):
                self.__write_code(caps["ri"])
        elif new_offset > old_offset and caps["ind"]:
            bottom = old_offset + self.height - 1
            if self.__posxy_known:
                self.__move(self.posxy[0], min(bottom, self.__rows_seen - 1))
            for _ in range(new_offset - old_offset):
                self.__write_code(caps["ind"])
        else:
            self.__needs_full_redraw = True
        self.__offset = new_offset
        self.__rows_seen = max(self.__rows_seen, new_offset + self.height)
        self.__posxy_known = False
        self.__needs_full_redraw = True

    def refresh(self, rendered_screen) -> None:
        lines = list(rendered_screen.composed_lines)
        cx, cy = rendered_screen.cursor
        height = max(self.height, 1)
        size = (self.height, self.width)
        if self.__last_size is not None and size != self.__last_size:
            self.__needs_full_redraw = True
        self.__last_size = size
        offset = self.__offset
        if cy < offset:
            offset = cy
        elif cy >= offset + height:
            offset = cy - height + 1
        elif offset > 0 and len(lines) < offset + height:
            offset = max(len(lines) - height, 0)
        self.__hide_cursor()
        if offset != self.__offset:
            self.__scroll(offset)
        old_lines = self.__lines
        last_visible = min(len(lines), offset + height)
        for y in range(offset, last_visible):
            if self.__needs_full_redraw or y >= len(old_lines):
                old = None
            else:
                old = old_lines[y]
            self.__draw_line(y, old, lines[y])
        # rows that are no longer used
        for y in range(len(lines), min(len(old_lines), offset + height)):
            if y < offset:
                continue
            if old_lines[y].cells or self.__needs_full_redraw:
                self.__move(0, y)
                self.__clear_eol()
        self.__needs_full_redraw = False
        self.__lines = lines
        self.screen = [line.text for line in lines]
        if offset <= cy < offset + height:
            self.__move(cx, cy)
        self.__show_cursor()
        self.flushoutput()

    # -- setting up and tearing down -----------------------------------------------------

    def prepare(self) -> None:
        try:
            self.__svtermstate = tcgetattr(self.input_fd)
            raw = self.__svtermstate.copy()
            raw.iflag &= ~(termios.INPCK | termios.ISTRIP | termios.IXON)
            raw.oflag &= ~termios.OPOST
            raw.cflag &= ~(termios.CSIZE | termios.PARENB)
            raw.cflag |= termios.CS8
            raw.iflag |= termios.BRKINT
            raw.lflag &= ~(termios.ICANON | termios.ECHO | termios.IEXTEN)
            raw.lflag |= termios.ISIG
            raw.cc[termios.VMIN] = 1
            raw.cc[termios.VTIME] = 0
            self.__raw = raw
            tcsetattr(self.input_fd, termios.TCSADRAIN, raw)
        except termios.error:
            self.__svtermstate = None
            self.__raw = None
        try:
            self.height, self.width = self.getheightwidth()
        except Exception:
            pass
        self.__buffer = []
        self.__lines = []
        self.screen = []
        self.posxy = (0, 0)
        self.__offset = 0
        self.__rows_seen = 1
        self.__posxy_known = True
        self.__needs_full_redraw = False
        self.__last_size = None
        self.__write_code(self.caps["smkx"])
        self.__buffer.append(BRACKETED_PASTE_ON)
        try:
            self.old_sigwinch = signal.signal(signal.SIGWINCH, self.__sigwinch)
        except (ValueError, AttributeError, OSError):
            pass
        self.flushoutput()

    def restore(self) -> None:
        self.__write(RESET_SEQUENCE)
        self.__write_code(self.caps["rmkx"])
        self.__buffer.append(BRACKETED_PASTE_OFF)
        self.flushoutput()
        if self.__svtermstate is not None:
            try:
                tcsetattr(self.input_fd, termios.TCSADRAIN, self.__svtermstate)
            except termios.error:
                pass
        if hasattr(self, "old_sigwinch"):
            try:
                signal.signal(signal.SIGWINCH, self.old_sigwinch)
            except (ValueError, OSError, TypeError):
                pass
            else:
                del self.old_sigwinch

    def __sigwinch(self, signum, frame) -> None:
        try:
            self.height, self.width = self.getheightwidth()
        except Exception:
            pass
        self.event_queue.insert(Event("resize", None))
        old = getattr(self, "old_sigwinch", None)
        if callable(old):
            old(signum, frame)

    @property
    def input_hook(self):
        try:
            installed = posix._is_inputhook_installed()
        except AttributeError:
            return None
        if not installed:
            return None

        def run_hook():
            cooked = None
            saved = self.__svtermstate
            if saved is not None:
                try:
                    cooked = tcgetattr(self.input_fd)
                    cooked.oflag = saved.oflag
                    tcsetattr(self.input_fd, termios.TCSADRAIN, cooked)
                except termios.error:
                    cooked = None
            try:
                return posix._inputhook()
            finally:
                raw = getattr(self, "_UnixConsole__raw", None)
                if cooked is not None and raw is not None:
                    try:
                        tcsetattr(self.input_fd, termios.TCSADRAIN, raw)
                    except termios.error:
                        pass
        return run_hook

    # -- input ----------------------------------------------------------------------------

    def push_char(self, char: int | bytes) -> None:
        trace("push char {char!r}", char=char)
        self.event_queue.push(char)

    def get_event(self, block: bool = True) -> Event | None:
        while self.event_queue.empty():
            if not block and not self.wait(0):
                return None
            try:
                data = os.read(self.input_fd, 1024)
            except InterruptedError:
                continue
            except OSError as exc:
                if exc.errno == errno.EIO:
                    raise EOFError from exc
                raise
            if not data:
                raise EOFError
            for byte in data:
                self.push_char(byte)
        return self.event_queue.get()

    def wait(self, timeout: float | None = None) -> bool:
        while True:
            try:
                return bool(self.pollob.poll(timeout))
            except InterruptedError:
                continue

    def getpending(self) -> Event:
        event = Event("key", "", b"")
        while not self.event_queue.empty():
            e = self.event_queue.get()
            event.data += e.data
            event.raw += e.raw
        try:
            amount = struct.unpack("i", ioctl(self.input_fd, termios.FIONREAD, b"\0\0\0\0"))[0]
        except OSError:
            amount = 0
        if amount:
            raw = os.read(self.input_fd, amount)
            event.data += raw.decode(self.encoding, "replace")
            event.raw += raw
        return event

    def forgetinput(self) -> None:
        try:
            termios.tcflush(self.input_fd, termios.TCIFLUSH)
        except termios.error:
            pass

    def getheightwidth(self) -> tuple[int, int]:
        try:
            return int(os.environ["LINES"]), int(os.environ["COLUMNS"])
        except (KeyError, TypeError, ValueError):
            pass
        try:
            packed = ioctl(self.input_fd, termios.TIOCGWINSZ, b"\0" * 8)
            rows, cols = struct.unpack("hhhh", packed)[:2]
            if rows > 0 and cols > 0:
                return rows, cols
        except (OSError, AttributeError):
            pass
        try:
            packed = ioctl(self.output_fd, termios.TIOCGWINSZ, b"\0" * 8)
            rows, cols = struct.unpack("hhhh", packed)[:2]
            if rows > 0 and cols > 0:
                return rows, cols
        except (OSError, AttributeError):
            pass
        return 25, 80

    # -- misc -------------------------------------------------------------------------------

    def beep(self) -> None:
        self.__write_code(self.caps["bel"])
        self.flushoutput()

    def clear(self) -> None:
        self.__write_code(self.caps["clear"])
        self.posxy = (0, 0)
        self.__posxy_known = True
        self.__lines = []
        self.screen = []
        self.__offset = 0
        self.__rows_seen = 1
        self.flushoutput()

    def finish(self) -> None:
        y = len(self.__lines) - 1
        while y > 0 and not self.__lines[y].cells:
            y -= 1
        y = max(min(y, self.__offset + self.height - 1), 0)
        self.__move(0, y)
        self.__write("\r\n")
        self.__write(RESET_SEQUENCE)
        self.posxy = (0, y + 1)
        self.flushoutput()

    def repaint(self) -> None:
        self.__needs_full_redraw = True
        self.__posxy_known = False
