"""The line editor: a buffer, a cursor, a keymap and a screen.

``Reader`` keeps the edited text as a list of characters, translates key
events into commands through a keymap, and renders the buffer (with
prompts, wrapping and optional syntax colouring) into screen lines that
the console draws.  Subclasses add history and completion.
"""

from __future__ import annotations

import sys
import unicodedata
from typing import Callable

import _colorize

from . import commands, input as input_mod
from .content import ContentFragment, ContentLine, PromptContent, split_source_lines
from .layout import LayoutMap, layout_content_lines
from .render import RenderedScreen
from .trace import trace
from .utils import THEME, disp_char, gen_colors, style_tags, unbracket, wlen


def make_default_syntax_table():
    """Character classes used by word motion: 1 for word characters."""
    table = {}
    for c in map(chr, range(256)):
        table[c] = 1 if c.isalnum() or c == "_" else 0
    return table


def _word_char(c: str) -> bool:
    return c.isalnum() or c == "_"


default_keymap: tuple[tuple[str, str], ...] = tuple(
    [
        (r"\C-a", "beginning-of-line"),
        (r"\C-b", "left"),
        (r"\C-c", "interrupt"),
        (r"\C-d", "delete"),
        (r"\C-e", "end-of-line"),
        (r"\C-f", "right"),
        (r"\C-g", "cancel"),
        (r"\C-h", "backspace"),
        (r"\C-j", "accept"),
        (r"\<return>", "accept"),
        (r"\C-k", "kill-line"),
        (r"\C-l", "clear-screen"),
        (r"\C-m", "accept"),
        (r"\C-q", "quoted-insert"),
        (r"\C-t", "transpose-characters"),
        (r"\C-u", "unix-line-discard"),
        (r"\C-v", "quoted-insert"),
        (r"\C-w", "unix-word-rubout"),
        (r"\C-y", "yank"),
        (r"\C-z", "suspend"),
        (r"\M-b", "backward-word"),
        (r"\M-c", "capitalize-word"),
        (r"\M-d", "kill-word"),
        (r"\M-f", "forward-word"),
        (r"\M-l", "downcase-word"),
        (r"\M-t", "transpose-words"),
        (r"\M-u", "upcase-word"),
        (r"\M-y", "yank-pop"),
        (r"\M--", "digit-arg"),
        (r"\M-\<backspace>", "backward-kill-word"),
        (r"\<backspace>", "backspace"),
        (r"\<delete>", "delete"),
        (r"\<left>", "left"),
        (r"\<right>", "right"),
        (r"\<up>", "up"),
        (r"\<down>", "down"),
        (r"\<home>", "beginning-of-line"),
        (r"\<end>", "end-of-line"),
        (r"\eOF", "end-of-line"),
        (r"\eOH", "beginning-of-line"),
        (r"\e[1;5D", "backward-word"),
        (r"\e[1;5C", "forward-word"),
        (r"\e[3~", "delete"),
        (r"\e[200~", "enable_bracketed_paste"),
        (r"\e[201~", "disable_bracketed_paste"),
        (r"\<f1>", "help"),
        (r"\<f2>", "show-history"),
        (r"\<f3>", "paste-mode"),
    ]
    + [(rf"\M-{d}", "digit-arg") for d in "0123456789"]
)


