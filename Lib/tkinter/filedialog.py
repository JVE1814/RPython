"""File selection dialogs.

FileDialog, LoadFileDialog, SaveFileDialog -- a portable dialog written
with tkinter widgets (run it with go()).

Open, SaveAs, Directory and the ask*() functions -- the platform's native
dialogs (Tk's tk_getOpenFile, tk_getSaveFile, tk_chooseDirectory).  Their
options: defaultextension, filetypes (a sequence of (label, pattern)
pairs), initialdir, initialfile, parent, title, multiple (Open only),
mustexist (Directory only).
"""

__all__ = ["FileDialog", "LoadFileDialog", "SaveFileDialog",
           "Open", "SaveAs", "Directory",
           "askopenfilename", "asksaveasfilename", "askopenfilenames",
           "askopenfile", "askopenfiles", "asksaveasfile", "askdirectory"]

import fnmatch
import os
from tkinter import (
    Frame, LEFT, YES, BOTTOM, Entry, TOP, Button, Tk, X,
    Toplevel, RIGHT, Y, END, Listbox, BOTH, Scrollbar,
)
from tkinter.dialog import Dialog
from tkinter import commondialog
from tkinter.simpledialog import _setup_dialog

# remembered (directory, pattern) per dialog key
dialogstates = {}


