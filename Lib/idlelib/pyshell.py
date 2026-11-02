#! /usr/bin/env python3
"""IDLE's shell window and the start of IDLE.

The shell (PyShell) is an output window whose text after the 'iomark'
mark is the user's input.  User code normally runs in a separate process
(idlelib.run) controlled through rpc by ModifiedInterpreter; with -n it
runs in the IDLE process itself.
"""
import sys

if __name__ == "__main__":
    # Started as a script: the current directory must not shadow the
    # standard modules IDLE imports (it is given back to user code).
    import os as _os
    _here = _os.path.dirname(_os.path.abspath(__file__))
    if sys.path and (not sys.path[0] or _os.path.abspath(sys.path[0])
                     in (_os.getcwd(), _here)):
        del sys.path[0]
    del _os, _here
    sys.modules['idlelib.pyshell'] = sys.modules['__main__']

try:
    from tkinter import *
except ImportError:
    print("IDLE needs tkinter, which cannot be imported here"
          " (Tk may be missing).", file=sys.__stderr__)
    raise SystemExit(1)

if sys.platform == 'win32':
    from idlelib.util import fix_win_hidpi
    fix_win_hidpi()

from tkinter import messagebox

import ast
from code import InteractiveInterpreter
import getopt
import itertools
import linecache
import os
import os.path
from platform import python_version
import re
import socket
import subprocess
import threading
import time
import tokenize
import warnings

from idlelib.colorizer import ColorDelegator
from idlelib.config import idleConf
from idlelib.delegator import Delegator
from idlelib import debugger
from idlelib import debugger_r
from idlelib.editor import EditorWindow, fixwordbreaks
from idlelib.filelist import FileList
from idlelib.outwin import OutputWindow
from idlelib import replace
from idlelib import rpc
from idlelib.run import idle_formatwarning, StdInputFile, StdOutputFile
from idlelib.undo import UndoDelegator
from idlelib.util import fix_x11_paste

# The rpc connection to the user process.
HOST = '127.0.0.1'
PORT = 0  # let the system choose a port for the rpc connection

# Where warnings go (the shell's stderr once it exists).
warning_stream = sys.__stderr__

use_subprocess = True  # False with -n: user code runs in this process


def idle_showwarning(message, category, filename, lineno, file=None,
                     line=None):
    """Show a warning in the shell, followed by a new prompt."""
    if file is None:
        file = warning_stream
    try:
        file.write(idle_formatwarning(message, category, filename,
                                      lineno, line=line))
        file.write(">>> ")
    except (AttributeError, OSError):
        pass  # the stream is gone


_warnings_showwarning = None


def capture_warnings(capture):
    """Show warnings in the shell (capture=True) or as usual (False)."""
    global _warnings_showwarning
    if capture:
        if _warnings_showwarning is None:
            _warnings_showwarning = warnings.showwarning
            warnings.showwarning = idle_showwarning
    else:
        if _warnings_showwarning is not None:
            warnings.showwarning = _warnings_showwarning
            _warnings_showwarning = None


def extended_linecache_checkcache(filename=None,
                                  orig_checkcache=linecache.checkcache):
    """Like linecache.checkcache, but keep the entries of the shell's
    input ('<pyshell#N>'), which have no file to check."""
    cache = linecache.cache
    save = {key: value for key, value in cache.items()
            if key[:1] + key[-1:] == '<>'}
    orig_checkcache(filename)
    cache.update(save)


linecache.checkcache = extended_linecache_checkcache


def restart_line(width, filename):
    """Return the line written when the shell restarts, padded with '='
    to *width* (but never truncated below the tag)."""
    tag = f"= RESTART: {filename or 'Shell'} ="
    if width >= len(tag):
        div, mod = divmod(width - len(tag), 2)
        return f"{(div + mod) * '='}{tag}{div * '='}"
    return tag[:-2]  # without the trailing ' ='


def fix_user_path(paths):
    """Return *paths* without the idlelib directory (user code should not
    import IDLE's modules by their bare names)."""
    idlelib_dir = os.path.dirname(os.path.abspath(__file__))
    return [p for p in paths if not p or os.path.abspath(p) != idlelib_dir]


