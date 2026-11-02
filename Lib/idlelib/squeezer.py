"""Fold long output in the shell into a button.

Output written to stdout with at least auto-squeeze-min-lines lines
(counting wrapped lines) is not inserted; an ExpandingButton stands in
its place.  The button can expand the text again, copy it or show it in
a separate window.  Output already in the shell can be squeezed from the
context menu.
"""
import re
import tkinter as tk
from tkinter import messagebox

from idlelib.config import idleConf
from idlelib.textview import view_text
from idlelib.tooltip import Hovertip
from idlelib import macosx


def count_lines_with_wrapping(s, linewidth=80):
    """Return the number of screen lines *s* needs in a window
    *linewidth* characters wide (tab stops every 8 columns).

    A final newline does not start another line.
    """
    tabwidth = 8
    lines = s.split('\n')
    if lines[-1] == '':
        lines.pop()  # nothing after the last newline
    total = 0
    for line in lines:
        if '\t' in line:
            col = 0
            for part in line.split('\t')[:-1]:
                col += len(part)
                col += tabwidth - col % tabwidth
            col += len(line.rsplit('\t', 1)[1])
        else:
            col = len(line)
        total += max(1, -(-col // linewidth))
    return total


class ExpandingButton(tk.Button):
    """A button standing for squeezed output in the text.

    Double-click inserts the text again in place of the button; the
    context menu copies the text or shows it in a viewer.
    """

    def __init__(self, s, tags, numoflines, squeezer):
        self.s = s
        self.tags = tags
        self.numoflines = numoflines
        self.squeezer = squeezer
        self.editwin = editwin = squeezer.editwin
        self.text = text = editwin.text
        # Insert below the filters (the colorizer, undo) of the shell.
        self.base_text = editwin.per.bottom

        label = f"Squeezed text ({numoflines} lines)."
        tk.Button.__init__(self, text, text=label,
                           background="#FFFFC0", activebackground="#FFFFE0")
        Hovertip(self, "Double-click to expand, right-click for more options.",
                 hover_delay=80)
        self.bind("<Double-Button-1>", self.expand)
        if macosx.isAquaTk():
            self.bind("<Button-2>", self.context_menu_event)
        else:
            self.bind("<Button-3>", self.context_menu_event)

        # Whether expanding could make Tk very slow; computed when the
        # widget has its size.
        self.is_dangerous = None
        self.after_idle(self.set_is_dangerous)

    def set_is_dangerous(self):
        """Flag text with very many lines or very long lines."""
        max_line_len = 50 * self.text.winfo_width()
        self.is_dangerous = (
            self.numoflines > 1000 or
            len(self.s) > 50000 or
            any(len(m.group(0)) >= max_line_len
                for m in re.finditer(r'[^\n]+', self.s)))

    def expand(self, event=None):
        """Replace the button by its text (asking first if it is huge)."""
        if self.is_dangerous is None:
            self.set_is_dangerous()
        if self.is_dangerous:
            confirm = messagebox.askokcancel(
                title="Expand huge output?",
                message=(f"The squeezed output is very long: {len(self.s)} "
                         f"characters in {self.numoflines} lines.  "
                         "Expanding it may make IDLE slow or unresponsive.  "
                         "Viewing or copying it are safer.\n\nExpand anyway?"),
                default=messagebox.CANCEL,
                parent=self.text)
            if not confirm:
                return "break"
        index = self.text.index(self)
        self.base_text.insert(index, self.s, self.tags)
        self.base_text.delete(self)
        self.editwin.on_squeezed_expand(index, self.s, self.tags)
        self.squeezer.expandingbuttons.remove(self)

    def copy(self, event=None):
        """Put the text on the clipboard."""
        self.clipboard_clear()
        self.clipboard_append(self.s)

    def view(self, event=None):
        """Show the text in a separate (non-modal) window."""
        view_text(self.text, "Squeezed Output Viewer", self.s,
                  modal=False, wrap='none')

    rmenu_specs = (
        ('copy', 'copy'),
        ('view', 'view'),
    )

    def context_menu_event(self, event):
        self.text.mark_set("insert", "@%d,%d" % (event.x, event.y))
        rmenu = tk.Menu(self.text, tearoff=0)
        for label, method_name in self.rmenu_specs:
            rmenu.add_command(label=label, command=getattr(self, method_name))
        rmenu.tk_popup(event.x_root, event.y_root)
        return "break"


class Squeezer:
    """Shell extension squeezing long output (see the module doc)."""

    @classmethod
    def reload(cls):
        cls.auto_squeeze_min_lines = idleConf.GetOption(
            "main", "PyShell", "auto-squeeze-min-lines",
            type="int", default=50)

    def __init__(self, editwin):
        self.editwin = editwin
        self.text = editwin.text
        self.base_text = editwin.per.bottom
        # The buttons in the text, in text order.
        self.expandingbuttons = []

        # Intercept the shell's output.
        original_write = editwin.write

        def write(s, tags=(), write=original_write):
            if tags != "stdout":
                return write(s, tags)
            numoflines = self.count_lines(s)
            if numoflines < self.auto_squeeze_min_lines:
                return write(s, tags)
            button = ExpandingButton(s, tags, numoflines, self)
            # Keep iomark after the button.
            text = self.text
            text.mark_gravity("iomark", tk.RIGHT)
            text.window_create("iomark", window=button, padx=3, pady=5)
            text.see("iomark")
            text.update()
            text.mark_gravity("iomark", tk.LEFT)
            self.expandingbuttons.append(button)
            return None

        editwin.write = write

    def count_lines(self, s):
        """Screen lines of *s* in the editor window's width."""
        return count_lines_with_wrapping(s, self.editwin.width)

    def squeeze_current_text(self, event=None):
        """Squeeze the output block (stdout or stderr) at the cursor."""
        tag_name = next((tag for tag in ("stdout", "stderr")
                         if tag in self.text.tag_names("insert")), None)
        if tag_name is None:
            self.text.bell()
            return "break"
        start, end = self.text.tag_prevrange(tag_name, "insert +1c")
        s = self.text.get(start, end)
        # Keep the final newline outside the button.
        if s.endswith('\n'):
            s = s[:-1]
            end = self.text.index(f"{end} -1c")
        self.base_text.delete(start, end)
        numoflines = self.count_lines(s)
        button = ExpandingButton(s, tag_name, numoflines, self)
        self.text.window_create(start, window=button, padx=3, pady=5)
        # Keep the list in text order.
        i = len(self.expandingbuttons)
        while i > 0 and self.text.compare(self.expandingbuttons[i-1],
                                          ">", button):
            i -= 1
        self.expandingbuttons.insert(i, button)
        return "break"


Squeezer.reload()


if __name__ == "__main__":
    from unittest import main
    main('idlelib.idle_test.test_squeezer', verbosity=2, exit=False)
