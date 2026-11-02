"""Run > Run Module, Run... Customized and Check Module.

The module is saved (asking first unless autosave is on), compiled and
checked with tabnanny; Run then restarts the shell (unless asked not to)
and executes the code in the shell's namespace with sys.argv and the
current directory set for the file.
"""
import os
import tabnanny
import time
import tokenize

from tkinter import messagebox

from idlelib.config import idleConf
from idlelib import macosx
from idlelib import pyshell
from idlelib.query import CustomRun
from idlelib import outwin

indent_message = """Error: Inconsistent indentation detected!

1) Your indentation is outright incorrect (easy to fix), OR

2) Your indentation mixes tabs and spaces.

To fix case 2, change all tabs to spaces by using Edit->Select All followed \
by Format->Untabify Region and specify the number of columns used by each tab.
"""


class ScriptBinding:
    """The Run menu commands of an editor window."""

    def __init__(self, editwin):
        self.editwin = editwin
        self.flist = self.editwin.flist
        self.root = self.editwin.root
        # Arguments of the last Run... Customized.
        self.cli_args = []
        self.perf = 0.0  # when the last error box was closed

        if macosx.isCocoaTk():
            self.editwin.text_frame.bind('<<run-module-event-2>>',
                                         self._run_module_event)

    def check_module_event(self, event):
        if isinstance(self.editwin, outwin.OutputWindow):
            self.editwin.text.bell()
            return 'break'
        filename = self.getfilename()
        if not filename:
            return 'break'
        if not self.checksyntax(filename):
            return 'break'
        if not self.tabnanny(filename):
            return 'break'
        return "break"

    def tabnanny(self, filename):
        """Check the indentation; report and return False on problems."""
        with tokenize.open(filename) as f:
            try:
                tabnanny.process_tokens(tokenize.generate_tokens(f.readline))
            except tokenize.TokenError as msg:
                msgtxt, (lineno, start) = msg.args
                self.editwin.gotoline(lineno)
                self.errorbox("Tabnanny Tokenizing Error",
                              f"Token Error: {msgtxt}")
                return False
            except tabnanny.NannyNag as nag:
                self.editwin.gotoline(nag.get_lineno())
                self.errorbox("Tab/space error", indent_message)
                return False
        return True

    def checksyntax(self, filename):
        """Compile the file; return the code object, or False after
        showing the error."""
        self.shell = shell = self.flist.open_shell()
        saved_stream = shell.get_warning_stream()
        # Compiler warnings go to the shell.
        shell.set_warning_stream(shell.stderr)
        with open(filename, 'rb') as f:
            source = f.read()
        if b'\r' in source:
            source = source.replace(b'\r\n', b'\n').replace(b'\r', b'\n')
        if source and source[-1] != ord(b'\n'):
            source = source + b'\n'
        editwin = self.editwin
        text = editwin.text
        text.tag_remove("ERROR", "1.0", "end")
        try:
            return compile(source, filename, "exec")
        except (SyntaxError, OverflowError, ValueError) as value:
            msg = getattr(value, 'msg', '') or value or "<no detail available>"
            lineno = getattr(value, 'lineno', '') or 1
            offset = getattr(value, 'offset', '') or 0
            if offset == 0:
                lineno += 1  # mark the end of the line
            pos = f"0.0 + {lineno - 1} lines + {offset - 1} chars"
            editwin.colorize_syntax_error(text, pos)
            self.errorbox("SyntaxError", "%-20s" % msg)
            return False
        except Exception as err:
            # Any other compiler failure (MemoryError, RecursionError, ...).
            self.errorbox(type(err).__name__,
                          str(err) or "<no detail available>")
            return False
        finally:
            shell.set_warning_stream(saved_stream)

    def run_custom_event(self, event):
        return self.run_module_event(event, customize=True)

    def run_module_event(self, event, *, customize=False):
        """Run the module after saving and checking it.

        With *customize*, ask for the command line arguments and whether
        to restart the shell first.
        """
        if macosx.isCocoaTk():
            # The menu is still posted on macOS; run a little later.
            self.editwin.text_frame.after(
                200, lambda: self.editwin.text_frame.event_generate(
                    '<<run-module-event-2>>'))
            return 'break'
        return self._run_module_event(event, customize=customize)

    def _run_module_event(self, event, *, customize=False):
        if isinstance(self.editwin, outwin.OutputWindow):
            self.editwin.text.bell()
            return 'break'
        filename = self.getfilename()
        if not filename:
            return 'break'
        code = self.checksyntax(filename)
        if not code:
            return 'break'
        if not self.tabnanny(filename):
            return 'break'
        if customize:
            title = f"Customize {self.editwin.short_title()} Run"
            run_args = CustomRun(self.shell.text, title,
                                 cli_args=self.cli_args).result
            if not run_args:  # cancelled
                return 'break'
            self.cli_args, restart = run_args
        else:
            restart = True
        interp = self.shell.interp
        if self.shell.executing and not restart:
            # Running into a busy shell without restarting is impossible.
            interp.display_executing_dialog()
            return 'break'
        if pyshell.use_subprocess and restart:
            interp.restart_subprocess(with_cwd=False, filename=filename)
        dirname = os.path.dirname(filename)
        argv = [filename] + list(self.cli_args)
        # Set up __file__, sys.argv and the directory in the user's
        # namespace, leaving no helper names behind.
        interp.runcommand(
            "if 1:\n"
            f"    __file__ = {filename!r}\n"
            "    import sys as _idle_sys, os as _idle_os\n"
            f"    _idle_argv = {argv!r}\n"
            "    if (not _idle_sys.argv or len(_idle_argv) > 1 or\n"
            "            _idle_os.path.basename(_idle_sys.argv[0]) !=\n"
            "            _idle_os.path.basename(__file__)):\n"
            "        _idle_sys.argv = _idle_argv\n"
            f"    _idle_os.chdir({dirname!r})\n"
            "    del _idle_sys, _idle_os, _idle_argv\n"
            "\n")
        interp.prepend_syspath(filename)
        interp.runcode(code)
        return 'break'

    def getfilename(self):
        """Return the file name after saving, or None if not saved."""
        filename = self.editwin.io.filename
        if not self.editwin.get_saved():
            autosave = idleConf.GetOption('main', 'General',
                                          'autosave', type='bool')
            if autosave and filename:
                self.editwin.io.save(None)
            else:
                confirm = self.ask_save_dialog()
                self.editwin.text.focus_set()
                if confirm:
                    self.editwin.io.save(None)
                    filename = self.editwin.io.filename
                else:
                    filename = None
        return filename

    def ask_save_dialog(self):
        msg = "Source Must Be Saved\n" + 5 * ' ' + "OK to Save?"
        return messagebox.askokcancel(title="Save Before Run or Check",
                                      message=msg,
                                      default=messagebox.OK,
                                      parent=self.editwin.text)

    def errorbox(self, title, message):
        messagebox.showerror(title, message, parent=self.editwin.text)
        self.editwin.text.focus_set()
        self.perf = time.perf_counter()


if __name__ == "__main__":
    from unittest import main
    main('idlelib.idle_test.test_runscript', verbosity=2,)