class PyShellEditorWindow(EditorWindow):
    """An editor window of the shell's file list: it has breakpoints."""

    def __init__(self, *args):
        self.breakpoints = []
        EditorWindow.__init__(self, *args)
        self.text.bind("<<set-breakpoint>>", self.set_breakpoint_event)
        self.text.bind("<<clear-breakpoint>>", self.clear_breakpoint_event)
        self.text.bind("<<open-python-shell>>", self.flist.open_shell)

        self.breakpointPath = os.path.join(idleConf.userdir, 'breakpoints.lst')
        # Breakpoints follow renames (Save As).
        def filename_changed_hook(old_hook=self.io.filename_change_hook,
                                  self=self):
            self.restore_file_breaks()
            old_hook()
        self.io.set_filename_change_hook(filename_changed_hook)
        if self.io.filename:
            self.restore_file_breaks()
        self.color_breakpoint_text()

    rmenu_specs = [
        ("Cut", "<<cut>>", "rmenu_check_cut"),
        ("Copy", "<<copy>>", "rmenu_check_copy"),
        ("Paste", "<<paste>>", "rmenu_check_paste"),
        (None, None, None),
        ("Set Breakpoint", "<<set-breakpoint>>", None),
        ("Clear Breakpoint", "<<clear-breakpoint>>", None)
    ]

    def color_breakpoint_text(self, color=True):
        """Show (or hide) the breakpoint lines' background."""
        if self.io is None:
            return
        if color:
            theme = idleConf.CurrentTheme()
            cfg = idleConf.GetHighlight(theme, "break")
        else:
            cfg = {'foreground': '', 'background': ''}
        self.text.tag_config('BREAK', cfg)

    def set_breakpoint(self, lineno):
        text = self.text
        filename = self.io.filename
        text.tag_add("BREAK", f"{lineno}.0", f"{lineno + 1}.0")
        if lineno not in self.breakpoints:
            self.breakpoints.append(lineno)
        try:  # the debugger may be running
            debug = self.flist.pyshell.interp.debugger
            debug.set_breakpoint(filename, lineno)
        except AttributeError:
            pass

    def set_breakpoint_event(self, event=None):
        text = self.text
        filename = self.io.filename
        if not filename:
            text.bell()
            return
        lineno = int(float(text.index("insert")))
        self.set_breakpoint(lineno)

    def clear_breakpoint_event(self, event=None):
        text = self.text
        filename = self.io.filename
        if not filename:
            text.bell()
            return
        lineno = int(float(text.index("insert")))
        try:
            self.breakpoints.remove(lineno)
        except ValueError:
            pass
        text.tag_remove("BREAK", "insert linestart", "insert lineend +1char")
        try:
            debug = self.flist.pyshell.interp.debugger
            debug.clear_breakpoint(filename, lineno)
        except AttributeError:
            pass

    def clear_file_breaks(self):
        if self.breakpoints:
            text = self.text
            filename = self.io.filename
            if not filename:
                text.bell()
                return
            self.breakpoints = []
            text.tag_remove("BREAK", "1.0", END)
            try:
                debug = self.flist.pyshell.interp.debugger
                debug.clear_file_breaks(filename)
            except AttributeError:
                pass

    def store_file_breaks(self):
        """Save the breakpoints of this file in breakpoints.lst."""
        # The file holds lines 'filename=[linenumbers]'; this file's line
        # is replaced.
        breaks = self.breakpoints
        filename = self.io.filename
        try:
            with open(self.breakpointPath) as fp:
                lines = fp.readlines()
        except OSError:
            lines = []
        try:
            with open(self.breakpointPath, "w") as new_file:
                for line in lines:
                    if not line.startswith(filename + '='):
                        new_file.write(line)
                self.update_breakpoints()
                breaks = self.breakpoints
                if breaks:
                    new_file.write(filename + '=' + str(breaks) + '\n')
        except OSError as err:
            if not getattr(self.root, "breakpoint_error_displayed", False):
                self.root.breakpoint_error_displayed = True
                messagebox.showerror(
                    title='IDLE Error',
                    message=f'The breakpoints could not be saved:\n{err}',
                    parent=self.text)

    def restore_file_breaks(self):
        self.text.update()  # make sure the file is loaded
        if self.io is None:
            return
        filename = self.io.filename
        if filename is None:
            return
        if os.path.isfile(self.breakpointPath):
            with open(self.breakpointPath) as fp:
                lines = fp.readlines()
            for line in lines:
                if line.startswith(filename + '='):
                    # The value is a list literal of line numbers.
                    try:
                        breakpoint_linenumbers = ast.literal_eval(
                            line[len(filename) + 1:].strip())
                    except (ValueError, SyntaxError):
                        continue
                    for breakpoint_linenumber in breakpoint_linenumbers:
                        self.set_breakpoint(breakpoint_linenumber)

    def update_breakpoints(self):
        """The breakpoint lines move with the text: read them back from
        the BREAK tag."""
        text = self.text
        ranges = text.tag_ranges("BREAK")
        self.breakpoints = self.ranges_to_linenumbers(ranges)

    def ranges_to_linenumbers(self, ranges):
        lines = []
        for index in range(0, len(ranges), 2):
            lineno = int(float(ranges[index].string))
            end = int(float(ranges[index + 1].string))
            while lineno < end:
                lines.append(lineno)
                lineno += 1
        return lines

    def _close(self):
        """Forget the breakpoints of the file (in the debugger)."""
        self.clear_file_breaks()
        EditorWindow._close(self)


class PyShellFileList(FileList):
    """The file list of IDLE with a shell."""

    EditorWindow = PyShellEditorWindow
    pyshell = None

    def open_shell(self, event=None):
        """Show the shell, creating and starting it if needed."""
        if self.pyshell:
            self.pyshell.top.wakeup()
        else:
            self.pyshell = PyShell(self)
            if self.pyshell:
                if not self.pyshell.begin():
                    return None
        return self.pyshell


class ModifiedColorDelegator(ColorDelegator):
    """The shell's colorizer: output is not colorized, and neither is
    the text typed in reply to input()."""

    def __init__(self):
        ColorDelegator.__init__(self)
        self.reading = False  # set while the shell reads for input()

    def recolorize_main(self):
        # Everything before the input area is output (or old input).
        self.tag_remove("TODO", "1.0", "iomark")
        self.tag_add("SYNC", "1.0", "iomark")
        if self.reading:
            self.tag_remove("TODO", "iomark", "end")
        ColorDelegator.recolorize_main(self)

    def config_colors(self):
        ColorDelegator.config_colors(self)
        theme = idleConf.CurrentTheme()
        for tag in ("stdout", "stderr", "console"):
            self.tag_configure(tag, **idleConf.GetHighlight(theme, tag))
        self.tag_raise('sel')


class ModifiedUndoDelegator(UndoDelegator):
    """Output and old input cannot be changed (only the input area)."""

    def insert(self, index, chars, tags=None):
        try:
            if self.delegate.compare(index, "<", "iomark"):
                self.delegate.bell()
                return
        except TclError:
            pass
        UndoDelegator.insert(self, index, chars, tags)

    def delete(self, index1, index2=None):
        try:
            if self.delegate.compare(index1, "<", "iomark"):
                self.delegate.bell()
                return
        except TclError:
            pass
        UndoDelegator.delete(self, index1, index2)

    def undo_event(self, event):
        # Undo would make the text before iomark editable; keep the
        # boundary where it is.
        orig_insert = self.delegate.insert
        self.delegate.insert = (
            lambda index, chars, tags=None:
                orig_insert(index, chars, tags or "stdin"))
        try:
            super().undo_event(event)
        finally:
            self.delegate.insert = orig_insert


class UserInputTaggingDelegator(Delegator):
    """Typed text gets the 'stdin' tag."""

    def insert(self, index, chars, tags=None):
        if tags is None:
            tags = "stdin"
        self.delegate.insert(index, chars, tags)


class MyRPCClient(rpc.RPCClient):

    def handle_EOF(self):
        """The subprocess is gone (restart or crash)."""
        raise EOFError