class FileDialog:
    """A file selection dialog built from tkinter widgets.

    d = FileDialog(master)
    fname = d.go(dir_or_file, pattern, default, key)

    go() returns the chosen path, or None when cancelled.  With *key*, the
    directory and pattern are remembered for the next go() with that key.
    """

    title = "File Selection Dialog"

    def __init__(self, master, title=None):
        if title is None:
            title = self.title
        self.master = master
        self.directory = None

        self.top = Toplevel(master)
        self.top.title(title)
        self.top.iconname(title)
        _setup_dialog(self.top)

        self.botframe = Frame(self.top)
        self.botframe.pack(side=BOTTOM, fill=X)

        self.selection = Entry(self.top)
        self.selection.pack(side=BOTTOM, fill=X)
        self.selection.bind('<Return>', self.ok_event)

        self.filter = Entry(self.top)
        self.filter.pack(side=TOP, fill=X)
        self.filter.bind('<Return>', self.filter_command)

        self.midframe = Frame(self.top)
        self.midframe.pack(expand=YES, fill=BOTH)

        self.filesbar = Scrollbar(self.midframe)
        self.filesbar.pack(side=RIGHT, fill=Y)
        self.files = Listbox(self.midframe, exportselection=0,
                             yscrollcommand=(self.filesbar, 'set'))
        self.files.pack(side=RIGHT, expand=YES, fill=BOTH)
        tags = self.files.bindtags()
        self.files.bindtags(tags[1:] + tags[:1])
        self.files.bind('<ButtonRelease-1>', self.files_select_event)
        self.files.bind('<Double-ButtonRelease-1>', self.files_double_event)
        self.filesbar.config(command=(self.files, 'yview'))

        self.dirsbar = Scrollbar(self.midframe)
        self.dirsbar.pack(side=LEFT, fill=Y)
        self.dirs = Listbox(self.midframe, exportselection=0,
                            yscrollcommand=(self.dirsbar, 'set'))
        self.dirs.pack(side=LEFT, expand=YES, fill=BOTH)
        self.dirsbar.config(command=(self.dirs, 'yview'))
        tags = self.dirs.bindtags()
        self.dirs.bindtags(tags[1:] + tags[:1])
        self.dirs.bind('<ButtonRelease-1>', self.dirs_select_event)
        self.dirs.bind('<Double-ButtonRelease-1>', self.dirs_double_event)

        self.ok_button = Button(self.botframe, text="OK",
                                command=self.ok_command)
        self.ok_button.pack(side=LEFT)
        self.filter_button = Button(self.botframe, text="Filter",
                                    command=self.filter_command)
        self.filter_button.pack(side=LEFT, expand=YES)
        self.cancel_button = Button(self.botframe, text="Cancel",
                                    command=self.cancel_command)
        self.cancel_button.pack(side=RIGHT)

        self.top.protocol('WM_DELETE_WINDOW', self.cancel_command)
        self.top.bind('<Alt-w>', self.cancel_command)
        self.top.bind('<Alt-W>', self.cancel_command)

    def go(self, dir_or_file=os.curdir, pattern="*", default="", key=None):
        """Run the dialog; return the selected path or None."""
        if key and key in dialogstates:
            self.directory, pattern = dialogstates[key]
        else:
            dir_or_file = os.path.expanduser(dir_or_file)
            if os.path.isdir(dir_or_file):
                self.directory = dir_or_file
            else:
                self.directory, default = os.path.split(dir_or_file)
        self.set_filter(self.directory, pattern)
        self.set_selection(default)
        self.filter_command()
        self.selection.focus_set()
        self.top.wait_visibility()
        self.top.grab_set()
        self.how = None
        self.master.mainloop()
        if key:
            directory, pattern = self.get_filter()
            if self.how:
                directory = os.path.dirname(self.how)
            dialogstates[key] = directory, pattern
        self.top.destroy()
        return self.how

    def quit(self, how=None):
        self.how = how
        self.master.quit()

    def dirs_double_event(self, event):
        self.filter_command()

    def dirs_select_event(self, event):
        dir, pat = self.get_filter()
        subdir = self.dirs.get('active')
        dir = os.path.normpath(os.path.join(self.directory, subdir))
        self.set_filter(dir, pat)

    def files_double_event(self, event):
        self.ok_command()

    def files_select_event(self, event):
        self.set_selection(self.files.get('active'))

    def ok_event(self, event):
        self.ok_command()

    def ok_command(self):
        """Accept the current selection."""
        self.quit(self.get_selection())

    def filter_command(self, event=None):
        """List the directory of the filter, showing matching files."""
        dir, pat = self.get_filter()
        try:
            names = os.listdir(dir)
        except OSError:
            self.master.bell()
            return
        self.directory = dir
        self.set_filter(dir, pat)
        names.sort()
        subdirs = [os.pardir]
        matching = []
        for name in names:
            if os.path.isdir(os.path.join(dir, name)):
                subdirs.append(name)
            elif fnmatch.fnmatch(name, pat):
                matching.append(name)
        self.dirs.delete(0, END)
        for name in subdirs:
            self.dirs.insert(END, name)
        self.files.delete(0, END)
        for name in matching:
            self.files.insert(END, name)
        head, tail = os.path.split(self.get_selection())
        if tail == os.curdir:
            tail = ''
        self.set_selection(tail)

    def get_filter(self):
        """Return the (directory, pattern) of the filter entry."""
        filter = os.path.expanduser(self.filter.get())
        if filter[-1:] == os.sep or os.path.isdir(filter):
            filter = os.path.join(filter, "*")
        return os.path.split(filter)

    def get_selection(self):
        """Return the path in the selection entry."""
        return os.path.expanduser(self.selection.get())

    def cancel_command(self, event=None):
        """Close the dialog without a selection."""
        self.quit()

    def set_filter(self, dir, pat):
        """Show directory *dir* and pattern *pat* in the filter entry."""
        if not os.path.isabs(dir):
            try:
                pwd = os.getcwd()
            except OSError:
                pwd = None
            if pwd:
                dir = os.path.join(pwd, dir)
                dir = os.path.normpath(dir)
        self.filter.delete(0, END)
        self.filter.insert(END, os.path.join(dir or os.curdir, pat or "*"))

    def set_selection(self, file):
        """Show *file* (relative to the current directory) as selection."""
        self.selection.delete(0, END)
        self.selection.insert(END, os.path.join(self.directory, file))


