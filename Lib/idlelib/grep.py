"""Find in Files: search the files matching a glob pattern for a regular
expression and list the hits in an output window, where right-clicking (or
double-clicking) a hit opens the file at that line.
"""
import fnmatch
import os
import sys

from tkinter import StringVar, BooleanVar
from tkinter.ttk import Checkbutton

from idlelib.searchbase import SearchDialogBase
from idlelib import searchengine


def grep(text, io=None, flist=None):
    """Open the Find in Files dialog for an editor (or the shell).

    The selection is the initial pattern; the file pattern defaults to the
    files with the same extension in the directory of *io*'s file.
    """
    root = text._root()
    engine = searchengine.get(root)
    if not hasattr(engine, "_grepdialog"):
        engine._grepdialog = GrepDialog(root, engine, flist)
    dialog = engine._grepdialog
    searchphrase = text.get("sel.first", "sel.last")
    dialog.open(text, searchphrase, io)


def default_glob(path):
    """Return the default file pattern for the file *path*: the files in
    its directory with its extension (*.py when it has none, and the
    current directory for an unsaved window)."""
    if not path:
        return os.path.join(os.getcwd(), '*.py')
    folder, base = os.path.split(path)
    ext = os.path.splitext(base)[1] or '.py'
    return os.path.join(folder, '*' + ext)


def walk_error(msg):
    """Report a directory that cannot be listed."""
    print(msg)


def findfiles(folder, pattern, recursive):
    """Yield the paths of the files in *folder* (and its subdirectories if
    *recursive*) whose names match the glob *pattern*."""
    for dirpath, dirnames, filenames in os.walk(folder, onerror=walk_error):
        for name in filenames:
            if fnmatch.fnmatch(name, pattern):
                yield os.path.join(dirpath, name)
        if not recursive:
            break


class GrepDialog(SearchDialogBase):
    """Dialog for searching several files."""

    title = "Find in Files Dialog"
    icon = "Grep"
    needwrapbutton = 0

    def __init__(self, root, engine, flist):
        """*flist* is used to open the output window."""
        super().__init__(root, engine)
        self.flist = flist
        self.globvar = StringVar(root)
        self.recvar = BooleanVar(root)

    def open(self, text, searchphrase, io=None):
        """Show the dialog; *io* gives the file of the window."""
        SearchDialogBase.open(self, text, searchphrase)
        path = io.filename if io else ''
        self.globvar.set(default_glob(path or ''))

    def create_entries(self):
        SearchDialogBase.create_entries(self)
        self.globent = self.make_entry("In files:", self.globvar)[0]

    def create_other_buttons(self):
        btn = Checkbutton(self.make_frame()[0], variable=self.recvar,
                          text="Recurse down subdirectories")
        btn.pack(side="top", fill="both")

    def create_command_buttons(self):
        SearchDialogBase.create_command_buttons(self)
        self.make_button("Search Files", self.default_command, isdef=True)

    def default_command(self, event=None):
        """Search with the current settings, writing to an output window."""
        prog = self.engine.getprog()
        if not prog:
            return
        path = self.globvar.get()
        if not path:
            self.top.bell()
            return
        from idlelib import outwin
        saved = sys.stdout
        try:
            sys.stdout = outwin.OutputWindow(self.flist)
            self.grep_it(prog, path)
        finally:
            sys.stdout = saved

    def grep_it(self, prog, path):
        """Write the lines of the files matching *path* that match *prog*
        to sys.stdout as 'file: line: text'."""
        folder, filepat = os.path.split(path)
        if not folder:
            folder = os.curdir
        files = sorted(findfiles(folder, filepat, self.recvar.get()))
        self.close()
        pat = self.engine.getpat()
        print(f"Searching {pat!r} in {path} ...")
        hits = 0
        try:
            for fn in files:
                try:
                    with open(fn, errors='replace') as f:
                        for lineno, line in enumerate(f, 1):
                            if line[-1:] == '\n':
                                line = line[:-1]
                            if prog.search(line):
                                sys.stdout.write(f"{fn}: {lineno}: {line}\n")
                                hits += 1
                except OSError as msg:
                    print(msg)
            if hits:
                print(f"Hits found: {hits}\n"
                      "(Hint: right-click to open locations.)")
            else:
                print("No hits.")
        except AttributeError:
            # the output window was closed while the search was running
            pass


def _grep_dialog(parent):  # htest #
    from tkinter import Toplevel, Text, SEL
    from tkinter.ttk import Frame, Button
    from idlelib.pyshell import PyShellFileList

    top = Toplevel(parent)
    top.title("Test GrepDialog")
    x, y = map(int, parent.geometry().split('+')[1:])
    top.geometry(f"+{x}+{y + 175}")

    flist = PyShellFileList(top)
    frame = Frame(top)
    frame.pack()
    text = Text(frame, height=5)
    text.pack()
    text.insert('1.0', 'import grep')

    def show_grep_dialog():
        text.tag_add(SEL, "1.0", '1.end')
        grep(text, flist=flist)
        text.tag_remove(SEL, "1.0", '1.end')

    button = Button(frame, text="Show GrepDialog", command=show_grep_dialog)
    button.pack()


if __name__ == "__main__":
    from unittest import main
    main('idlelib.idle_test.test_grep', verbosity=2, exit=False)