class ModifiedInterpreter(InteractiveInterpreter):
    """Compile the shell's input here and run it in the subprocess (or
    here, without one)."""

    def __init__(self, tkconsole):
        self.tkconsole = tkconsole
        locals = sys.modules['__main__'].__dict__
        InteractiveInterpreter.__init__(self, locals=locals)
        self.restarting = False
        self.subprocess_arglist = None
        self.port = PORT
        self.original_compiler_flags = self.compile.compiler.flags

    _afterid = None
    rpcclt = None
    rpcsubproc = None
    debugger = None
    active_seq = None
    gid = 0

    def spawn_subprocess(self):
        if self.subprocess_arglist is None:
            self.subprocess_arglist = self.build_subprocess_arglist()
        self.rpcsubproc = subprocess.Popen(self.subprocess_arglist)

    def build_subprocess_arglist(self):
        """The command starting the user process: our interpreter, -P
        (the current directory is added later), the warning options and
        the port to connect to."""
        assert self.port != 0, (
            "Socket should have been assigned a port number.")
        w = ['-W' + s for s in sys.warnoptions]
        del_exitf = idleConf.GetOption('main', 'General', 'delete-exitfunc',
                                       default=False, type='bool')
        command = f"__import__('idlelib.run').run.main({del_exitf!r})"
        return [sys.executable, '-P'] + w + ["-c", command, str(self.port)]

    def start_subprocess(self):
        addr = (HOST, self.port)
        # The port may be busy for a moment after a restart.
        for i in range(3):
            time.sleep(i)
            try:
                self.rpcclt = MyRPCClient(addr)
                break
            except OSError:
                pass
        else:
            self.display_port_binding_error()
            return None
        self.port = self.rpcclt.listening_sock.getsockname()[1]
        if PORT != 0:
            self.rpcclt.listening_sock.setsockopt(socket.SOL_SOCKET,
                                                  socket.SO_REUSEADDR, 1)
        self.spawn_subprocess()
        self.rpcclt.listening_sock.settimeout(10)
        try:
            self.rpcclt.accept()
        except TimeoutError:
            self.display_no_subprocess_error()
            return None
        self.rpcclt.register("console", self.tkconsole)
        self.rpcclt.register("stdin", self.tkconsole.stdin)
        self.rpcclt.register("stdout", self.tkconsole.stdout)
        self.rpcclt.register("stderr", self.tkconsole.stderr)
        self.rpcclt.register("flist", self.tkconsole.flist)
        self.rpcclt.register("linecache", linecache)
        self.rpcclt.register("interp", self)
        self.transfer_path(with_cwd=True)
        self.poll_subprocess()
        return self.rpcclt

    def restart_subprocess(self, with_cwd=False, filename=''):
        """Start a fresh user process (Shell > Restart, Run Module)."""
        if self.restarting:
            return self.rpcclt
        self.restarting = True
        debug = self.getdebugger()
        if debug:
            try:
                debugger_r.close_subprocess_debugger(self.rpcclt)
            except Exception:
                pass
        self.rpcclt.close()
        self.terminate_subprocess()
        console = self.tkconsole
        was_executing = console.executing
        console.executing = False
        self.spawn_subprocess()
        try:
            self.rpcclt.accept()
        except TimeoutError:
            self.display_no_subprocess_error()
            return None
        self.transfer_path(with_cwd=with_cwd)
        console.stop_readline()
        # Drop the pending input; mark the restart in the text.
        console.text.delete("iomark", "end-1c")
        console.write('\n')
        console.write(restart_line(console.width, filename))
        console.text.mark_set("restart", "end-1c")
        console.text.mark_gravity("restart", "left")
        if not filename:
            console.showprompt()
        if debug:
            debugger_r.restart_subprocess_debugger(self.rpcclt)
            debug.load_breakpoints()
        self.compile.compiler.flags = self.original_compiler_flags
        self.restarting = False
        return self.rpcclt

    def __request_interrupt(self):
        self.rpcclt.remotecall("exec", "interrupt_the_server", (), {})

    def interrupt_subprocess(self):
        threading.Thread(target=self.__request_interrupt).start()

    def kill_subprocess(self):
        if self._afterid is not None:
            self.tkconsole.text.after_cancel(self._afterid)
        try:
            self.rpcclt.listening_sock.close()
        except AttributeError:  # no socket
            pass
        try:
            self.rpcclt.close()
        except AttributeError:  # no client
            pass
        self.terminate_subprocess()
        self.tkconsole.executing = False
        self.rpcclt = None

    def terminate_subprocess(self):
        """End the user process (and reap it)."""
        if self.rpcsubproc is None:
            return
        try:
            self.rpcsubproc.kill()
        except OSError:
            return  # already gone
        try:
            self.rpcsubproc.wait()
        except OSError:
            return

    def transfer_path(self, with_cwd=False):
        """Give the user process our sys.path (and the current
        directory, at startup)."""
        if with_cwd:
            path = ['']
            path.extend(sys.path)
        else:
            path = sys.path
        self.runcommand(
            "if 1:\n"
            "    import sys as _idle_sys\n"
            f"    _idle_sys.path = {fix_user_path(path)!r}\n"
            "    del _idle_sys\n"
            "\n")

    def poll_subprocess(self):
        """Check for the result of the running code (every pollinterval
        milliseconds)."""
        clt = self.rpcclt
        if clt is None:
            return
        try:
            response = clt.pollresponse(self.active_seq, wait=0.05)
        except (EOFError, OSError, KeyboardInterrupt):
            # The process died (or is restarting).
            if self.tkconsole.closing:
                return
            response = None
            self.restart_subprocess()
        if response:
            self.tkconsole.resetoutput()
            self.active_seq = None
            how, what = response
            console = self.tkconsole.console
            if how == "OK":
                if what is not None:
                    print(repr(what), file=console)
            elif how == "EXCEPTION":
                if self.tkconsole.getvar("<<toggle-jit-stack-viewer>>"):
                    self.remote_stack_viewer()
            elif how == "ERROR":
                errmsg = "pyshell.ModifiedInterpreter: Subprocess ERROR:\n"
                print(errmsg, what, file=sys.__stderr__)
                print(errmsg, what, file=console)
            try:
                self.tkconsole.endexecuting()
            except AttributeError:  # shell closed
                pass
        if not self.tkconsole.closing:
            self._afterid = self.tkconsole.text.after(
                self.tkconsole.pollinterval, self.poll_subprocess)

    def setdebugger(self, debugger):
        self.debugger = debugger

    def getdebugger(self):
        return self.debugger

    def open_remote_stack_viewer(self):
        """Called by the user process after an exception, when the stack
        viewer should open (it must not run inside the rpc call)."""
        self.tkconsole.text.after(300, self.remote_stack_viewer)

    def remote_stack_viewer(self):
        from idlelib import debugobj_r
        oid = self.rpcclt.remotequeue("exec", "stackviewer", ("flist",), {})
        if oid is None:
            self.tkconsole.root.bell()
            return
        item = debugobj_r.StubObjectTreeItem(self.rpcclt, oid)
        from idlelib.tree import ScrolledCanvas, TreeNode
        top = Toplevel(self.tkconsole.root)
        theme = idleConf.CurrentTheme()
        background = idleConf.GetHighlight(theme, 'normal')['background']
        sc = ScrolledCanvas(top, bg=background, highlightthickness=0)
        sc.frame.pack(expand=1, fill="both")
        node = TreeNode(sc.canvas, None, item)
        node.expand()

    def execsource(self, source):
        """Run *source* as a script."""
        filename = self.stuffsource(source)
        self.execfile(filename, source)

    def execfile(self, filename, source=None):
        """Run the file *filename* (or *source* under that name)."""
        if source is None:
            with tokenize.open(filename) as fp:
                source = fp.read()
                if use_subprocess:
                    source = (f"__file__ = {os.path.abspath(filename)!r}\n"
                              + source + "\ndel __file__")
        try:
            code = compile(source, filename, "exec")
        except (OverflowError, SyntaxError):
            self.tkconsole.resetoutput()
            print('The script or command has a syntax error.\n'
                  'Traceback (most recent call last):',
                  file=self.tkconsole.stderr)
            InteractiveInterpreter.showsyntaxerror(self, filename)
            self.tkconsole.showprompt()
        else:
            self.runcode(code)

    def runsource(self, source):
        """Compile and run *source*; return True if it is incomplete."""
        filename = self.stuffsource(source)
        # Syntax warnings are errors at the prompt (they would be shown
        # again at every keystroke otherwise).
        saved_filters = warnings.filters[:]
        warnings.filterwarnings(action="error", category=SyntaxWarning)
        try:
            return InteractiveInterpreter.runsource(self, source, filename)
        finally:
            warnings.filters[:] = saved_filters

    def stuffsource(self, source):
        """Put *source* in linecache under a name of its own; return it."""
        filename = f"<pyshell#{self.gid}>"
        self.gid += 1
        lines = source.split("\n")
        linecache.cache[filename] = len(source) + 1, 0, lines, filename
        return filename

    def prepend_syspath(self, filename):
        """Put the directory of *filename* first on the user's sys.path."""
        self.runcommand(
            "if 1:\n"
            f"    _idle_dir = __import__('os').path.dirname({filename!r})\n"
            "    import sys as _idle_sys\n"
            "    if _idle_dir not in _idle_sys.path:\n"
            "        _idle_sys.path.insert(0, _idle_dir)\n"
            "    del _idle_dir, _idle_sys\n"
            "\n")

    def showsyntaxerror(self, filename=None, **kwargs):
        """Mark the error in the input; show the message."""
        tkconsole = self.tkconsole
        text = tkconsole.text
        text.tag_remove("ERROR", "1.0", "end")
        type, value, tb = sys.exc_info()
        msg = getattr(value, 'msg', '') or value or "<no detail available>"
        lineno = getattr(value, 'lineno', '') or 1
        offset = getattr(value, 'offset', '') or 0
        if offset == 0:
            lineno += 1  # mark the end of the line
        if lineno == 1:
            pos = f"iomark + {offset - 1} chars"
        else:
            pos = f"iomark linestart + {lineno - 1} lines + {offset - 1} chars"
        tkconsole.colorize_syntax_error(text, pos)
        tkconsole.resetoutput()
        self.write(f"SyntaxError: {msg}\n")
        tkconsole.showprompt()

    def showtraceback(self):
        """Show the traceback (of code run in this process)."""
        self.tkconsole.resetoutput()
        self.checklinecache()
        InteractiveInterpreter.showtraceback(self)
        if self.tkconsole.getvar("<<toggle-jit-stack-viewer>>"):
            self.tkconsole.open_stack_viewer()

    def checklinecache(self):
        """Keep only the shell's entries in linecache."""
        cache = linecache.cache
        for key in list(cache):
            if key[:1] + key[-1:] != "<>":
                del cache[key]

    def runcommand(self, code):
        """Run *code* (a string) without showing it in the shell."""
        if self.tkconsole.executing:
            self.display_executing_dialog()
            return 0
        if self.rpcclt:
            self.rpcclt.remotequeue("exec", "runcode", (code,), {})
        else:
            exec(code, self.locals)
        return 1

    def runcode(self, code):
        """Run a code object (asynchronously in the user process)."""
        if self.tkconsole.executing:
            self.restart_subprocess()
        self.checklinecache()
        debugger = self.debugger
        try:
            self.tkconsole.beginexecuting()
            if not debugger and self.rpcclt is not None:
                self.active_seq = self.rpcclt.asyncqueue("exec", "runcode",
                                                         (code,), {})
            elif debugger:
                debugger.run(code, self.locals)
            else:
                exec(code, self.locals)
        except SystemExit:
            if not self.tkconsole.closing:
                if messagebox.askyesno(
                        "Quit IDLE?",
                        "The code called exit().  Quit IDLE as well?",
                        default="yes",
                        parent=self.tkconsole.text):
                    raise
                else:
                    self.showtraceback()
            else:
                raise
        except BaseException:
            if use_subprocess:
                print("IDLE could not start the code:",
                      file=self.tkconsole.stderr)
                self.showtraceback()
                self.tkconsole.endexecuting()
            else:
                if self.tkconsole.canceled:
                    self.tkconsole.canceled = False
                    print("KeyboardInterrupt", file=self.tkconsole.stderr)
                else:
                    self.showtraceback()
        finally:
            if not use_subprocess:
                try:
                    self.tkconsole.endexecuting()
                except AttributeError:  # shell closed
                    pass

    def write(self, s):
        """Errors of the interpreter go to the shell's stderr."""
        return self.tkconsole.stderr.write(s)

    def display_port_binding_error(self):
        messagebox.showerror(
            "Port Binding Error",
            "No local TCP port could be opened for the connection to the "
            "process running user code.\n\n"
            "Start IDLE with -n to run code without that process "
            "(see IDLE Help).",
            parent=self.tkconsole.text)

    def display_no_subprocess_error(self):
        messagebox.showerror(
            "Subprocess Connection Error",
            "The process for running user code did not connect to IDLE.\n"
            "IDLE Help ('Startup failure') lists common causes.",
            parent=self.tkconsole.text)

    def display_executing_dialog(self):
        messagebox.showerror(
            "Already executing",
            "The shell is busy running code.  Wait for it to finish (or "
            "interrupt it) first.",
            parent=self.tkconsole.text)


