"""A reader with tab completion.

The first Tab inserts what is common to all candidates (and says so when
the choice is not unique); a second Tab shows a menu below the cursor
line.  While the menu is shown, typing narrows it.  A completer may also
offer an *action* (e.g. "import the module to see its attributes"): its
message is shown when nothing else is left to show, and the next Tab runs
it.
"""

from __future__ import annotations

import re

from . import commands
from .historical_reader import HistoricalReader
from .utils import ANSI_ESCAPE_SEQUENCE, wlen

_ANY_ESCAPE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")


def stripcolor(s: str) -> str:
    """*s* without ANSI escape sequences."""
    return _ANY_ESCAPE.sub("", s)


def prefix(wordlist: list[str], j: int = 0) -> str:
    """Longest common prefix of the words, starting at index *j*."""
    if not wordlist:
        return ""
    first = wordlist[0]
    end = len(first)
    for word in wordlist[1:]:
        limit = min(end, len(word))
        i = j
        while i < limit and word[i] == first[i]:
            i += 1
        end = i
    return first[j:end]


def build_menu(cons, wordlist: list[str], start: int, use_brackets: bool,
               sort_in_column: bool) -> tuple[list[str], int]:
    """Lay *wordlist* out in columns; returns (lines, index of the first
    word not shown, or 0 if everything fit)."""
    pad = 4 if use_brackets else 2
    visible = [stripcolor(w) for w in wordlist]
    width = max(getattr(cons, "width", 80), 10)
    height = max(getattr(cons, "height", 25), 3)
    item_width = min(max(wlen(v) for v in visible), width - pad - 1)
    cols = max(1, (width - 1) // (item_width + pad))
    rows = max(1, height // 3 - 1)
    remaining = len(wordlist) - start
    if sort_in_column:
        rows_needed = (remaining + cols - 1) // cols
        rows = min(rows, rows_needed)
    capacity = rows * cols
    shown = list(range(start, min(start + capacity, len(wordlist))))
    grid: list[list[int]] = [[] for _ in range(rows)]
    for n, index in enumerate(shown):
        r, c = (n % rows, n // rows) if sort_in_column else (n // cols, n % cols)
        if r < rows:
            grid[r].append(index)
    lines = []
    for row in grid:
        if not row:
            continue
        cells = []
        for index in row:
            word, vis = wordlist[index], visible[index]
            if wlen(vis) > item_width:
                vis = vis[:item_width]
                word = vis
            text = f"[ {word} ]" if use_brackets else word
            cells.append(text + " " * (item_width - wlen(vis) + (pad if not use_brackets else 0)))
        lines.append("".join(cells))
    end = start + len(shown)
    return lines, (end if end < len(wordlist) else 0)


class complete(commands.Command):
    def do(self) -> None:
        r = self.reader
        if not "".join(r.buffer[r.bol():r.pos]).strip():
            # Tab at the start of a line indents
            r.insert(" " * (4 - (r.pos - r.bol()) % 4))
            return
        again = r.last_command_is(self.__class__)
        stem = r.get_stem()
        action = r.cmpltn_action if again else None
        if action is not None:
            r.cmpltn_action = None
            message, func = action
            try:
                err = func()
            except Exception as exc:  # the action is user code (an import)
                err = f"[ error: {exc} ]"
            if err:
                r.msg = err
                r.dirty = True
                r.cmpltn_reset()
                return
            matches, action = r.collect_completions(stem)
            r.cmpltn_menu_choices = matches
            self._show(matches, stem, again=False, action=action, after_action=True)
            return
        if not (again and r.assume_immutable_completions and r.cmpltn_menu_choices):
            matches, action = r.collect_completions(stem)
            r.cmpltn_menu_choices = matches
        else:
            matches, action = r.cmpltn_menu_choices, None
        self._show(matches, stem, again, action)

    def _show(self, matches, stem, again, action, after_action=False) -> None:
        r = self.reader
        plain = [stripcolor(m) for m in matches]
        if not matches:
            if action is not None:
                r.offer_action(action)
            else:
                r.error("no matches")
            return
        if len(matches) == 1:
            if again and plain[0] == stem and action is None and not after_action:
                r.msg = "[ sole completion ]"
                r.dirty = True
            r.insert(plain[0][len(stem):])
            if action is not None:
                r.offer_action(action)
            return
        common = prefix(plain, len(stem))
        if common:
            r.insert(common)
        if again or after_action:
            r.show_menu(matches, start=r.cmpltn_menu_end if r.cmpltn_menu_visible else 0)
            if action is not None:
                r.offer_action(action)
        elif not r.cmpltn_menu_visible:
            r.msg = ("[ complete but not unique ]" if stem + common in plain
                     else "[ not unique ]")
            r.dirty = True


class self_insert(commands.self_insert):
    def do(self) -> None:
        super().do()
        r = self.reader
        if r.cmpltn_menu_visible:
            stem = r.get_stem()
            if not stem:
                r.cmpltn_reset()
                return
            narrowed = [w for w in r.cmpltn_menu_choices
                        if stripcolor(w).startswith(stem)]
            if narrowed:
                r.show_menu(narrowed, start=0)
            else:
                r.cmpltn_reset()


class CompletingReader(HistoricalReader):
    """Adds completion to the historical reader."""

    assume_immutable_completions = False
    use_brackets = False
    sort_in_column = False

    def __init__(self, console) -> None:
        super().__init__(console)
        self.cmpltn_menu: list[str] = []
        self.cmpltn_menu_visible = False
        self.cmpltn_message_visible = False
        self.cmpltn_menu_end = 0
        self.cmpltn_menu_choices: list[str] = []
        self.cmpltn_action = None

    def collect_keymap(self):
        return super().collect_keymap() + ((r"\t", "complete"),)

    def collect_commands(self):
        table = super().collect_commands()
        table["complete"] = complete
        table["self-insert"] = self_insert
        table["self_insert"] = self_insert
        return table

    def paste_safe_commands(self) -> set:
        return super().paste_safe_commands() | {self_insert}

    def after_command(self, cmd) -> None:
        super().after_command(cmd)
        if not isinstance(cmd, (complete, self_insert)):
            self.cmpltn_reset()
        elif not isinstance(cmd, complete):
            self.cmpltn_action = None

    def calc_screen(self) -> list[str]:
        screen = super().calc_screen()
        if self.cmpltn_menu_visible and self.cmpltn_menu:
            row = self.cxy[1] + 1
            visible_rows = len(self.layout.rows)
            # put the menu after the last row of the cursor's line
            while row < visible_rows and self.layout.rows[row].prompt_width == 0 \
                    and self.layout.rows[row - 1].suffix_width:
                row += 1
            screen[row:row] = self.cmpltn_menu
        return screen

    def finish(self) -> None:
        super().finish()
        self.cmpltn_reset()

    def abandon(self) -> None:
        super().abandon()
        self.cmpltn_reset()

    def cmpltn_reset(self) -> None:
        if self.cmpltn_menu_visible or self.cmpltn_menu:
            self.dirty = True
        self.cmpltn_menu = []
        self.cmpltn_menu_visible = False
        self.cmpltn_message_visible = False
        self.cmpltn_menu_end = 0
        self.cmpltn_menu_choices = []
        self.cmpltn_action = None

    def show_menu(self, matches: list[str], start: int = 0) -> None:
        self.cmpltn_menu, self.cmpltn_menu_end = build_menu(
            self.console, matches, start, self.use_brackets, self.sort_in_column)
        self.cmpltn_menu_visible = True
        self.cmpltn_message_visible = False
        self.dirty = True

    def offer_action(self, action) -> None:
        message, _func = action
        self.cmpltn_action = action
        self.msg = message
        self.dirty = True

    def get_stem(self) -> str:
        b = self.buffer
        p = self.pos - 1
        while p >= 0 and (b[p].isalnum() or b[p] == "_"):
            p -= 1
        return "".join(b[p + 1:self.pos])

    def get_completions(self, stem: str):
        return [], None

    def collect_completions(self, stem: str):
        result = self.get_completions(stem)
        if isinstance(result, tuple) and len(result) == 2 and not isinstance(result[0], str):
            matches, action = result
        else:
            matches, action = result, None
        return list(matches or []), action
