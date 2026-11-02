"""Small modal dialogs asking for one value, with validation.

Query is the base: a prompt, an entry, OK and Cancel, and an error line.
Subclasses check the input in entry_ok() and return the value, or report
the problem with showerror() and return None.

  SectionName -- name of a new theme or key set
  ModuleName  -- module to open (File > Open Module)
  Goto        -- line number (Edit > Go to Line)
  HelpSource  -- menu item and path/URL of an additional help source
  CustomRun   -- command line arguments for Run... Customized
"""
import importlib.util
import importlib.abc
import os
from sys import executable, platform  # Platform is set for one test.

from tkinter import Toplevel, StringVar, BooleanVar, W, E, S
from tkinter.ttk import Frame, Button, Entry, Label, Checkbutton
from tkinter import filedialog
from tkinter.font import Font
from tkinter.simpledialog import _setup_dialog

from idlelib.util import split_cli_args, join_cli_args


class Query(Toplevel):
    """Ask for one value; `result` is it, or None when cancelled."""

    def __init__(self, parent, title, message, *, text0='', used_names={},
                 _htest=False, _utest=False):
        """Create the dialog above *parent* and wait for the answer.

        *text0* is the initial entry text; *used_names* are names that
        must not be chosen.  With _utest the dialog is not shown and the
        call returns at once.
        """
        self.parent = parent
        self.message = message
        self.text0 = text0
        self.used_names = used_names

        Toplevel.__init__(self, parent)
        self.withdraw()  # shown when complete
        self.title(title)
        self.transient(parent)
        if not _utest:
            self.grab_set()
        _setup_dialog(self)
        if self._windowingsystem == 'aqua':
            self.bind("<Command-.>", self.cancel)
        self.bind('<Key-Escape>', self.cancel)
        self.protocol("WM_DELETE_WINDOW", self.cancel)
        self.bind('<Key-Return>', self.ok)
        self.bind("<KP_Enter>", self.ok)

        self.create_widgets()
        self.update_idletasks()
        self.geometry(
                "+%d+%d" % (
                    parent.winfo_rootx() +
                    (parent.winfo_width()/2 - self.winfo_reqwidth()/2),
                    parent.winfo_rooty() +
                    ((parent.winfo_height()/2 - self.winfo_reqheight()/2)
                    if not _htest else 150)
                ) )
        self.resizable(height=False, width=False)

        if not _utest:
            self.deiconify()
            self.entry.focus_set()
            self.wait_window()

    def create_widgets(self, ok_text='OK'):
        """Create the prompt, entry, error line and buttons."""
        self.frame = frame = Frame(self, padding=10)
        frame.grid(column=0, row=0, sticky='news')
        frame.grid_columnconfigure(0, weight=1)

        entrylabel = Label(frame, anchor='w', justify='left', text=self.message)
        self.entryvar = StringVar(self, self.text0)
        self.entry = Entry(frame, width=30, textvariable=self.entryvar)
        self.error_font = Font(name='TkCaptionFont', exists=True, root=self.parent)
        self.entry_error = Label(frame, text=' ', foreground='red',
                                 font=self.error_font)
        entrylabel.grid(column=0, row=0, columnspan=3, padx=5, sticky=W)
        self.entry.grid(column=0, row=1, columnspan=3, padx=5, sticky=W + E,
                        pady=[10, 0])
        self.entry_error.grid(column=0, row=2, columnspan=3, padx=5,
                              sticky=W + E)
        self.create_extra()

        self.button_ok = Button(frame, text=ok_text, default='active',
                                command=self.ok)
        self.button_cancel = Button(frame, text='Cancel', command=self.cancel)
        self.button_ok.grid(column=1, row=99, padx=5)
        self.button_cancel.grid(column=2, row=99, padx=5)

    def create_extra(self):
        """Add more widgets (subclasses)."""

    def showerror(self, message, widget=None):
        """Show *message* in the error line (of *widget*)."""
        (widget or self.entry_error)['text'] = 'ERROR: ' + message

    def entry_ok(self):
        """Return the stripped entry text, or None if it is blank."""
        self.entry_error['text'] = ''
        entry = self.entry.get().strip()
        if not entry:
            self.showerror('blank line.')
            return None
        return entry

    def ok(self, event=None):
        """Accept a valid entry and close; else keep the dialog open."""
        self.entry_error['text'] = ''
        entry = self.entry_ok()
        if entry is not None:
            self.result = entry
            self.destroy()
        else:
            self.entry.focus_set()

    def cancel(self, event=None):
        """Close without a result."""
        self.result = None
        self.destroy()

    def destroy(self):
        self.grab_release()
        super().destroy()


class SectionName(Query):
    """Name of a new theme or key set (unused, at most 30 characters)."""

    def __init__(self, parent, title, message, used_names,
                 *, _htest=False, _utest=False):
        super().__init__(parent, title, message, used_names=used_names,
                         _htest=_htest, _utest=_utest)

    def entry_ok(self):
        name = self.entry.get().strip()
        if not name:
            self.showerror('no name specified.')
            return None
        if len(name) > 30:
            self.showerror('name is longer than 30 characters.')
            return None
        if name in self.used_names:
            self.showerror('name is already in use.')
            return None
        return name


