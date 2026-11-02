"""The Replace dialog: search and replace text in an editor window."""
import re

from tkinter import StringVar, TclError

from idlelib.searchbase import SearchDialogBase
from idlelib import searchengine


def replace(text, insert_tags=None):
    """Open the Replace dialog for *text* (created once per root).

    *insert_tags* are the tags given to inserted replacement text (used
    in the shell)."""
    root = text._root()
    engine = searchengine.get(root)
    if not hasattr(engine, "_replacedialog"):
        engine._replacedialog = ReplaceDialog(root, engine)
    dialog = engine._replacedialog
    searchphrase = text.get("sel.first", "sel.last")
    dialog.open(text, searchphrase, insert_tags=insert_tags)


class ReplaceDialog(SearchDialogBase):
    """Dialog with a replacement entry in addition to the search entry."""

    title = "Replace Dialog"
    icon = "Replace"

    def __init__(self, root, engine):
        SearchDialogBase.__init__(self, root, engine)
        self.replvar = StringVar(root)
        self.insert_tags = None

    def open(self, text, searchphrase=None, insert_tags=None):
        """Show the dialog; the selection becomes the search phrase."""
        SearchDialogBase.open(self, text, searchphrase)
        try:
            first = text.index("sel.first")
        except TclError:
            first = None
        try:
            last = text.index("sel.last")
        except TclError:
            last = None
        first = first or text.index("insert")
        last = last or first
        self.show_hit(first, last)
        self.ok = True
        self.insert_tags = insert_tags

    def create_entries(self):
        SearchDialogBase.create_entries(self)
        self.replent = self.make_entry("Replace with:", self.replvar)[0]

    def create_command_buttons(self):
        SearchDialogBase.create_command_buttons(self)
        self.make_button("Find", self.find_it)
        self.make_button("Replace", self.replace_it)
        self.make_button("Replace+Find", self.default_command, isdef=True)
        self.make_button("Replace All", self.replace_all)

    def find_it(self, event=None):
        """Find the next match."""
        self.do_find(False)

    def replace_it(self, event=None):
        """Replace the current match (finding one first if needed)."""
        if self.do_find(self.ok):
            self.do_replace()

    def default_command(self, event=None):
        """Replace the current match and find the next one."""
        if self.do_find(self.ok):
            if self.do_replace():
                self.do_find(False)

    def _replace_expand(self, m, repl):
        """Return the replacement for match *m* (with group references
        expanded in regular expression mode), or None on error."""
        if self.engine.isre():
            try:
                new = m.expand(repl)
            except re.error:
                self.engine.report_error(repl, 'Invalid Replace Expression')
                new = None
        else:
            new = repl
        return new

    def replace_all(self, event=None):
        """Replace every match in the text (in one undo block)."""
        prog = self.engine.getprog()
        if not prog:
            return
        repl = self.replvar.get()
        text = self.text
        res = self.engine.search_text(text, prog)
        if not res:
            self.bell()
            return
        text.tag_remove("sel", "1.0", "end")
        text.tag_remove("hit", "1.0", "end")
        line = res[0]
        col = res[1].start()
        # With wrap: the whole text.  Without: from the cursor down, or
        # (searching up) from the top down to the cursor.
        backwards_only = self.engine.isback() and not self.engine.iswrap()
        if self.engine.iswrap() or backwards_only:
            line = 1
            col = 0
        if backwards_only:
            text.mark_set("replace_all_stop", "insert")
        ok = True
        first = last = None
        # The matches are replaced from top to bottom, as one undo block.
        text.undo_block_start()
        while True:
            res = self.engine.search_forward(text, prog, line, col,
                                             wrap=False, ok=ok)
            if not res:
                break
            line, m = res
            if backwards_only and text.compare(
                    "%d.%d" % (line, m.start()), ">=", "replace_all_stop"):
                break
            chars = text.get("%d.0" % line, "%d.0" % (line + 1))
            orig = m.group()
            new = self._replace_expand(m, repl)
            if new is None:
                break
            i, j = m.span()
            first = "%d.%d" % (line, i)
            last = "%d.%d" % (line, j)
            if new == orig:
                text.mark_set("insert", last)
            else:
                text.mark_set("insert", first)
                if first != last:
                    text.delete(first, last)
                if new:
                    text.insert(first, new, self.insert_tags)
            col = i + len(new)
            ok = False
        text.undo_block_stop()
        if first and last:
            self.show_hit(first, last)
        self.close()

    def do_find(self, ok=False):
        """Select the next match; return True if there is one."""
        if not self.engine.getprog():
            return False
        text = self.text
        res = self.engine.search_text(text, None, ok)
        if not res:
            self.bell()
            return False
        line, m = res
        i, j = m.span()
        first = "%d.%d" % (line, i)
        last = "%d.%d" % (line, j)
        self.show_hit(first, last)
        self.ok = True
        return True

    def do_replace(self):
        """Replace the selected match; return True on success."""
        prog = self.engine.getprog()
        if not prog:
            return False
        text = self.text
        try:
            first = pos = text.index("sel.first")
            last = text.index("sel.last")
        except TclError:
            pos = None
        if not pos:
            first = last = pos = text.index("insert")
        line, col = searchengine.get_line_col(pos)
        chars = text.get("%d.0" % line, "%d.0" % (line + 1))
        m = prog.match(chars, col)
        if not prog:
            return False
        if m is None:
            return False
        new = self._replace_expand(m, self.replvar.get())
        if new is None:
            return False
        text.mark_set("insert", first)
        text.undo_block_start()
        if m.group():
            text.delete(first, last)
        if new:
            text.insert(first, new, self.insert_tags)
        text.undo_block_stop()
        self.show_hit(first, text.index("insert"))
        self.ok = False
        return True

    def show_hit(self, first, last):
        """Select and show the text from *first* to *last* (also marked
        with the 'hit' tag when nothing would be visible)."""
        text = self.text
        text.mark_set("insert", first)
        text.tag_remove("sel", "1.0", "end")
        text.tag_add("sel", first, last)
        text.tag_remove("hit", "1.0", "end")
        if first == last:
            text.tag_add("hit", first)
        else:
            text.tag_add("hit", first, last)
        text.see("insert")
        text.update_idletasks()

    def close(self, event=None):
        """Hide the dialog and remove the hit highlighting."""
        SearchDialogBase.close(self, event)
        self.text.tag_remove("hit", "1.0", "end")
        self.insert_tags = None


def _replace_dialog(parent):  # htest #
    from tkinter import Toplevel, Text, END, SEL
    from tkinter.ttk import Frame, Button

    top = Toplevel(parent)
    top.title("Test ReplaceDialog")
    x, y = map(int, parent.geometry().split('+')[1:])
    top.geometry("+%d+%d" % (x, y + 175))

    # mock undo delegator methods
    def undo_block_start():
        pass

    def undo_block_stop():
        pass

    frame = Frame(top)
    frame.pack()
    text = Text(frame, inactiveselectbackground='gray')
    text.undo_block_start = undo_block_start
    text.undo_block_stop = undo_block_stop
    text.pack()
    text.insert("insert", "This is a sample sTring\nPlus MORE.")
    text.focus_set()

    def show_replace():
        text.tag_add(SEL, "1.0", END)
        replace(text)
        text.tag_remove(SEL, "1.0", END)

    button = Button(frame, text="Replace", command=show_replace)
    button.pack()


if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_replace', verbosity=2, exit=False)