class PyShell(OutputWindow):
    """The shell window."""

    from idlelib.squeezer import Squeezer

    shell_title = "IDLE Shell " + python_version()

    # The colorizer and undo delegator of the shell.
    ColorDelegator = ModifiedColorDelegator
    UndoDelegator = ModifiedUndoDelegator

    menu_specs = [
        ("file", "_File"),
        ("edit", "_Edit"),
        ("debug", "_Debug"),
        ("options", "_Options"),
        ("window", "_Window"),
        ("help", "_Help"),
    ]

    rmenu_specs = [
        ("Cut", "<<cut>>", "rmenu_check_cut"),
        ("Copy", "<<copy>>", "rmenu_check_copy"),
        ("Copy with prompts", "<<copy-with-prompts>>", "rmenu_check_copy"),
        ("Paste", "<<paste>>", "rmenu_check_paste"),
        (None, None, None),
        ("Squeeze", "<<squeeze-current-text>>", None),
    ]

    allow_line_numbers = False
    user_input_insert_tags = "stdin"

    from idlelib.history import History
    from idlelib.sidebar import ShellSidebar

    COPYRIGHT = \
        'Type "help", "copyright", "credits" or "license()" for more information.'

    def __init__(self, flist=None):
        if use_subprocess:
            ms = self.menu_specs
            if ms[2][0] != "shell":
                ms.insert(2, ("shell", "She_ll"))
        self.interp = ModifiedInterpreter(self)
        if flist is None:
            root = Tk()
            fixwordbreaks(root)
            root.withdraw()
            flist = PyShellFileList(root)
        self.shell_sidebar = None  # created after the text
        self._output_newline_added = False

        OutputWindow.__init__(self, flist, None, None)

        self.usetabs = False
        # Indent with 4 spaces regardless of the configuration.
        self.indentwidth = 4

        self.sys_ps1 = sys.ps1 if hasattr(sys, 'ps1') else '>>>'
        self.prompt_last_line = self.sys_ps1.split('\n')[-1]
        self.prompt = self.sys_ps1  # changes when debug active

        text = self.text
        text.configure(wrap="char")
        text.bind("<<newline-and-indent>>", self.enter_callback)
        text.bind("<<plain-newline-and-indent>>", self.linefeed_callback)
        text.bind("<<interrupt-execution>>", self.cancel_callback)
        text.bind("<<end-of-file>>", self.eof_callback)
        text.bind("<<open-stack-viewer>>", self.open_stack_viewer)
        text.bind("<<toggle-debugger>>", self.toggle_debugger)
        text.bind("<<toggle-jit-stack-viewer>>", self.toggle_jit_stack_viewer)
        text.bind("<<copy-with-prompts>>", self.copy_with_prompts_callback)
        if use_subprocess:
            text.bind("<<view-restart>>", self.view_restart_mark)
            text.bind("<<restart-shell>>", self.restart_shell)
        self.squeezer = self.Squeezer(self)
        text.bind("<<squeeze-current-text>>", self.squeeze_current_text_event)

        self.save_stdout = sys.stdout
        self.save_stderr = sys.stderr
        self.save_stdin = sys.stdin
        from idlelib import iomenu
        self.stdin = StdInputFile(self, "stdin",
                                  iomenu.encoding, iomenu.errors)
        self.stdout = StdOutputFile(self, "stdout",
                                    iomenu.encoding, iomenu.errors)
        self.stderr = StdOutputFile(self, "stderr",
                                    iomenu.encoding, "backslashreplace")
        self.console = StdOutputFile(self, "console",
                                     iomenu.encoding, iomenu.errors)
        if not use_subprocess:
            sys.stdout = self.stdout
            sys.stderr = self.stderr
            sys.stdin = self.stdin
        try:
            # help() output goes to the shell, without a pager.
            import pydoc
            pydoc.pager = pydoc.plainpager
        except Exception:
            sys.stderr = sys.__stderr__
            raise

        self.history = self.History(self.text)
        self.pollinterval = 50  # milliseconds

        self.shell_sidebar = self.ShellSidebar(self)

    def ResetFont(self):
        super().ResetFont()
        if self.shell_sidebar is not None:
            self.shell_sidebar.update_font()

    def ResetColorizer(self):
        super().ResetColorizer()
        theme = idleConf.CurrentTheme()
        tag_colors = {
            "stdin": {'background': None, 'foreground': None},
            "stdout": idleConf.GetHighlight(theme, "stdout"),
            "stderr": idleConf.GetHighlight(theme, "stderr"),
            "console": idleConf.GetHighlight(theme, "normal"),
        }
        for tag, tag_colors_config in tag_colors.items():
            self.text.tag_configure(tag, **tag_colors_config)
        if self.shell_sidebar is not None:
            self.shell_sidebar.update_colors()

    def get_standard_extension_names(self):
        return idleConf.GetExtensions(shell_only=True)

    def get_prompt_text(self, first, last):
        """Return the text between *first* and *last* with the prompts
        of the input lines (for saving the shell)."""
        text = self.text
        lines = text.get(first, last).split('\n')
        first_line = int(float(text.index(first)))
        prompts = self.shell_sidebar.line_prompts
        result = []
        for lineno, line in enumerate(lines, first_line):
            prompt = prompts.get(lineno)
            result.append(f"{prompt} {line}" if prompt else line)
        return '\n'.join(result)

    def copy_with_prompts_callback(self, event=None):
        """Copy the selected lines with their prompts."""
        text = self.text
        try:
            first = text.index("sel.first linestart")
            last = text.index("sel.last")
        except TclError:
            return None
        copied = self.get_prompt_text(first, last)
        if text.get(last + " -1c") == '\n' or copied.endswith('\n'):
            pass
        if not copied.endswith('\n'):
            copied += '\n'
        text.clipboard_clear()
        text.clipboard_append(copied)
        return "break"

    reading = False
    executing = False
    canceled = False
    endoffile = False
    closing = False
    _stop_readline_flag = False

    def set_warning_stream(self, stream):
        global warning_stream
        warning_stream = stream

    def get_warning_stream(self):
        return warning_stream

    def toggle_debugger(self, event=None):
        if self.executing:
            messagebox.showerror("Debugger",
                                 "The debugger cannot be switched on or off "
                                 "while code is running.",
                                 parent=self.text)
            self.set_debugger_indicator()
            return "break"
        db = self.interp.getdebugger()
        if db:
            self.close_debugger()
        else:
            self.open_debugger()

    def set_debugger_indicator(self):
        db = self.interp.getdebugger()
        self.setvar("<<toggle-debugger>>", bool(db))

    def toggle_jit_stack_viewer(self, event=None):
        pass  # the menu checkbutton holds the state

    def close_debugger(self):
        db = self.interp.getdebugger()
        if db:
            self.interp.setdebugger(None)
            db.close()
            if self.interp.rpcclt:
                debugger_r.close_remote_debugger(self.interp.rpcclt)
            self.resetoutput()
            self.console.write("[DEBUG OFF]\n")
            self.prompt = self.sys_ps1
            self.showprompt()
        self.set_debugger_indicator()

    def open_debugger(self):
        if self.interp.rpcclt:
            dbg_gui = debugger_r.start_remote_debugger(self.interp.rpcclt,
                                                       self)
        else:
            dbg_gui = debugger.Debugger(self)
        self.interp.setdebugger(dbg_gui)
        dbg_gui.load_breakpoints()
        self.prompt = "[DEBUG ON]\n" + self.sys_ps1
        self.showprompt()
        self.set_debugger_indicator()

    def debug_menu_postcommand(self):
        state = 'disabled' if self.executing else 'normal'
        self.update_menu_state('debug', '*tack*iewer', state)

    def beginexecuting(self):
        """User code starts running."""
        self.resetoutput()
        self.executing = True

    def endexecuting(self):
        """User code ended: show a prompt."""
        self.executing = False
        self.canceled = False
        self.showprompt()

    def close(self):
        """Close the shell (asking first while code is running)."""
        if self.executing:
            response = messagebox.askokcancel(
                "Stop running code?",
                "Code is still running in the shell.\n"
                "Stop it and close the shell?",
                default="ok",
                parent=self.text)
            if response is False:
                return "cancel"
        self.stop_readline()
        self.canceled = True
        self.closing = True
        return EditorWindow.close(self)

    def _close(self):
        self.close_debugger()
        if use_subprocess:
            self.interp.kill_subprocess()
        sys.stdout = self.save_stdout
        sys.stderr = self.save_stderr
        sys.stdin = self.save_stdin
        self.interp = None
        self.console = None
        self.flist.pyshell = None
        self.history = None
        EditorWindow._close(self)

    def ispythonsource(self, filename):
        """The shell's input is Python code."""
        return True

    def short_title(self):
        return self.shell_title

    def begin(self):
        """Write the banner and the first prompt (starting the user
        process).  Return False if that fails."""
        self.text.mark_set("iomark", "insert")
        self.resetoutput()
        if use_subprocess:
            nosub = ''
            client = self.interp.start_subprocess()
            if not client:
                self.close()
                return False
        else:
            nosub = ("==== No Subprocess ====\n\n"
                     "Note: user code runs in the IDLE process (-n).  This "
                     "mode is deprecated;\nsee IDLE Help.\n\n")
            sys.displayhook = rpc.displayhook

        self.write(f"Python {sys.version} on {sys.platform}\n"
                   f"{self.COPYRIGHT}\n{nosub}")
        self.text.focus_force()
        self.showprompt()
        # User code should create its own root window.
        import tkinter
        tkinter._default_root = None
        return True

    def stop_readline(self):
        if not self.reading:
            return
        self._stop_readline_flag = True
        self.top.quit()

    def readline(self):
        """Read a line typed in the shell (for input() and sys.stdin)."""
        save = self.reading
        try:
            self.reading = True
            if self.color:
                self.color.reading = True
            self.top.mainloop()  # until enter_callback() quits
        finally:
            self.reading = save
            if self.color:
                self.color.reading = save
        if self._stop_readline_flag:
            self._stop_readline_flag = False
            return ""
        line = self.text.get("iomark", "end-1c")
        if len(line) == 0:  # can be an empty string after a cancel
            line = "\n"
        self.resetoutput()
        if self.canceled:
            self.canceled = False
            if not use_subprocess:
                raise KeyboardInterrupt
        if self.endoffile:
            self.endoffile = False
            line = ""
        return line

    def isatty(self):
        return True

    def cancel_callback(self, event=None):
        """Control-C: interrupt the running code (or drop the input)."""
        try:
            if self.text.compare("sel.first", "!=", "sel.last"):
                return  # with a selection, Control-C copies
        except TclError:
            pass
        if not (self.executing or self.reading):
            self.resetoutput()
            self.interp.write("KeyboardInterrupt\n")
            self.showprompt()
            return "break"
        self.endoffile = False
        self.canceled = True
        if self.executing and self.interp.rpcclt:
            if self.interp.getdebugger():
                self.interp.restart_subprocess()
            else:
                self.interp.interrupt_subprocess()
        if self.reading:
            self.top.quit()  # exit the nested mainloop of readline()
        return "break"

    def eof_callback(self, event):
        """Control-D at the end of an empty input: close or end input."""
        if self.executing and not self.reading:
            return None  # let the default binding delete a character
        if not (self.text.compare("iomark", "==", "insert") and
                self.text.compare("insert", "==", "end-1c")):
            return None
        if not self.executing:
            self.resetoutput()
            self.close()
        else:
            self.canceled = False
            self.endoffile = True
            self.top.quit()
        return "break"

    def linefeed_callback(self, event):
        """Insert a newline without running the input."""
        if self.reading:
            self.text.insert("insert", "\n")
            self.text.see("insert")
        else:
            self.newline_and_indent_event(event)
        return "break"

    def enter_callback(self, event):
        """Return: run the input (or recall old input at the cursor)."""
        if self.executing and not self.reading:
            return None  # let the default binding insert a newline
        text = self.text
        # A selection in old input or output is recalled.
        try:
            sel = text.get("sel.first", "sel.last")
            if sel and text.compare("sel.last", "<=", "iomark"):
                self.recall(sel, event)
                return "break"
        except TclError:
            pass
        # The cursor in old input: recall that statement.
        if text.compare("insert", "<", "iomark linestart"):
            prev = text.tag_prevrange("stdin", "insert")
            if prev and text.compare("insert", "<", prev[1]):
                self.recall(text.get(prev[0], prev[1]), event)
                return "break"
            next = text.tag_nextrange("stdin", "insert")
            if next and text.compare("insert lineend", ">=", next[0]):
                self.recall(text.get(next[0], next[1]), event)
                return "break"
            # Elsewhere: the line itself.
            line = text.get("insert linestart", "insert lineend")
            self.recall(line, event)
            return "break"
        # In the prompt line before the input: go to the input.
        if text.compare("insert", "<", "iomark"):
            text.mark_set("insert", "iomark")
        # Blanks after the cursor at the end of the input are dropped.
        s = text.get("insert", "end-1c")
        if s and not s.strip():
            text.delete("insert", "end-1c")
        # Inside the input: just a new line.
        if text.compare("insert", "<", "end-1c linestart"):
            self.newline_and_indent_event(event)
            return "break"
        # At the end: run (or continue a compound statement).
        text.mark_set("insert", "end-1c")
        if self.reading:
            text.insert("insert", "\n")
            text.see("insert")
        else:
            self.newline_and_indent_event(event)
        text.update_idletasks()
        if self.reading:
            self.top.quit()  # readline() returns the line
        else:
            self.runit()
        return "break"

    def recall(self, s, event):
        """Copy old input *s* to the input area (reindented)."""
        s = re.sub(r'^\s*\n', '', s)
        s = re.sub(r'\n\s*$', '', s)
        lines = s.split('\n')
        text = self.text
        text.undo_block_start()
        try:
            text.tag_remove("sel", "1.0", "end")
            text.mark_set("insert", "end-1c")
            prefix = text.get("insert linestart", "insert")
            if prefix.rstrip().endswith(':'):
                self.newline_and_indent_event(event)
                prefix = text.get("insert linestart", "insert")
            text.insert("insert", lines[0].strip(),
                        self.user_input_insert_tags)
            if len(lines) > 1:
                orig_base_indent = re.search(r'^([ \t]*)', lines[0]).group(0)
                new_base_indent = re.search(r'^([ \t]*)', prefix).group(0)
                for line in lines[1:]:
                    if line.startswith(orig_base_indent):
                        # Replace the old base indent by the new one.
                        line = new_base_indent + line[len(orig_base_indent):]
                    text.insert('insert', '\n' + line.rstrip(),
                                self.user_input_insert_tags)
        finally:
            text.see("insert")
            text.undo_block_stop()

    # The last newline (and blanks around it) is not part of the source.
    _last_newline_re = re.compile(r"[ \t]*(\n[ \t]*)?\Z")

    def runit(self):
        """Run the input if it is a complete statement."""
        index_before = self.text.index("end-2c")
        line = self.text.get("iomark", "end-1c")
        line = self._last_newline_re.sub("", line)
        input_is_incomplete = self.interp.runsource(line)
        if not input_is_incomplete:
            # The newline ending the input is not part of it: the line
            # after it is output, not a continuation line.
            if self.text.get(index_before) == '\n':
                self.text.tag_remove(self.user_input_insert_tags,
                                     index_before)
            self.shell_sidebar.update_sidebar()

    def open_stack_viewer(self, event=None):
        """Show the stack of the last exception."""
        if self.interp.rpcclt:
            return self.interp.remote_stack_viewer()
        try:
            sys.last_exc
        except AttributeError:
            messagebox.showerror("Stack Viewer",
                                 "No exception has been raised in the shell"
                                 " yet (sys.last_exc is not set).",
                                 parent=self.text)
            return
        from idlelib.stackviewer import StackBrowser
        StackBrowser(self.root, sys.last_exc, self.flist)

    def view_restart_mark(self, event=None):
        self.text.see("iomark")
        self.text.see("restart")

    def restart_shell(self, event=None):
        """Shell > Restart Shell."""
        self.interp.restart_subprocess(with_cwd=True)

    def showprompt(self):
        """Start a new input area after a prompt."""
        self.resetoutput()
        self._output_newline_added = False
        prompt = self.prompt
        if self.sys_ps1 and prompt.endswith(self.sys_ps1):
            prompt = prompt[:-len(self.sys_ps1)]
        if prompt:
            # Lines of the prompt before the last one (like the
            # debugger's) are written; the sidebar shows the last one.
            self.console.write(prompt if prompt.endswith('\n')
                               else prompt + '\n')
        self.text.mark_set("insert", "end-1c")
        self.set_line_and_column()
        self.io.reset_undo()
        # The newline before the input area marks the prompt line.
        self.text.tag_add("console", "iomark-1c")
        if self.shell_sidebar is not None:
            self.shell_sidebar.update_sidebar()

    def show_warning(self, msg):
        width = self.interp.tkconsole.width
        wrapper = __import__('textwrap').TextWrapper(width=width, tabsize=8,
                                                     expand_tabs=True)
        wrapped_msg = '\n'.join(wrapper.wrap(msg))
        if not wrapped_msg.endswith('\n'):
            wrapped_msg += '\n'
        self.per.bottom.insert("iomark linestart", wrapped_msg, "stderr")

    def resetoutput(self):
        """Make the input area start on a new line at the end."""
        source = self.text.get("iomark", "end-1c")
        if self.history:
            self.history.store(source)
        if self.text.get("end-2c") != "\n":
            self.text.insert("end-1c", "\n")
        self.text.mark_set("iomark", "end-1c")
        self.set_line_and_column()
        self.ctip.remove_calltip_window()

    def write(self, s, tags=()):
        """Write output; while a prompt is shown (output of a thread, or
        of a warning), the output goes before the prompt line."""
        text = self.text
        at_prompt = (not self.executing and
                     'console' in text.tag_names("iomark-1c") and
                     text.compare("iomark", ">", "1.0"))
        try:
            text.mark_gravity("iomark", "right")
            if not at_prompt:
                count = OutputWindow.write(self, s, tags, "iomark")
            else:
                count = self._write_before_prompt(s, tags)
            text.mark_gravity("iomark", "left")
        except BaseException:
            raise  # the shell is closing or the text is gone
        if self.canceled:
            self.canceled = False
            if not use_subprocess:
                raise KeyboardInterrupt
        if at_prompt and self.shell_sidebar is not None:
            self.shell_sidebar.update_sidebar()
        return count

    def _write_before_prompt(self, s, tags):
        """Insert *s* after the output and before the prompt line.

        The newline before the prompt line has the 'console' tag.  Output
        not ending with a newline gets one (added here) so that the
        prompt line stays separate; the next output goes before that
        added newline.
        """
        text = self.text
        if self._output_newline_added:
            # Continue the output on its last line.
            text.insert("iomark-1c", s, tags)
            if s.endswith('\n'):
                text.delete("iomark-1c")
                self._output_newline_added = False
                text.tag_add("console", "iomark-1c")
        else:
            text.tag_remove("console", "iomark-1c")
            text.insert("iomark", s, tags)
            if not s.endswith('\n'):
                text.insert("iomark", '\n', tags)
                self._output_newline_added = True
            text.tag_add("console", "iomark-1c")
        text.see("iomark")
        text.update()
        return len(s)

    def rmenu_check_cut(self):
        try:
            if self.text.compare('sel.first', '<', 'iomark'):
                return 'disabled'
        except TclError:  # no selection
            return 'disabled'
        return super().rmenu_check_cut()

    def rmenu_check_paste(self):
        if self.text.compare('insert', '<', 'iomark'):
            return 'disabled'
        return super().rmenu_check_paste()

    def squeeze_current_text_event(self, event=None):
        self.squeezer.squeeze_current_text()
        self.shell_sidebar.update_sidebar()

    def on_squeezed_expand(self, index, text, tags):
        self.shell_sidebar.update_sidebar()