class ModuleName(Query):
    """Name of a module with Python source; the result is its file."""

    def __init__(self, parent, title, message, text0,
                 *, _htest=False, _utest=False):
        super().__init__(parent, title, message, text0=text0,
                         _htest=_htest, _utest=_utest)

    def entry_ok(self):
        name = self.entry.get().strip()
        if not name:
            self.showerror('no name specified.')
            return None
        try:
            spec = importlib.util.find_spec(name)
        except (ValueError, ImportError) as msg:
            self.showerror(str(msg))
            return None
        if spec is None:
            self.showerror("module not found.")
            return None
        if not isinstance(spec.loader, importlib.abc.SourceLoader):
            self.showerror("not a source-based module.")
            return None
        try:
            file_path = spec.loader.get_filename(name)
        except AttributeError:
            self.showerror("loader does not support get_filename.")
            return None
        except ImportError:
            # namespace packages have no file
            self.showerror("not a source-based module.")
            return None
        return file_path


class Goto(Query):
    """A line number (positive integer)."""

    def entry_ok(self):
        try:
            lineno = int(self.entry.get())
        except ValueError:
            self.showerror('not a base 10 integer.')
            return None
        if lineno <= 0:
            self.showerror('not a positive integer.')
            return None
        return lineno


class HelpSource(Query):
    """Menu item and path (or URL) of an additional help source."""

    def __init__(self, parent, title, *, menuitem='', filepath='',
                 used_names={}, _htest=False, _utest=False):
        self.filepath = filepath
        message = 'Name for item on Help menu:'
        super().__init__(
                parent, title, message, text0=menuitem,
                used_names=used_names, _htest=_htest, _utest=_utest)

    def create_extra(self):
        """Add the path entry and Browse button."""
        frame = self.frame
        pathlabel = Label(frame, anchor='w', justify='left',
                          text='Help File Path: Enter URL or browse for file')
        self.pathvar = StringVar(self, self.filepath)
        self.path = Entry(frame, textvariable=self.pathvar, width=40)
        browse = Button(frame, text='Browse', width=8,
                        command=self.browse_file)
        self.path_error = Label(frame, text=' ', foreground='red',
                                font=self.error_font)

        pathlabel.grid(column=0, row=10, columnspan=3, padx=5, pady=[10, 0],
                       sticky=W)
        self.path.grid(column=0, row=11, columnspan=2, padx=5, sticky=W + E,
                       pady=[10, 0])
        browse.grid(column=2, row=11, padx=5, sticky=W + S)
        self.path_error.grid(column=0, row=12, columnspan=3, padx=5,
                             sticky=W + E)

    def askfilename(self, filetypes, initdir, initfile):
        # Separate for testing.
        return filedialog.Open(parent=self, filetypes=filetypes)\
               .show(initialdir=initdir, initialfile=initfile)

    def browse_file(self):
        """Ask for a help file; it replaces the path entry."""
        filetypes = [
            ("HTML Files", "*.htm *.html", "TEXT"),
            ("PDF Files", "*.pdf", "TEXT"),
            ("Windows Help Files", "*.chm"),
            ("Text Files", "*.txt", "TEXT"),
            ("All Files", "*")]
        path = self.pathvar.get()
        if path:
            dir, base = os.path.split(path)
        else:
            base = None
            if platform[:3] == 'win':
                dir = os.path.join(os.path.dirname(executable), 'Doc')
                if not os.path.isdir(dir):
                    dir = os.getcwd()
            else:
                dir = os.getcwd()
        file = self.askfilename(filetypes, dir, base)
        if file:
            self.pathvar.set(file)

    item_ok = SectionName.entry_ok  # the menu item name

    def path_ok(self):
        """Return the help path (URL or existing file), or None."""
        path = self.path.get().strip()
        if not path:
            self.showerror('no help file path specified.', self.path_error)
            return None
        if not path.startswith(('www.', 'http')):
            if path[:5] == 'file:':
                path = path[5:]
            if not os.path.exists(path):
                self.showerror('help file path does not exist.',
                               self.path_error)
                return None
            if platform == 'darwin':  # for Mac Safari
                path = "file://" + path
        return path

    def entry_ok(self):
        """Return (menu item, path), or None if either is invalid."""
        self.path_error['text'] = ''
        name = self.item_ok()
        path = self.path_ok()
        return None if name is None or path is None else (name, path)


class CustomRun(Query):
    """Command line arguments, and whether to restart the shell first."""

    def __init__(self, parent, title, *, cli_args=[],
                 _htest=False, _utest=False):
        message = 'Command Line Arguments for sys.argv:'
        super().__init__(
                parent, title, message, text0=join_cli_args(cli_args),
                _htest=_htest, _utest=_utest)

    def create_extra(self):
        """Add the restart checkbutton."""
        frame = self.frame
        self.restartvar = BooleanVar(self, value=True)
        restart = Checkbutton(frame, variable=self.restartvar, onvalue=True,
                              offvalue=False, text='Restart shell')
        self.args_error = Label(frame, text=' ', foreground='red',
                                font=self.error_font)
        restart.grid(column=0, row=10, columnspan=3, padx=5, sticky='w')
        self.args_error.grid(column=0, row=12, columnspan=3, padx=5,
                             sticky='we')

    def cli_args_ok(self):
        """Return the arguments as a list, or None if they cannot be
        split."""
        cli_string = self.entry.get().strip()
        try:
            return split_cli_args(cli_string)
        except ValueError as err:
            self.showerror(str(err))
            return None

    def entry_ok(self):
        """Return (arguments, restart), or None."""
        self.entry_error['text'] = ''
        cli_args = self.cli_args_ok()
        restart = self.restartvar.get()
        return None if cli_args is None else (cli_args, restart)


if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_query', verbosity=2, exit=False)