class LoadFileDialog(FileDialog):
    """FileDialog that only accepts existing files."""

    title = "Load File Selection Dialog"

    def ok_command(self):
        file = self.get_selection()
        if not os.path.isfile(file):
            self.master.bell()
        else:
            self.quit(file)


class SaveFileDialog(FileDialog):
    """FileDialog that asks before overwriting an existing file."""

    title = "Save File Selection Dialog"

    def ok_command(self):
        file = self.get_selection()
        if os.path.exists(file):
            if os.path.isdir(file):
                self.master.bell()
                return
            d = Dialog(self.top,
                       title="Overwrite Existing File Question",
                       text="Overwrite existing file %r?" % (file,),
                       bitmap='questhead',
                       default=1,
                       strings=("Yes", "Cancel"))
            if d.num != 0:
                return
        else:
            head, tail = os.path.split(file)
            if not os.path.isdir(head):
                self.master.bell()
                return
        self.quit(file)


# ---------------------------------------------------------------------------
# Native dialogs

class _Dialog(commondialog.Dialog):

    def _fixoptions(self):
        try:
            # Tk needs a list of pairs, not an arbitrary iterable
            self.options["filetypes"] = tuple(self.options["filetypes"])
        except KeyError:
            pass

    def _fixresult(self, widget, result):
        if result:
            result = getattr(result, "string", result)
            path, file = os.path.split(result)
            self.options["initialdir"] = path
            self.options["initialfile"] = file
        self.filename = result
        return result


class Open(_Dialog):
    """The native "open file" dialog."""

    command = "tk_getOpenFile"

    def _fixresult(self, widget, result):
        if isinstance(result, tuple):
            # several files selected
            result = tuple([getattr(r, "string", r) for r in result])
            if result:
                path, file = os.path.split(result[0])
                self.options["initialdir"] = path
            return result
        if not widget.tk.wantobjects() and "multiple" in self.options:
            return self._fixresult(widget, widget.tk.splitlist(result))
        return _Dialog._fixresult(self, widget, result)


class SaveAs(_Dialog):
    """The native "save as" dialog."""

    command = "tk_getSaveFile"


class Directory(commondialog.Dialog):
    """The native directory chooser."""

    command = "tk_chooseDirectory"

    def _fixresult(self, widget, result):
        if result:
            result = getattr(result, "string", result)
            self.options["initialdir"] = result
        self.directory = result
        return result


def askopenfilename(**options):
    """Ask for the name of an existing file to open."""
    return Open(**options).show()


def asksaveasfilename(**options):
    """Ask for a file name to save to."""
    return SaveAs(**options).show()


def askopenfilenames(**options):
    """Ask for several files to open; return a tuple of names (or '')."""
    options["multiple"] = 1
    return Open(**options).show()


def askopenfile(mode="r", **options):
    """Ask for a file and return it opened in *mode* (or None)."""
    filename = Open(**options).show()
    if filename:
        return open(filename, mode)
    return None


def askopenfiles(mode="r", **options):
    """Ask for several files and return them opened in *mode*."""
    files = askopenfilenames(**options)
    if files:
        files = [open(filename, mode) for filename in files]
    return files


def asksaveasfile(mode="w", **options):
    """Ask for a file to save to and return it opened in *mode* (or
    None)."""
    filename = SaveAs(**options).show()
    if filename:
        return open(filename, mode)
    return None


def askdirectory(**options):
    """Ask for a directory."""
    return Directory(**options).show()


def test():
    """Simple test program."""
    root = Tk()
    root.withdraw()
    fd = LoadFileDialog(root)
    loadfile = fd.go(key="test")
    fd = SaveFileDialog(root)
    savefile = fd.go(key="test")
    print(loadfile, savefile)
    print("open", askopenfilename(filetypes=[("all files", "*")]))
    print("saveas", asksaveasfilename())


if __name__ == '__main__':
    test()
