"""Simple text-entry widget for curses windows.

``Textbox(win).edit()`` lets the user type into *win* with Emacs-like key
bindings until Ctrl-G and returns the text.  ``rectangle()`` draws a box.

Key bindings:
  Ctrl-A  start of line            Ctrl-E  end of line
  Ctrl-B  left (wraps to previous line)   Ctrl-F  right (wraps)
  Ctrl-D  delete character         Ctrl-H / Backspace  delete backwards
  Ctrl-G  finish editing           Ctrl-J  next line (finish in a 1-line box)
  Ctrl-K  kill to end of line (or the empty line)
  Ctrl-L  redraw                   Ctrl-N / Down, Ctrl-P / Up  line motion
  Ctrl-O  open a new line
"""

import curses
import curses.ascii


def rectangle(win, uly, ulx, lry, lrx):
    """Draw a box with corners (uly, ulx) and (lry, lrx) using line graphics."""
    win.vline(uly + 1, ulx, curses.ACS_VLINE, lry - uly - 1)
    win.vline(uly + 1, lrx, curses.ACS_VLINE, lry - uly - 1)
    win.hline(uly, ulx + 1, curses.ACS_HLINE, lrx - ulx - 1)
    win.hline(lry, ulx + 1, curses.ACS_HLINE, lrx - ulx - 1)
    win.addch(uly, ulx, curses.ACS_ULCORNER)
    win.addch(uly, lrx, curses.ACS_URCORNER)
    win.addch(lry, ulx, curses.ACS_LLCORNER)
    # The lower right corner may be the window's last cell, where addch()
    # cannot advance the cursor; insch() writes it without moving.
    try:
        win.addch(lry, lrx, curses.ACS_LRCORNER)
    except curses.error:
        win.insch(lry, lrx, curses.ACS_LRCORNER)


def _printable(ch):
    # ASCII graphic characters plus the upper half of an 8-bit code page.
    return 32 <= ch <= 126 or 160 <= ch <= 255


class Textbox:
    """Editing state for one window.

    stripspaces (default on): trailing blanks of a line are not part of the
    text, motion snaps to the end of the text on a line, and gather() keeps a
    single trailing blank per line.
    """

    def __init__(self, win, insert_mode=False):
        self.win = win
        self.insert_mode = insert_mode
        self._update_max_yx()
        self.stripspaces = 1
        self.lastcmd = None
        win.keypad(1)

    def _update_max_yx(self):
        rows, cols = self.win.getmaxyx()
        self.maxy = rows - 1
        self.maxx = cols - 1

    # -- reading the window

    def _cell_is_blank(self, y, x):
        return self.win.instr(y, x, 1) in (b"", b" ")

    def _end_of_line(self, y):
        """Column just after the last non-blank cell of line y (at most maxx)."""
        self._update_max_yx()
        x = self.maxx
        while x >= 0:
            if not self._cell_is_blank(y, x):
                return min(self.maxx, x + 1)
            x -= 1
        return 0

    def _cell_text(self, y, x):
        data = self.win.instr(y, x, 1)
        return data.decode(self.win.encoding, "replace") if data else " "

    # -- editing

    def _insert_printable_char(self, ch):
        self._update_max_yx()
        y, x = self.win.getyx()
        if y > self.maxy or x > self.maxx:
            return
        last_cell = (y == self.maxy and x == self.maxx)
        if self.insert_mode or last_cell:
            # insch() shifts the rest of the line right and never scrolls.
            self.win.insch(ch)
            if self.insert_mode and not last_cell:
                if x < self.maxx:
                    self.win.move(y, x + 1)
                else:
                    self.win.move(y + 1, 0)
        else:
            self.win.addch(ch)

    def _left(self, y, x):
        if x > 0:
            self.win.move(y, x - 1)
        elif y > 0:
            if self.stripspaces:
                self.win.move(y - 1, self._end_of_line(y - 1))
            else:
                self.win.move(y - 1, self.maxx)

    def _vertical(self, y, x, dy):
        ny = y + dy
        self.win.move(ny, x)
        end = self._end_of_line(ny)
        if x > end:
            self.win.move(ny, end)

    def do_command(self, ch):
        """Process one keystroke; return 0 when editing should stop."""
        self._update_max_yx()
        y, x = self.win.getyx()
        self.lastcmd = ch
        if isinstance(ch, int) and _printable(ch) or isinstance(ch, str):
            self._insert_printable_char(ch)
        elif ch == curses.ascii.SOH:
            self.win.move(y, 0)
        elif ch in (curses.ascii.STX, curses.KEY_LEFT, curses.ascii.BS,
                    curses.KEY_BACKSPACE, curses.ascii.DEL):
            self._left(y, x)
            if ch in (curses.ascii.BS, curses.KEY_BACKSPACE, curses.ascii.DEL):
                self.win.delch()
        elif ch == curses.ascii.EOT:
            self.win.delch()
        elif ch == curses.ascii.ENQ:
            if self.stripspaces:
                self.win.move(y, self._end_of_line(y))
            else:
                self.win.move(y, self.maxx)
        elif ch in (curses.ascii.ACK, curses.KEY_RIGHT):
            if x < self.maxx:
                self.win.move(y, x + 1)
            elif y < self.maxy:
                self.win.move(y + 1, 0)
        elif ch == curses.ascii.BEL:
            return 0
        elif ch == curses.ascii.NL:
            if self.maxy == 0:
                return 0
            if y < self.maxy:
                self.win.move(y + 1, 0)
        elif ch == curses.ascii.VT:
            if x == 0 and self._end_of_line(y) == 0:
                self.win.deleteln()
            else:
                self.win.move(y, x)
                self.win.clrtoeol()
        elif ch == curses.ascii.FF:
            self.win.refresh()
        elif ch in (curses.ascii.SO, curses.KEY_DOWN):
            if y < self.maxy:
                self._vertical(y, x, 1)
        elif ch == curses.ascii.SI:
            self.win.insertln()
        elif ch in (curses.ascii.DLE, curses.KEY_UP):
            if y > 0:
                self._vertical(y, x, -1)
        return 1

    def gather(self):
        """Return the window contents as a string (see stripspaces)."""
        self._update_max_yx()
        lines = []
        for y in range(self.maxy + 1):
            stop = self._end_of_line(y)
            if stop == 0 and self.stripspaces:
                continue
            last = stop if self.stripspaces else self.maxx
            text = "".join(self._cell_text(y, x) for x in range(last + 1))
            lines.append(text + ("\n" if self.maxy > 0 else ""))
        return "".join(lines)

    def edit(self, validate=None):
        """Read keystrokes until Ctrl-G (or a terminating command); return
        the gathered text.  *validate* may translate each keystroke."""
        while True:
            ch = self.win.getch()
            if validate:
                ch = validate(ch)
            if not ch:
                continue
            if not self.do_command(ch):
                break
            self.win.refresh()
        return self.gather()


if __name__ == "__main__":
    def _demo(stdscr):
        ncols, nlines = 9, 4
        uly, ulx = 15, 20
        stdscr.addstr(uly - 2, ulx, "Use Ctrl-G to end editing.")
        win = curses.newwin(nlines, ncols, uly, ulx)
        rectangle(stdscr, uly - 1, ulx - 1, uly + nlines, ulx + ncols)
        stdscr.refresh()
        return Textbox(win).edit()

    print("Contents of text box:", repr(curses.wrapper(_demo)))