class Reader:
    """Base line editor.  ``readline()`` runs a full edit session."""

    keymap: tuple[tuple[str, str], ...] = default_keymap
    help_lines = (
        "Keys: arrows move, Enter accepts, Ctrl-C interrupts, Ctrl-D on an",
        "empty line ends input, F2 shows history, F3 toggles paste mode.",
    )

    def __init__(self, console) -> None:
        self.console = console
        self.buffer: list[str] = []
        self.pos = 0
        self.ps1 = ">>> "
        self.ps2 = ">>> "
        self.ps3 = "... "
        self.ps4 = "... "
        self.msg = ""
        self.dirty = False
        self.finished = False
        self.paste_mode = False
        self.in_bracketed_paste = False
        self.arg: int | None = None
        self.arg_sign_only = False
        self.kill_ring: list[str] = []
        self.last_command: type | None = None
        self.last_command_instance: commands.Command | None = None
        self.more_lines: Callable[[str], bool] | None = None
        self.screen: list[str] = []
        self.cxy = (0, 0)
        self.layout = LayoutMap.empty()
        self.can_colorize = _colorize.can_colorize()
        self.scheduled_commands: list[str] = []
        self.threading_hook = None
        self.commands = self.collect_commands()
        self.input_trans_stack: list = []
        self.input_trans = input_mod.KeymapTranslator(
            self.collect_keymap(), invalid_cls="invalid-key",
            character_cls="self-insert")
        self._rendered: RenderedScreen | None = None

    # -- configuration -------------------------------------------------------

    def collect_keymap(self) -> tuple[tuple[str, str], ...]:
        return self.keymap

    def collect_commands(self) -> dict:
        return commands.all_commands()

    def push_input_trans(self, trans) -> None:
        self.input_trans_stack.append(self.input_trans)
        self.input_trans = trans

    def pop_input_trans(self) -> None:
        self.input_trans = self.input_trans_stack.pop()

    # -- buffer helpers ------------------------------------------------------

    def get_unicode(self) -> str:
        return "".join(self.buffer)

    def get_arg(self, default: int = 1) -> int:
        return default if self.arg is None else self.arg

    def bol(self, p: int | None = None) -> int:
        """Position of the start of the line containing *p*."""
        if p is None:
            p = self.pos
        b = self.buffer
        p -= 1
        while p >= 0 and b[p] != "\n":
            p -= 1
        return p + 1

    def eol(self, p: int | None = None) -> int:
        """Position of the end of the line containing *p*."""
        if p is None:
            p = self.pos
        b = self.buffer
        while p < len(b) and b[p] != "\n":
            p += 1
        return p

    def bow(self, p: int | None = None) -> int:
        """Start of the word before *p*."""
        if p is None:
            p = self.pos
        b = self.buffer
        p -= 1
        while p >= 0 and not _word_char(b[p]):
            p -= 1
        while p >= 0 and _word_char(b[p]):
            p -= 1
        return p + 1

    def eow(self, p: int | None = None) -> int:
        """End of the word after *p*."""
        if p is None:
            p = self.pos
        b = self.buffer
        while p < len(b) and not _word_char(b[p]):
            p += 1
        while p < len(b) and _word_char(b[p]):
            p += 1
        return p

    def insert(self, text: str | list[str]) -> None:
        text = list(text)
        self.buffer[self.pos:self.pos] = text
        self.pos += len(text)
        self.dirty = True

    def error(self, msg: str = "none") -> None:
        """Signal a failed command: just beep."""
        trace("error: {}", msg)
        self.console.beep()

    def help_text(self) -> str:
        return "\n".join(self.help_lines)

    # -- prompts -------------------------------------------------------------

    def get_prompt(self, lineno: int, cursor_on_line: bool) -> str:
        if self.paste_mode and not self.in_bracketed_paste:
            return "(paste) "
        if "\n" not in self.buffer:
            return self.ps1
        if lineno == 0:
            return self.ps2
        if self.ps4 and lineno == self.buffer.count("\n"):
            return self.ps4
        return self.ps3

    @staticmethod
    def process_prompt(prompt: str) -> tuple[str, int]:
        """The prompt as it should be printed (without readline's
        \\x01/\\x02 markers) and its width on screen."""
        shown = unbracket(prompt)
        return shown, wlen(unbracket(prompt, including_content=True))

    # -- screen ----------------------------------------------------------------

    def _theme(self):
        return THEME(force_color=True)

    def _prompt_content(self, prompt: str, theme) -> PromptContent:
        parts = prompt.split("\n")
        rendered = []
        for part in parts:
            text, width = self.process_prompt(part)
            if theme is not None and text:
                text = f"{theme.prompt}{text}{theme.reset}"
            rendered.append((text, width))
        leading = tuple(ContentFragment(t, w) for t, w in rendered[:-1])
        return PromptContent(leading, rendered[-1][0], rendered[-1][1])

    def content_lines(self) -> list[ContentLine]:
        text = self.get_unicode()
        theme = self._theme() if self.can_colorize else None
        tags = None
        if theme is not None and text:
            try:
                tags = style_tags(text, list(gen_colors(text)))
            except Exception:
                tags = None
        result = []
        for src in split_source_lines(text, self.pos):
            prompt = self._prompt_content(
                self.get_prompt(src.lineno, src.cursor_index is not None), theme)
            body = []
            for i, ch in enumerate(src.text):
                shown, width = disp_char(ch)
                style = ""
                if tags is not None:
                    tag = tags[src.start_offset + i]
                    if tag is not None:
                        style = theme[tag]
                        if style == theme.reset:
                            style = ""
                body.append(ContentFragment(shown, width, style))
            result.append(ContentLine(src, prompt, tuple(body)))
        return result

    @staticmethod
    def render_row(row) -> str:
        out = [row.prompt_text]
        active = ""
        for frag in row.fragments:
            if frag.style != active:
                if active:
                    out.append("\x1b[0m")
                if frag.style:
                    out.append(frag.style)
                active = frag.style
            out.append(frag.text)
        if active:
            out.append("\x1b[0m")
        out.append(row.suffix)
        return "".join(out)

    def calc_screen(self) -> list[str]:
        """Lay out the buffer for the console's width; sets ``layout`` and
        ``cxy`` and returns the screen lines."""
        width = self.console.width
        result = layout_content_lines(self.content_lines(), width, 0)
        self.layout = result.layout_map
        screen = [self.render_row(row) for row in result.wrapped_rows]
        self.cxy = self.pos2xy()
        if self.msg and width > 0:
            screen.extend(self.msg.split("\n"))
        return screen

    def pos2xy(self) -> tuple[int, int]:
        return self.layout.pos_to_xy(self.pos)

    def setpos_from_xy(self, x: int, y: int) -> None:
        self.pos = self.layout.xy_to_pos(x, y)

    def max_column(self, y: int) -> int:
        return self.layout.max_column(y)

    def max_row(self) -> int:
        return self.layout.max_row()

    def invalidate_full(self) -> None:
        self.dirty = True
        self._rendered = None

    def refresh(self) -> None:
        """Recompute the screen and hand it to the console."""
        self.screen = self.calc_screen()
        rendered = RenderedScreen.from_screen_lines(self.screen, self.cxy)
        self._rendered = rendered
        self.console.refresh(rendered)
        self.dirty = False

    def update_cursor(self) -> None:
        self.cxy = self.pos2xy()
        self.console.move_cursor(*self.cxy)

    # -- vertical motion ---------------------------------------------------------

    def _buffer_rows(self) -> list[int]:
        return [y for y, row in enumerate(self.layout.rows)
                if not row.prompt_only or y == len(self.layout.rows) - 1]

    def move_vertically(self, direction: int) -> bool:
        """Move the cursor one screen row up (-1) or down (+1).  Returns
        False when there is no row in that direction."""
        rows = self._buffer_rows()
        x, y = self.pos2xy()
        if y not in rows:
            return False
        index = rows.index(y) + direction
        if not 0 <= index < len(rows):
            return False
        target = rows[index]
        line_has_text = any(not c.isspace() for c in self.buffer[self.bol():self.eol()])
        at_end = self.pos == self.eol() or x >= self.layout.max_column(y)
        target_end = self.layout.max_column(target)
        if (at_end and line_has_text) or x > target_end:
            x = target_end
        self.setpos_from_xy(x, target)
        return True

    def on_top_row(self) -> None:
        self.pos = 0
        self.error("start of buffer")

    def on_bottom_row(self) -> None:
        self.pos = len(self.buffer)
        self.error("end of buffer")

    # -- the edit loop -------------------------------------------------------------

    def prepare(self) -> None:
        """Get ready for a new line of input."""
        self.console.prepare()
        self.arg = None
        self.arg_sign_only = False
        self.finished = False
        del self.buffer[:]
        self.pos = 0
        self.dirty = True
        self.last_command = None
        self.last_command_instance = None
        self.screen = []
        self.cxy = (0, 0)
        self.layout = LayoutMap.empty()
        self._rendered = None

    def restore(self) -> None:
        self.console.restore()

    def finish(self) -> None:
        """Called once the input has been accepted."""

    def abandon(self) -> None:
        """Called when the input is abandoned (Ctrl-C)."""

    def after_command(self, cmd) -> None:
        pass

    def run_hooks(self) -> None:
        if self.threading_hook is not None:
            try:
                self.threading_hook()
            except Exception:
                pass
        input_hook = self.console.input_hook
        if input_hook:
            try:
                input_hook()
            except Exception:
                pass

    def do_cmd(self, cmd) -> None:
        name, keys = cmd[0], cmd[1]
        if isinstance(name, str):
            command_type = self.commands.get(name, commands.invalid_command)
        elif isinstance(name, type) and issubclass(name, commands.Command):
            command_type = name
        else:
            return
        if self.in_bracketed_paste and command_type not in self.paste_safe_commands():
            # everything typed inside a bracketed paste is text
            command_type = commands.self_insert
            keys = [k if len(k) == 1 else "" for k in keys]
        command = command_type(self, name if isinstance(name, str) else command_type.command_name(), keys)
        command.do()
        self.after_command(command)
        if self.dirty:
            self.refresh()
        else:
            self.update_cursor()
        if command.kills_digit_arg:
            self.arg = None
            self.arg_sign_only = False
        self.last_command = command_type
        self.last_command_instance = command
        if command.finish:
            self.finished = True
            self.console.finish()
            self.finish()

    def paste_safe_commands(self) -> set:
        return {commands.self_insert, commands.disable_bracketed_paste,
                commands.insert_nl}

    def last_command_is(self, cls) -> bool:
        return self.last_command is not None and issubclass(self.last_command, cls)

    def handle1(self, block: bool = True) -> bool:
        """Process one event; returns False if *block* is false and no
        event was ready."""
        if self.msg:
            self.msg = ""
            self.dirty = True
        while True:
            self.run_hooks()
            self.console.wait(100)
            event = self.console.get_event(block=False)
            if not event:
                if block:
                    continue
                return False
            translate = True
            if event.evt == "key":
                self.input_trans.push(event)
            elif event.evt == "scroll":
                self.refresh()
            elif event.evt == "resize":
                try:
                    self.console.height, self.console.width = self.console.getheightwidth()
                except Exception:
                    pass
                self.refresh()
            else:
                translate = False
            if translate:
                cmd = self.input_trans.get()
            else:
                cmd = [event.evt, event.data]
            if cmd is None:
                if block:
                    continue
                return False
            self.do_cmd(cmd)
            return True

    def push_char(self, char: int | bytes) -> None:
        self.console.push_char(char)
        self.handle1(block=False)

    def run_scheduled_commands(self) -> None:
        while self.scheduled_commands:
            name = self.scheduled_commands.pop(0)
            self.do_cmd((name, []))

    def readline(self, startup_hook: Callable | None = None) -> str:
        """Edit one input and return it."""
        self.prepare()
        try:
            if startup_hook is not None:
                startup_hook()
            self.run_scheduled_commands()
            self.refresh()
            while not self.finished:
                self.handle1()
            return self.get_unicode()
        finally:
            self.restore()

    def bind(self, spec: str, command: str) -> None:
        self.keymap = self.keymap + ((spec, command),)
        self.input_trans = input_mod.KeymapTranslator(
            self.collect_keymap(), invalid_cls="invalid-key",
            character_cls="self-insert")
