"""Opening, saving and printing the file of an editor window.

IOBinding is attached to every editor window.  Python source files are
decoded with their coding declaration (else UTF-8), other files with
UTF-8 or the locale encoding.  The line-end convention found when loading
is kept for saving.
"""
import io
import os
import shlex
import sys
import tempfile
import tokenize

from tkinter import filedialog
from tkinter import messagebox
from tkinter.simpledialog import askstring  # loadfile encoding.

from idlelib.config import idleConf
from idlelib.util import py_extensions

# Encoding of the streams of the shell's user process.
encoding = 'utf-8'
errors = 'surrogatepass' if sys.platform == 'win32' else 'surrogateescape'

_py_patterns = ' '.join('*' + ext for ext in py_extensions)


class IOBinding:
    """File commands of an editor window."""

    filetypes = (
        ("Python files", _py_patterns, "TEXT"),
        ("Text files", "*.txt", "TEXT"),
        ("All files", "*"),
    )
    # The types offered by Save As (the output window prefers text).
    save_filetypes = filetypes
    defaultextension = '.py' if sys.platform == 'darwin' else ''

    def __init__(self, editwin):
        self.editwin = editwin
        self.text = editwin.text
        self.__id_open = self.text.bind("<<open-window-from-file>>", self.open)
        self.__id_save = self.text.bind("<<save-window>>", self.save)
        self.__id_saveas = self.text.bind("<<save-window-as-file>>",
                                          self.save_as)
        self.__id_savecopy = self.text.bind("<<save-copy-of-window-as-file>>",
                                            self.save_a_copy)
        self.fileencoding = 'utf-8'
        self.__id_print = self.text.bind("<<print-window>>", self.print_window)
        self.filename_change_hook = None
        self.filename = None
        self.dirname = None
        self.eol_convention = os.linesep
        self.opendialog = None
        self.savedialog = None

    def close(self):
        """Remove the bindings."""
        self.text.unbind("<<open-window-from-file>>", self.__id_open)
        self.text.unbind("<<save-window>>", self.__id_save)
        self.text.unbind("<<save-window-as-file>>", self.__id_saveas)
        self.text.unbind("<<save-copy-of-window-as-file>>", self.__id_savecopy)
        self.text.unbind("<<print-window>>", self.__id_print)
        self.editwin = None
        self.text = None
        self.filename_change_hook = None

    def get_saved(self):
        return self.editwin.get_saved()

    def set_saved(self, flag):
        self.editwin.set_saved(flag)

    def reset_undo(self):
        self.editwin.reset_undo()

    def set_filename_change_hook(self, hook):
        self.filename_change_hook = hook

    def set_filename(self, filename):
        """Set the file name (and directory) of the window."""
        if filename and os.path.isdir(filename):
            self.filename = None
            self.dirname = filename
        else:
            self.filename = filename
            self.dirname = None
            self.set_saved(1)
            if self.filename_change_hook:
                self.filename_change_hook()

    def open(self, event=None, editFile=None):
        """Open a file (asking for it unless *editFile*): in this window if
        it is empty and unchanged, else in a new one."""
        flist = self.editwin.flist
        if flist:
            if not editFile:
                filename = self.askopenfile()
            else:
                filename = editFile
            if filename:
                # Reuse this window if it is an empty, unchanged editor.
                if (self.editwin and not getattr(self.editwin, 'interp', None)
                        and not self.filename and self.get_saved()):
                    flist.open(filename, self.loadfile)
                else:
                    flist.open(filename)
            else:
                if self.text:
                    self.text.focus_set()
            return "break"
        # Without a file list (e.g. in tests): open in this window.
        if self.get_saved():
            reply = self.maybesave()
            if reply == "cancel":
                self.text.focus_set()
                return "break"
        if not editFile:
            filename = self.askopenfile()
        else:
            filename = editFile
        if filename:
            self.loadfile(filename)
        else:
            self.text.focus_set()
        return "break"

    def loadfile(self, filename):
        """Load *filename* into the window; return True on success."""
        try:
            if filename.endswith(py_extensions):
                with tokenize.open(filename) as f:
                    chars = f.read()
                    fileencoding = f.encoding
                    eol = f.newlines
            else:
                try:
                    with open(filename, encoding='utf-8') as f:
                        chars = f.read()
                        fileencoding = 'utf-8'
                        eol = f.newlines
                except UnicodeDecodeError:
                    enc = askstring(
                        "Specify file encoding",
                        "The file's encoding is invalid for Python 3.x.\n"
                        "IDLE will convert it to UTF-8.\n"
                        "What is the current encoding of the file?",
                        initialvalue='utf-8',
                        parent=self.editwin.text)
                    with open(filename, encoding=enc) as f:
                        chars = f.read()
                        fileencoding = f.encoding
                        eol = f.newlines
        except (OSError, SyntaxError, LookupError, UnicodeDecodeError) as err:
            messagebox.showerror("I/O Error", str(err), parent=self.text)
            return False
        if not isinstance(eol, str):
            eol = (eol or (os.linesep,))[0]
        self.eol_convention = eol
        self.fileencoding = fileencoding

        self.text.delete("1.0", "end")
        self.set_filename(None)
        self.text.insert("1.0", chars)
        self.reset_undo()
        self.set_filename(filename)
        self.text.mark_set("insert", "1.0")
        self.text.yview("insert")
        self.updaterecentfileslist(filename)
        return True

    def maybesave(self):
        """Ask whether to save unsaved changes; return 'yes', 'no' or
        'cancel'."""
        if self.get_saved():
            return "yes"
        message = ("Do you want to save "
                   f"{self.filename or 'this untitled document'} "
                   "before closing?")
        confirm = messagebox.askyesnocancel(
                  title="Save On Close",
                  message=message,
                  default=messagebox.YES,
                  parent=self.text)
        if confirm:
            reply = "yes"
            self.save(None)
            if not self.get_saved():
                reply = "cancel"
        elif confirm is None:
            reply = "cancel"
        else:
            reply = "no"
        self.text.focus_set()
        return reply

    def save(self, event):
        """Save to the window's file (asking for a name if it has none)."""
        if not self.filename:
            self.save_as(event)
        else:
            if self.writefile(self.filename):
                self.set_saved(True)
                try:
                    self.editwin.store_file_breaks()
                except AttributeError:  # may be a PyShell
                    pass
        self.text.focus_set()
        return "break"

    def save_as(self, event):
        """Save under a new name, which becomes the window's file."""
        filename = self.asksavefile()
        if filename:
            if self.writefile(filename):
                self.set_filename(filename)
                self.set_saved(1)
                try:
                    self.editwin.store_file_breaks()
                except AttributeError:
                    pass
        self.text.focus_set()
        self.updaterecentfileslist(filename)
        return "break"

    def save_a_copy(self, event):
        """Save to another file without changing the window's file."""
        filename = self.asksavefile()
        if filename:
            self.writefile(filename)
        self.text.focus_set()
        self.updaterecentfileslist(filename)
        return "break"

    def writefile(self, filename):
        """Write the text to *filename*; return True on success."""
        text = self.fixnewlines()
        chars = self.encode(text)
        try:
            with open(filename, "wb") as f:
                f.write(chars)
                f.flush()
                os.fsync(f.fileno())
            return True
        except OSError as msg:
            messagebox.showerror("I/O Error", str(msg), parent=self.text)
            return False

    def fixnewlines(self):
        """Return the text to save, with the file's line ends: a final
        newline is added to an editor's text; the shell is saved with its
        prompts."""
        if hasattr(self.editwin, "interp"):
            text = self.editwin.get_prompt_text('1.0', self.text.index('end-1c'))
        else:
            if self.text.get("end-2c") != '\n':
                self.text.insert("end-1c", "\n")  # changes the text too
            text = self.text.get('1.0', "end-1c")
        if self.eol_convention != "\n":
            text = text.replace("\n", self.eol_convention)
        return text

    def encode(self, chars):
        """Encode *chars* for saving: with the coding declaration of a
        Python file, else UTF-8."""
        if isinstance(chars, bytes):
            return chars
        if self.fileencoding == 'utf-8-sig':
            return chars.encode('utf-8-sig')
        try:
            encoded = chars.encode('ascii')
        except UnicodeEncodeError:
            pass
        else:
            return encoded
        try:
            enc, _ = tokenize.detect_encoding(io.BytesIO(chars.encode('utf-8')).readline)
        except SyntaxError as err:
            failed = str(err)
        else:
            try:
                return chars.encode(enc)
            except UnicodeEncodeError:
                failed = f"Invalid encoding '{enc}'"
        messagebox.showerror(
            "I/O Error",
            f"{failed}.\nSaving as UTF-8",
            parent=self.text)
        return chars.encode('utf-8-sig')

    def print_window(self, event):
        """Print the text with the configured print command."""
        confirm = messagebox.askokcancel(
                  title="Print",
                  message="Print to Default Printer",
                  default=messagebox.OK,
                  parent=self.text)
        if not confirm:
            self.text.focus_set()
            return "break"
        tempfilename = None
        saved = self.get_saved()
        if saved:
            filename = self.filename
        if not saved or filename is None:
            fd, tempfilename = tempfile.mkstemp(prefix='IDLE_tmp_')
            filename = tempfilename
            os.close(fd)
            if not self.writefile(tempfilename):
                os.unlink(tempfilename)
                return "break"
        platform = os.name
        printPlatform = True
        if platform == 'posix':
            command = idleConf.GetOption('main', 'General', 'print-command-posix')
            command = command + " 2>&1"
        elif platform == 'nt':
            command = idleConf.GetOption('main', 'General', 'print-command-win')
        else:
            printPlatform = False
        if printPlatform:
            command = command % shlex.quote(filename)
            pipe = os.popen(command, "r")
            output = pipe.read().strip()
            status = pipe.close()
            if status:
                output = ("Printing failed (exit status 0x%x)\n" % status +
                          output)
            if output:
                output = "Printing command: %s\n" % repr(command) + output
                messagebox.showerror("Print status", output, parent=self.text)
        else:
            message = f"Printing is not enabled for this platform: {platform}"
            messagebox.showinfo("Print status", message, parent=self.text)
        if tempfilename:
            os.unlink(tempfilename)
        return "break"

    def askopenfile(self):
        dir, base = self.defaultfilename("open")
        if not self.opendialog:
            self.opendialog = filedialog.Open(parent=self.text,
                                              filetypes=self.filetypes)
        filename = self.opendialog.show(initialdir=dir, initialfile=base)
        return filename

    def defaultfilename(self, mode="open"):
        """Return the (directory, file name) proposed by file dialogs."""
        if self.filename:
            dirname, basename = os.path.split(self.filename)
            # Tcl before 9 expands a leading '~' as a home directory.
            from tkinter import TclVersion
            from idlelib import macosx
            if (basename.startswith('~') and TclVersion < 9 and
                    not macosx.isAquaTk()):
                basename = os.path.join(os.curdir, basename)
            return dirname, basename
        if self.dirname:
            return self.dirname, ""
        try:
            pwd = os.getcwd()
        except OSError:
            pwd = ""
        return pwd, ""

    def asksavefile(self):
        dir, base = self.defaultfilename("save")
        if not self.savedialog:
            self.savedialog = filedialog.SaveAs(
                    parent=self.text,
                    filetypes=self.save_filetypes,
                    defaultextension=self.defaultextension)
        filename = self.savedialog.show(initialdir=dir, initialfile=base)
        return filename

    def updaterecentfileslist(self, filename):
        """Add *filename* to the recent files list."""
        if self.editwin.flist:
            self.editwin.update_recent_files_list(filename)


