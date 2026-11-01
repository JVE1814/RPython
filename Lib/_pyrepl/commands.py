"""Editing commands.

Each command is a class; the reader instantiates it with the key events
that triggered it and calls ``do()``.  The command name used in keymaps is
the class name with underscores replaced by dashes (``kill_line`` is
"kill-line").  Commands that only move the cursor leave ``reader.dirty``
alone, so the reader just repositions the cursor instead of redrawing.
"""

from __future__ import annotations

import os
import signal

TYPE_CHECKING = False
if TYPE_CHECKING:
    from .reader import Reader


class Command:
    finish: bool = False
    kills_digit_arg: bool = True

    def __init__(self, reader: "Reader", event_name: str, event: list[str]) -> None:
        self.reader = reader
        self.event_name = event_name
        self.event = event

    def do(self) -> None:
        pass

    @classmethod
    def command_name(cls) -> str:
        return cls.__name__.replace("_", "-")


class EditCommand(Command):
    """A command that changes the buffer."""


class MotionCommand(Command):
    """A command that only moves the cursor."""


class KillCommand(Command):
    def kill_range(self, start: int, end: int) -> None:
        if start == end:
            return
        r = self.reader
        text = "".join(r.buffer[start:end])
        if isinstance(r.last_command_instance, KillCommand) and r.kill_ring:
            if start < r.pos:
                r.kill_ring[-1] = text + r.kill_ring[-1]
            else:
                r.kill_ring[-1] = r.kill_ring[-1] + text
        else:
            r.kill_ring.append(text)
        del r.buffer[start:end]
        r.pos = start
        r.dirty = True


class YankCommand(Command):
    pass


# --- numeric argument and screen -------------------------------------------

class digit_arg(Command):
    kills_digit_arg = False

    def do(self) -> None:
        r = self.reader
        key = self.event[-1]
        if key == "-":
            r.arg = -1 if r.arg is None else -r.arg
        else:
            digit = int(key)
            if r.arg is None or r.arg == -1 and r.arg_sign_only:
                r.arg = -digit if r.arg == -1 else digit
            else:
                r.arg = r.arg * 10 + (digit if r.arg >= 0 else -digit)
        r.arg_sign_only = key == "-"
        r.dirty = True


class clear_screen(Command):
    def do(self) -> None:
        r = self.reader
        r.console.clear()
        r.invalidate_full()


class refresh(Command):
    def do(self) -> None:
        self.reader.invalidate_full()


class repaint(Command):
    def do(self) -> None:
        self.reader.console.repaint()
        self.reader.invalidate_full()


# --- killing and yanking -----------------------------------------------------

class kill_line(KillCommand):
    def do(self) -> None:
        r = self.reader
        end = r.eol()
        if end == r.pos and end < len(r.buffer):
            end += 1  # at end of line: kill the newline
        elif "".join(r.buffer[r.pos:end]).isspace():
            end = min(end + 1, len(r.buffer))
        self.kill_range(r.pos, end)


class unix_line_discard(KillCommand):
    def do(self) -> None:
        r = self.reader
        self.kill_range(r.bol(), r.pos)


class unix_word_rubout(KillCommand):
    def do(self) -> None:
        r = self.reader
        for _ in range(r.get_arg()):
            p = r.pos
            while p > 0 and r.buffer[p - 1].isspace():
                p -= 1
            while p > 0 and not r.buffer[p - 1].isspace():
                p -= 1
            self.kill_range(p, r.pos)


class kill_word(KillCommand):
    def do(self) -> None:
        r = self.reader
        for _ in range(r.get_arg()):
            self.kill_range(r.pos, r.eow())


class backward_kill_word(KillCommand):
    def do(self) -> None:
        r = self.reader
        for _ in range(r.get_arg()):
            self.kill_range(r.bow(), r.pos)


class yank(YankCommand):
    def do(self) -> None:
        r = self.reader
        if not r.kill_ring:
            r.error("nothing to yank")
            return
        r.insert(r.kill_ring[-1])