usage_msg = """\

Usage: idle [options] [file ...]
       idle [options] (-c command | -r script | -) [arg ...]

Options:
  -h          show this help and exit
  -e          open an editor window (for each file given)
  -i          open the shell window
  -n          run user code in the IDLE process, without a separate
              subprocess (deprecated)
  -c command  run the command in the shell
  -r script   run the script in the shell
  -           run the code read from standard input in the shell
  -d          start the debugger
  -s          first run the file named by $IDLESTARTUP or $PYTHONSTARTUP
  -t title    use this title for the shell window

-c, -r, -, -d, -s and -t open the shell.  With -c, -r or -, the args
are given to the code in sys.argv[1:].  Without -e and -i, the IDLE
settings choose whether the shell or an editor window is opened.
"""


def _parse_command_line(argv):
    """Return the options of the command line as a dict (or exit)."""
    try:
        opts, args = getopt.getopt(argv, "c:deihnr:st:")
    except getopt.error as msg:
        print(f"Error: {msg}\n{usage_msg}", file=sys.stderr)
        sys.exit(2)
    options = dict(cmd=None, script=None, debug=False, edit=False,
                   shell=False, startup=False, subprocess=True, args=args)
    for opt, value in opts:
        if opt == '-h':
            sys.stdout.write(usage_msg)
            sys.exit()
        elif opt == '-c':
            options['cmd'] = value
        elif opt == '-r':
            if not os.path.isfile(value):
                print("No script file: ", value)
                sys.exit()
            options['script'] = value
        elif opt == '-d':
            options['debug'] = True
        elif opt == '-e':
            options['edit'] = True
        elif opt == '-i':
            options['shell'] = True
        elif opt == '-n':
            print(" Warning: running IDLE without a subprocess is "
                  "deprecated.", file=sys.stderr)
            options['subprocess'] = False
        elif opt == '-s':
            options['startup'] = True
        elif opt == '-t':
            PyShell.shell_title = value
            options['shell'] = True
        if opt in ('-c', '-r', '-d', '-s'):
            options['shell'] = True
    if args and args[0] == '-':
        options['cmd'] = sys.stdin.read()
        options['shell'] = True
    return options


