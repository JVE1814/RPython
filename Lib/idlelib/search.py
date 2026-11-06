"""The Find dialog and the Find Again / Find Selection commands."""
from tkinter import TclError

from idlelib import searchengine
from idlelib.searchbase import SearchDialogBase


def _setup(text):
    """Return the SearchDialog of the text's root (created on first use)."""
    root = text._root()
    engine = searchengine.get(root)
    if not hasattr(engine, "_searchdialog"):
        engine._searchdialog = SearchDialog(root, engine)
    return engine._searchdialog


def find(text):
    """Open the Find dialog, with the selection as the initial pattern."""
    pat = text.get("sel.first", "sel.last")
    return _setup(text).open(text, pat)


def find_again(text):
    """Repeat the last search (opening the dialog if there was none)."""
    return _setup(text).find_again(text)


def find_selection(text):
    """Search for the next occurrence of the selected text."""
    return _setup(text).find_selection(text)


class SearchDialog(SearchDialogBase):
    """The Find dialog."""

    def create_widgets(self):
        SearchDialogBase.create_widgets(self)
        self.make_button("Find Next", self.default_command, isdef=True)

    def default_command(self, event=None):
        """Find the next match (the dialog stays open)."""
        if not self.engine.getprog():
            return
        self.find_again(self.text)

    def find_again(self, text):
        """Select the next match in *text*; return True if found."""
        if not self.engine.getpat():
            self.open(text)
            return False
        if not self.engine.getprog():
            return False
        res = self.engine.search_text(text)
        if not res:
            self.bell()
            return False
        line, m = res
        i, j = m.span()
        first = "%d.%d" % (line, i)
        last = "%d.%d" % (line, j)
        try:
            selfirst = text.index("sel.first")
            sellast = text.index("sel.last")
            if selfirst == first and sellast == last:
                # the only match is the current selection
                self.bell()
                return False
        except TclError:
            pass
        text.tag_remove("sel", "1.0", "end")
        text.tag_add("sel", first, last)
        text.mark_set("insert", self.engine.isback() and first or last)
        text.see("insert")
        return True

    def find_selection(self, text):
        """Search for the selected text (literally)."""
        pat = text.get("sel.first", "sel.last")
        if pat:
            self.engine.setcookedpat(pat)
        return self.find_again(text)


def _search_dialog(parent):  # htest #
    "Display search test box."
    from tkinter import Toplevel, Text
    from tkinter.ttk import Frame, Button

    top = Toplevel(parent)
    top.title("Test SearchDialog")
    x, y = map(int, parent.geometry().split('+')[1:])
    top.geometry("+%d+%d" % (x, y + 175))

    frame = Frame(top)
    frame.pack()
    text = Text(frame, inactiveselectbackground='gray')
    text.pack()
    text.insert("insert", "This is a sample string.\n" * 5)

    def show_find():
        text.tag_add('sel', '1.0', 'end')
        _setup(text).open(text)
        text.tag_remove('sel', '1.0', 'end')

    button = Button(frame, text="Search (selection ignored)", command=show_find)
    button.pack()


if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_search', verbosity=2, exit=False)
