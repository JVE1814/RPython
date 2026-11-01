"""A reader with a history of earlier inputs.

Up/down on the first/last row walk the history; edits made to a recalled
entry are kept only while the current input is being edited (they never
change the stored history).  Ctrl-R/Ctrl-S search incrementally, page
up/down search for entries starting with the text before the cursor.
"""

from __future__ import annotations

from . import commands, input as input_mod
from .reader import Reader

isearch_keymap: tuple[tuple[str, str], ...] = (
    (r"\C-r", "isearch-backwards"),
    (r"\C-s", "isearch-forwards"),
    (r"\C-c", "isearch-cancel"),
    (r"\C-g", "isearch-cancel"),
    (r"\<backspace>", "isearch-backspace"),
    (r"\C-h", "isearch-backspace"),
)

ISEARCH_BACKWARDS = "backwards"
ISEARCH_FORWARDS = "forwards"


class next_history(commands.Command):
    def do(self) -> None:
        r = self.reader
        if r.historyi >= len(r.history):
            r.error("end of history list")
            return
        r.select_item(r.historyi + 1)


class previous_history(commands.Command):
    def do(self) -> None:
        r = self.reader
        if r.historyi == 0:
            r.error("start of history list")
            return
        r.select_item(r.historyi - 1)


class restore_history(commands.Command):
    def do(self) -> None:
        r = self.reader
        if r.historyi != len(r.history):
            if r.get_unicode() != r.history[r.historyi]:
                r.buffer = list(r.history[r.historyi])
                r.pos = len(r.buffer)
                r.dirty = True


class first_history(commands.Command):
    def do(self) -> None:
        self.reader.select_item(0)


class last_history(commands.Command):
    def do(self) -> None:
        self.reader.select_item(len(self.reader.history))


class operate_and_get_next(commands.Command):
    finish = True

    def do(self) -> None:
        self.reader.next_history = self.reader.historyi + 1


class yank_arg(commands.Command):
    def do(self) -> None:
        r = self.reader
        if r.last_command is self.__class__:
            r.yank_arg_i += 1
        else:
            r.yank_arg_i = 0
        if r.historyi < r.yank_arg_i:
            r.error("beginning of history list")
            return
        words = r.get_item(r.historyi - r.yank_arg_i - 1).split()
        if not words:
            r.error("no arguments")
            return
        word = words[-1]
        if r.last_command is self.__class__ and r.yank_arg_yanked:
            del r.buffer[r.pos - len(r.yank_arg_yanked):r.pos]
            r.pos -= len(r.yank_arg_yanked)
        r.insert(word)
        r.yank_arg_yanked = word


class _history_search(commands.Command):
    direction = -1

    def do(self) -> None:
        r = self.reader
        prefix = r.get_unicode()[:r.pos]
        index = r.historyi + self.direction
        while 0 <= index < len(r.history):
            if r.get_item(index).startswith(prefix):
                pos = r.pos
                r.select_item(index)
                r.pos = pos
                return
            index += self.direction
        r.error("not found")


class history_search_backward(_history_search):
    direction = -1


class history_search_forward(_history_search):
    direction = 1


class reverse_history_isearch(commands.Command):
    def do(self) -> None:
        r = self.reader
        r.isearch_start = r.historyi, r.pos
        r.isearch_term = ""
        r.isearch_direction = ISEARCH_BACKWARDS
        r.dirty = True
        r.push_input_trans(r.isearch_trans)


class forward_history_isearch(commands.Command):
    def do(self) -> None:
        r = self.reader
        r.isearch_start = r.historyi, r.pos
        r.isearch_term = ""
        r.isearch_direction = ISEARCH_FORWARDS
        r.dirty = True
        r.push_input_trans(r.isearch_trans)


class isearch_cancel(commands.Command):
    def do(self) -> None:
        r = self.reader
        r.isearch_direction = ""
        r.pop_input_trans()
        r.select_item(r.isearch_start[0])
        r.pos = r.isearch_start[1]
        r.dirty = True


class isearch_add_character(commands.Command):
    def do(self) -> None:
        r = self.reader
        r.isearch_term += self.event[-1]
        r.dirty = True
        term = r.isearch_term
        here = r.get_unicode()
        if here[r.pos:r.pos + len(term)] != term:
            r.isearch_next()


class isearch_backspace(commands.Command):
    def do(self) -> None:
        r = self.reader
        if r.isearch_term:
            r.isearch_term = r.isearch_term[:-1]
            r.dirty = True
        else:
            r.error("nothing to rubout")


class isearch_forwards(commands.Command):
    def do(self) -> None:
        r = self.reader
        r.isearch_direction = ISEARCH_FORWARDS
        r.isearch_next()


class isearch_backwards(commands.Command):
    def do(self) -> None:
        r = self.reader
        r.isearch_direction = ISEARCH_BACKWARDS
        r.isearch_next()