def _set_window_icons(root):
    from platform import system
    from idlelib import macosx
    icondir = os.path.join(os.path.dirname(__file__), 'Icons')
    if system() == 'Windows':
        iconfile = os.path.join(icondir, 'idle.ico')
        if os.path.exists(iconfile):
            root.wm_iconbitmap(default=iconfile)
        return
    if macosx.isAquaTk():
        return  # the application bundle has the icon
    ext, sizes = (('.png', (16, 32, 48, 256)) if TkVersion >= 8.6
                  else ('.gif', (16, 32, 48)))
    files = [os.path.join(icondir, f'idle_{size}{ext}') for size in sizes]
    icons = [PhotoImage(master=root, file=file)
             for file in files if os.path.exists(file)]
    if icons:
        root.wm_iconphoto(True, *icons)


def main():
    """Start IDLE (see usage_msg)."""
    from idlelib import testing
    from idlelib import macosx
    from idlelib.run import fix_scaling

    global flist, root, use_subprocess

    options = _parse_command_line(sys.argv[1:])
    use_subprocess = options['subprocess']
    cmd, script, args = options['cmd'], options['script'], options['args']

    # sys.argv and sys.path as user code expects them.
    sys.path[:] = [os.path.abspath(p) for p in sys.path]
    if args and args[0] == '-':
        sys.argv = [''] + args[1:]
    elif cmd:
        sys.argv = ['-c'] + args
    elif script:
        sys.argv = [script] + args
    elif args:
        options['edit'] = True
        for filename in args:
            folder = os.path.abspath(os.path.dirname(filename))
            if folder not in sys.path:
                sys.path.insert(0, folder)
    elif os.getcwd() not in sys.path:
        sys.path.insert(0, os.getcwd())

    edit = options['edit'] or idleConf.GetOption(
        'main', 'General', 'editor-on-startup', type='bool')
    shell_wanted = options['shell'] or not edit

    capture_warnings(True)
    if use_subprocess and not testing:
        NoDefaultRoot()
    root = Tk(className="Idle")
    root.withdraw()
    fix_scaling(root)
    _set_window_icons(root)
    fixwordbreaks(root)
    fix_x11_paste(root)
    flist = PyShellFileList(root)
    macosx.setupApp(root, flist)

    if edit and not (cmd or script):
        opened = [name for name in args if flist.open(name) is not None]
        if not opened:
            flist.new()

    shell = flist.pyshell
    if shell_wanted:
        shell = flist.open_shell()
        if not shell:
            return  # the user process could not be started
        if macosx.isAquaTk() and flist.dict:
            shell.top.lower()  # the editor windows stay in front

    if options['debug']:
        shell.open_debugger()
    if options['startup']:
        startup_file = (os.environ.get("IDLESTARTUP") or
                        os.environ.get("PYTHONSTARTUP"))
        if startup_file and os.path.isfile(startup_file):
            shell.interp.execfile(startup_file)
    if cmd or script:
        shell.interp.runcommand(
            "if 1:\n"
            "    import sys as _idle_sys\n"
            f"    _idle_sys.argv = {sys.argv!r}\n"
            "    del _idle_sys\n"
            "\n")
        if cmd:
            shell.interp.execsource(cmd)
        else:
            shell.interp.prepend_syspath(script)
            shell.interp.execfile(script)
    elif shell:
        warning = macosx.tkVersionWarning(root)
        if warning:
            shell.show_warning(warning)

    while flist.inversedict:  # until the last window is closed
        root.mainloop()
    root.destroy()
    capture_warnings(False)


if __name__ == "__main__":
    main()

# Importing this module must not leave warnings redirected.
capture_warnings(False)