class yank_pop(YankCommand):
    def do(self) -> None:
        r = self.reader
        if not r.kill_ring:
            r.error("nothing to yank")
            return
        if not isinstance(r.last_command_instance, YankCommand):
            r.error("previous command was not a yank")
            return
        previous = r.kill_ring[-1]
        r.kill_ring.insert(0, r.kill_ring.pop())
        current = r.kill_ring[-1]
        start = r.pos - len(previous)
        r.buffer[start:r.pos] = list(current)
        r.pos = start + len(current)
        r.dirty = True


# --- interrupting --------------------------------------------------------------

class interrupt(Command):
    def do(self) -> None:
        r = self.reader
        r.abandon()
        r.console.finish()
        raise KeyboardInterrupt


class suspend(Command):
    def do(self) -> None:
        r = self.reader
        sigstop = getattr(signal, "SIGSTOP", None)
        if sigstop is None:
            return
        r.console.restore()
        os.kill(os.getpid(), sigstop)
        r.console.prepare()
        r.invalidate_full()


class cancel(Command):
    def do(self) -> None:
        self.reader.console.beep()


# --- motion ------------------------------------------------------------------

class up(MotionCommand):
    def do(self) -> None:
        r = self.reader
        for _ in range(r.get_arg()):
            if not r.move_vertically(-1):
                r.on_top_row()
                return


class down(MotionCommand):
    def do(self) -> None:
        r = self.reader
        for _ in range(r.get_arg()):
            if not r.move_vertically(1):
                r.on_bottom_row()
                return


class left(MotionCommand):
    def do(self) -> None:
        r = self.reader
        for _ in range(r.get_arg()):
            if r.pos > 0:
                r.pos -= 1
            else:
                r.error("start of buffer")


class right(MotionCommand):
    def do(self) -> None:
        r = self.reader
        for _ in range(r.get_arg()):
            if r.pos < len(r.buffer):
                r.pos += 1
            else:
                r.error("end of buffer")


class beginning_of_line(MotionCommand):
    def do(self) -> None:
        self.reader.pos = self.reader.bol()


class end_of_line(MotionCommand):
    def do(self) -> None:
        self.reader.pos = self.reader.eol()


class home(MotionCommand):
    def do(self) -> None:
        self.reader.pos = 0


class end(MotionCommand):
    def do(self) -> None:
        self.reader.pos = len(self.reader.buffer)


class forward_word(MotionCommand):
    def do(self) -> None:
        r = self.reader
        for _ in range(r.get_arg()):
            r.pos = r.eow()


class backward_word(MotionCommand):
    def do(self) -> None:
        r = self.reader
        for _ in range(r.get_arg()):
            r.pos = r.bow()


# --- editing -------------------------------------------------------------------

class self_insert(EditCommand):
    def do(self) -> None:
        r = self.reader
        text = "".join(self.event) * r.get_arg()
        r.insert(text)


class insert_nl(EditCommand):
    def do(self) -> None:
        r = self.reader
        r.insert("\n" * r.get_arg())


class transpose_characters(EditCommand):
    def do(self) -> None:
        r = self.reader
        b = r.buffer
        p = r.pos
        if p == 0 or len(b) < 2:
            r.error("cannot transpose at start of buffer")
            return
        if p == len(b) or b[p] == "\n":
            p -= 1
            if p == 0:
                r.error("cannot transpose at start of buffer")
                return
        b[p - 1], b[p] = b[p], b[p - 1]
        r.pos = p + 1
        r.dirty = True


class backspace(EditCommand):
    def do(self) -> None:
        r = self.reader
        b = r.buffer
        for _ in range(r.get_arg()):
            if r.pos == 0:
                r.error("can't backspace at start")
                return
            start = r.bol()
            before = b[start:r.pos]
            if before and all(c == " " for c in before) and not r.paste_mode:
                # inside the indentation: go back to the previous indent stop
                count = (len(before) - 1) % 4 + 1
                del b[r.pos - count:r.pos]
                r.pos -= count
            else:
                r.pos -= 1
                del b[r.pos]
            r.dirty = True