class isearch_end(commands.Command):
    def do(self) -> None:
        r = self.reader
        r.isearch_direction = ""
        r.console.forgetinput()
        r.pop_input_trans()
        r.dirty = True


_HISTORY_COMMANDS = [
    next_history, previous_history, restore_history, first_history,
    last_history, yank_arg, history_search_backward, history_search_forward,
    reverse_history_isearch, forward_history_isearch, isearch_cancel,
    isearch_add_character, isearch_backspace, isearch_forwards,
    isearch_backwards, isearch_end, operate_and_get_next,
]


class HistoricalReader(Reader):
    """Reader with a list of previous inputs."""

    def __init__(self, console) -> None:
        super().__init__(console)
        self.history: list[str] = []
        self.historyi = 0
        self.should_auto_add_history = True
        self.transient_history: dict[int, str] = {}
        self.next_history: int | None = None
        self.isearch_direction = ""
        self.isearch_term = ""
        self.isearch_start = (0, 0)
        self.yank_arg_i = 0
        self.yank_arg_yanked = ""
        self.isearch_trans = input_mod.KeymapTranslator(
            isearch_keymap, invalid_cls="isearch-end",
            character_cls="isearch-add-character")

    def collect_keymap(self):
        return super().collect_keymap() + (
            (r"\C-n", "next-history"),
            (r"\C-p", "previous-history"),
            (r"\C-o", "operate-and-get-next"),
            (r"\C-r", "reverse-history-isearch"),
            (r"\C-s", "forward-history-isearch"),
            (r"\M-r", "restore-history"),
            (r"\M-.", "yank-arg"),
            (r"\<page down>", "history-search-forward"),
            (r"\<page up>", "history-search-backward"),
        )

    def collect_commands(self):
        table = super().collect_commands()
        for cls in _HISTORY_COMMANDS:
            table[cls.command_name()] = cls
        return table

    def get_item(self, i: int) -> str:
        if i != len(self.history):
            return self.transient_history.get(i, self.history[i])
        return self.transient_history.get(i, self.get_unicode())

    def select_item(self, i: int) -> None:
        self.transient_history[self.historyi] = self.get_unicode()
        buf = self.transient_history.get(i)
        if buf is None:
            buf = self.history[i] if i < len(self.history) else ""
        self.buffer = list(buf)
        self.historyi = i
        self.pos = len(self.buffer)
        self.dirty = True
        self.last_refresh_cache_invalidated = True

    def on_top_row(self) -> None:
        if self.historyi > 0:
            self.select_item(self.historyi - 1)
        else:
            super().on_top_row()

    def on_bottom_row(self) -> None:
        if self.historyi < len(self.history):
            self.select_item(self.historyi + 1)
        else:
            super().on_bottom_row()

    def get_prompt(self, lineno: int, cursor_on_line: bool) -> str:
        if cursor_on_line and self.isearch_direction:
            direction = "r" if self.isearch_direction == ISEARCH_BACKWARDS else "f"
            return f"({direction}-search `{self.isearch_term}') "
        return super().get_prompt(lineno, cursor_on_line)

    def isearch_next(self) -> None:
        term = self.isearch_term
        backwards = self.isearch_direction == ISEARCH_BACKWARDS
        index = self.historyi
        pos = self.pos
        text = self.get_unicode()
        if not term:
            self.error("nothing to search for")
            return
        while True:
            if backwards:
                found = text.rfind(term, 0, max(pos - 1 + len(term), 0))
            else:
                found = text.find(term, pos + 1)
            if found >= 0:
                if index != self.historyi:
                    self.select_item(index)
                self.pos = found
                self.dirty = True
                return
            if backwards:
                if index == 0:
                    self.error("not found")
                    return
                index -= 1
                text = self.get_item(index)
                pos = len(text) + 1
            else:
                if index >= len(self.history):
                    self.error("not found")
                    return
                index += 1
                text = self.get_item(index)
                pos = -1

    def prepare(self) -> None:
        super().prepare()
        try:
            self.transient_history = {}
            if self.next_history is not None and self.next_history < len(self.history):
                self.historyi = self.next_history
                self.buffer[:] = list(self.history[self.next_history])
                self.pos = len(self.buffer)
                self.transient_history[len(self.history)] = ""
            else:
                self.historyi = len(self.history)
            self.next_history = None
        except BaseException:
            self.restore()
            raise

    def finish(self) -> None:
        super().finish()
        ret = self.get_unicode()
        self.transient_history = {}
        self.isearch_direction = ""
        entry = ret.rstrip()
        if entry and self.should_auto_add_history:
            self.history.append(entry)

    def abandon(self) -> None:
        super().abandon()
        self.isearch_direction = ""
        self.transient_history = {}