def _io_binding(parent):  # htest #
    from tkinter import Toplevel, Text

    top = Toplevel(parent)
    top.title("Test IOBinding")
    x, y = map(int, parent.geometry().split('+')[1:])
    top.geometry("+%d+%d" % (x, y + 175))

    class MyEditWin:
        def __init__(self, text):
            self.text = text
            self.flist = None
            self.text.bind("<Control-o>", self.open)
            self.text.bind('<Control-p>', self.print)
            self.text.bind("<Control-s>", self.save)
            self.text.bind("<Alt-s>", self.saveas)
            self.text.bind('<Control-c>', self.savecopy)

        def get_saved(self): return 0

        def set_saved(self, flag): pass

        def reset_undo(self): pass

        def open(self, event):
            self.text.event_generate("<<open-window-from-file>>")

        def print(self, event):
            self.text.event_generate("<<print-window>>")

        def save(self, event):
            self.text.event_generate("<<save-window>>")

        def saveas(self, event):
            self.text.event_generate("<<save-window-as-file>>")

        def savecopy(self, event):
            self.text.event_generate("<<save-copy-of-window-as-file>>")

    text = Text(top)
    text.pack()
    text.focus_set()
    editwin = MyEditWin(text)
    IOBinding(editwin)


if __name__ == "__main__":
    from unittest import main
    main('idlelib.idle_test.test_iomenu', verbosity=2, exit=False)
