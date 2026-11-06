"""The search state shared by the Find, Replace and Find in Files dialogs,
and the search functions they use."""
import re

from tkinter import StringVar, BooleanVar, TclError
from tkinter import messagebox


def get(root):
    """Return the SearchEngine of *root* (created on first use)."""
    if not hasattr(root, '_searchengine'):
        root._searchengine = SearchEngine(root)
    return root._searchengine


class SearchEngine:
    """The pattern and options of a search, held in Tk variables so that
    all dialogs show the same settings."""

    def __init__(self, root):
        """*root* is used as the parent of error message boxes."""
        self.root = root
        self.patvar = StringVar(root, '')
        self.revar = BooleanVar(root, False)    # regular expression?
        self.casevar = BooleanVar(root, False)  # match case?
        self.wordvar = BooleanVar(root, False)  # whole words?
        self.wrapvar = BooleanVar(root, True)   # wrap around?
        self.backvar = BooleanVar(root, False)  # search backwards?
        # A dialog that is open sets this to show errors itself:
        # error_handler(message, position or None).
        self.error_handler = None

    def getpat(self):
        return self.patvar.get()

    def setpat(self, pat):
        self.patvar.set(pat)

    def isre(self):
        return self.revar.get()

    def iscase(self):
        return self.casevar.get()

    def isword(self):
        return self.wordvar.get()

    def iswrap(self):
        return self.wrapvar.get()

    def isback(self):
        return self.backvar.get()

    def setcookedpat(self, pat):
        """Set the pattern to search for the literal text *pat*."""
        if self.isre():
            pat = re.escape(pat)
        self.setpat(pat)

    def getcookedpat(self):
        """Return the pattern as a regular expression (escaped when not in
        regex mode, wrapped for whole-word search)."""
        pat = self.getpat()
        if not self.isre():
            pat = re.escape(pat)
        if self.isword():
            pat = r'\b(?:%s)\b' % pat
        return pat

    def getprog(self):
        """Return the compiled pattern, or None after reporting an error."""
        pat = self.getpat()
        if not pat:
            self.report_error(pat, "Empty regular expression")
            return None
        flags = 0 if self.iscase() else re.IGNORECASE
        if self.isre():
            # Check the pattern as typed, so that error positions refer
            # to what the user sees.
            try:
                re.compile(pat, flags)
            except re.error as e:
                self.report_error(pat, e.msg, e.pos)
                return None
        try:
            return re.compile(self.getcookedpat(), flags)
        except re.error as e:
            if self.isword():
                self.report_error(
                    pat, 'global flags like (?i) cannot be used with the '
                         '"Whole word" option')
            else:
                self.report_error(pat, e.msg, e.pos)
            return None

    def report_error(self, pat, msg, col=None):
        """Show a regular expression error, in the open dialog if there
        is one, else in a message box."""
        msg = "Error: " + str(msg)
        if self.error_handler is not None:
            if col is not None:
                msg += " at position %d" % col
            self.error_handler(msg, col)
            return
        if pat:
            msg += "\nPattern: " + str(pat)
        if col is not None:
            msg += "\nOffset: " + str(col)
        messagebox.showerror("Regular expression error", msg,
                             master=self.root)

    def search_text(self, text, prog=None, ok=False):
        """Search *text* for the pattern (or *prog*).

        The search starts at the selection end (forward) or start
        (backward), or at the cursor; with *ok* a match at the start
        position is accepted.  Return (line number, match) or None.
        """
        if not prog:
            prog = self.getprog()
            if not prog:
                return None
        wrap = self.wrapvar.get()
        first, last = get_selection(text)
        if self.isback():
            start = last if ok else first
            line, col = get_line_col(start)
            return self.search_backward(text, prog, line, col, wrap, ok)
        start = first if ok else last
        line, col = get_line_col(start)
        return self.search_forward(text, prog, line, col, wrap, ok)

    def search_forward(self, text, prog, line, col, wrap, ok=False):
        """Find the first match at or after (line, col).

        An empty match at the start position counts only with *ok*.
        Return (line, match) or None.
        """
        wrapped = False
        startline = line
        chars = text.get("%d.0" % line, "%d.0" % (line + 1))
        while chars:
            m = prog.search(chars[:-1], col)
            if m and (ok or m.end() > col):
                return line, m
            line += 1
            if wrapped and line > startline:
                break
            col = 0
            ok = True
            chars = text.get("%d.0" % line, "%d.0" % (line + 1))
            if not chars and wrap:
                wrapped = True
                wrap = False
                line = 1
                chars = text.get("1.0", "2.0")
        return None

    def search_backward(self, text, prog, line, col, wrap, ok=False):
        """Find the last match before (line, col).

        Return (line, match) or None.
        """
        wrapped = False
        startline = line
        chars = text.get("%d.0" % line, "%d.0" % (line + 1))
        while True:
            m = search_reverse(prog, chars[:-1], col)
            if m and (ok or m.start() < col):
                return line, m
            line -= 1
            if wrapped and line < startline:
                break
            ok = True
            if line <= 0:
                if not wrap:
                    break
                wrapped = True
                wrap = False
                last = text.index("end-1c")
                line, col = map(int, last.split("."))
            chars = text.get("%d.0" % line, "%d.0" % (line + 1))
            col = len(chars) - 1
        return None


def search_reverse(prog, chars, col):
    """Return the last match of *prog* in *chars* that ends at or before
    *col* and starts before it, or None.

    (Regular expressions cannot search backwards, so this scans forward.)
    """
    m = prog.search(chars)
    found = None
    while m:
        start, end = m.span()
        if not (start < col and end <= col):
            break
        found = m
        if start == end:
            end += 1  # step over an empty match
        m = prog.search(chars, end)
    return found


def get_selection(text):
    """Return the (first, last) indices of the selection, or of the
    cursor when nothing is selected."""
    try:
        first = text.index("sel.first")
        last = text.index("sel.last")
    except TclError:
        first = last = None
    if not first:
        first = text.index("insert")
    if not last:
        last = first
    return first, last


def get_line_col(index):
    """Return (line, column) of a 'line.column' index."""
    line, col = map(int, index.split("."))
    return line, col


if __name__ == "__main__":
    from unittest import main
    main('idlelib.idle_test.test_searchengine', verbosity=2)