class delete(EditCommand):
    def do(self) -> None:
        r = self.reader
        b = r.buffer
        if self.event[-1] == "\x04":
            # Ctrl-D: end of file on an empty buffer; on a finished
            # (trailing newline) or blank last line, accept what is there
            if not b:
                r.console.finish()
                raise EOFError
            if r.pos == len(b):
                line = "".join(b[r.bol():])
                if b[-1] == "\n" or (r.bol() > 0 and not line.strip()):
                    self.finish = True
                return
        for _ in range(r.get_arg()):
            if r.pos < len(b):
                del b[r.pos]
                r.dirty = True
            else:
                r.error("end of buffer")
                return


class accept(Command):
    finish = True


class help(Command):
    def do(self) -> None:
        r = self.reader
        r.msg = r.help_text()
        r.dirty = True


class invalid_key(Command):
    def do(self) -> None:
        pending = "".join(self.event)
        self.reader.error(f"`{pending}' not bound")


class invalid_command(Command):
    def do(self) -> None:
        self.reader.error(f"command `{self.event_name}' not known")


class show_history(Command):
    def do(self) -> None:
        r = self.reader
        r.console.restore()
        try:
            from .pager import get_pager
            pager = get_pager()
            pager(os.linesep.join(r.history[:-1] if getattr(r, "historyi", 0) else r.history),
                  "History")
        finally:
            r.console.prepare()
            r.invalidate_full()


class paste_mode(Command):
    def do(self) -> None:
        r = self.reader
        r.paste_mode = not r.paste_mode
        r.dirty = True


class enable_bracketed_paste(Command):
    def do(self) -> None:
        r = self.reader
        r.paste_mode = True
        r.in_bracketed_paste = True


class disable_bracketed_paste(Command):
    def do(self) -> None:
        r = self.reader
        r.paste_mode = False
        r.in_bracketed_paste = False
        r.dirty = True


class quoted_insert(Command):
    kills_digit_arg = False

    def do(self) -> None:
        self.reader.push_input_trans(_QuotedInsertTranslator(self.reader))


class _QuotedInsertTranslator:
    """Takes the next key and inserts it literally."""

    def __init__(self, reader) -> None:
        self.reader = reader
        self.result = None

    def push(self, evt) -> None:
        self.result = ("self-insert", [evt.raw.decode(self.reader.console.encoding, "replace") or evt.data])

    def get(self):
        result, self.result = self.result, None
        if result is not None:
            self.reader.pop_input_trans()
        return result

    def empty(self) -> bool:
        return self.result is None


def _change_word(reader, transform) -> None:
    for _ in range(reader.get_arg()):
        start = reader.pos
        end = reader.eow()
        word = "".join(reader.buffer[start:end])
        reader.buffer[start:end] = list(transform(word))
        reader.pos = end
    reader.dirty = True


class upcase_word(EditCommand):
    def do(self) -> None:
        _change_word(self.reader, str.upper)


class downcase_word(EditCommand):
    def do(self) -> None:
        _change_word(self.reader, str.lower)


class capitalize_word(EditCommand):
    def do(self) -> None:
        def cap(word):
            for i, c in enumerate(word):
                if c.isalnum():
                    return word[:i] + c.upper() + word[i + 1:].lower()
            return word
        _change_word(self.reader, cap)


class transpose_words(EditCommand):
    def do(self) -> None:
        r = self.reader
        b = r.buffer
        end2 = r.eow()
        start2 = r.bow(end2)
        start1 = r.bow(start2)
        end1 = r.eow(start1)
        if start1 >= start2 or end1 > start2:
            r.error("nothing to transpose")
            return
        w1, w2 = b[start1:end1], b[start2:end2]
        b[start2:end2] = w1
        b[start1:end1] = w2
        r.pos = end2
        r.dirty = True


def all_commands() -> dict[str, type[Command]]:
    """Map of command names to classes defined in this module."""
    table = {}
    for value in globals().values():
        if isinstance(value, type) and issubclass(value, Command) and value.__module__ == __name__:
            if not value.__name__[0].isupper():
                table[value.command_name()] = value
                table[value.__name__] = value
    return table
