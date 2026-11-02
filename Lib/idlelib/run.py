"""The user process of the IDLE shell.

The shell starts "python -m idlelib.run PORT".  This process connects back
to the GUI over rpc, executes the code it receives in __main__ and connects
sys.stdin, sys.stdout and sys.stderr to the shell window.  User code runs
in the main thread; a socket thread serves the GUI's requests, so that a
running program can be interrupted.
"""
import contextlib
import functools
import io
import linecache
import queue
import sys
import textwrap
import time
import traceback
import _thread as thread
import threading
import warnings

import idlelib  # for idlelib.testing
from idlelib import autocomplete
from idlelib import calltip
from idlelib import debugger_r
from idlelib import debugobj_r
from idlelib import iomenu
from idlelib import rpc
from idlelib import stackviewer
import __main__

import tkinter

LOCALHOST = '127.0.0.1'

try:
    # Tell users how to end input in the shell.
    eof = 'Ctrl-D (end-of-file)'
    exit.eof = eof
    quit.eof = eof
except NameError:  # started with -S: no site builtins
    pass


def idle_formatwarning(message, category, filename, lineno, line=None):
    """Return a warning formatted for the shell."""
    text = ["\nWarning (from warnings module):\n",
            f'  File "{filename}", line {lineno}\n']
    if line is None:
        line = linecache.getline(filename, lineno)
    line = line.strip()
    if line:
        text.append(f"    {line}\n")
    text.append(f"{category.__name__}: {message}\n")
    return ''.join(text)


def idle_showwarning_subproc(
        message, category, filename, lineno, file=None, line=None):
    """warnings.showwarning replacement used in the user process."""
    if file is None:
        file = sys.stderr
    try:
        file.write(idle_formatwarning(message, category, filename, lineno, line))
    except OSError:
        pass  # e.g. sys.__stderr__ of a process without console


_warnings_showwarning = None


def capture_warnings(capture):
    """Show warnings in IDLE's format (or restore the original function)."""
    global _warnings_showwarning
    if capture:
        if _warnings_showwarning is None:
            _warnings_showwarning = warnings.showwarning
            warnings.showwarning = idle_showwarning_subproc
    elif _warnings_showwarning is not None:
        warnings.showwarning = _warnings_showwarning
        _warnings_showwarning = None


# State shared by the main thread (user code) and the socket thread.
exit_now = False        # the main thread should exit
quitting = False        # the connection is gone
interruptible = False   # user code is running and may be interrupted
no_exitfunc = False


def main(del_exitfunc=False):
    """Connect to the GUI at port sys.argv[1] and run requests until told
    to stop."""
    global exit_now, quitting, no_exitfunc
    no_exitfunc = del_exitfunc
    try:
        port = int(sys.argv[1]) if sys.argv[1:] else 8833
    except ValueError:
        print("IDLE Subprocess: no IP port passed in sys.argv.",
              file=sys.__stderr__)
        return

    capture_warnings(True)
    sys.argv[:] = [""]
    threading.Thread(target=manage_socket, name='SockThread',
                     args=((LOCALHOST, port),), daemon=True).start()

    seq = None
    while True:
        try:
            if exit_now:
                try:
                    exit()
                except KeyboardInterrupt:
                    continue  # interrupted while exiting: exit again
            try:
                request = rpc.request_queue.get(block=True, timeout=0.05)
            except queue.Empty:
                request = None
            if request:
                seq, (method, args, kwargs) = request
                ret = method(*args, **kwargs)
                rpc.response_queue.put((seq, ret))
            else:
                handle_tk_events()
        except KeyboardInterrupt:
            if quitting:
                exit_now = True
            continue
        except SystemExit:
            capture_warnings(False)
            raise
        except:
            exc_type, exc, tb = sys.exc_info()
            try:
                print_exception()
                rpc.response_queue.put((seq, None))
            except:
                # The shell cannot show it: use the console instead.
                traceback.print_exception(exc_type, exc, tb, file=sys.__stderr__)
                exit()


def manage_socket(address):
    """Connect to the GUI (with a few retries) and serve it."""
    socket_error = None
    for attempt in range(3):
        time.sleep(attempt)
        try:
            server = MyRPCServer(address, MyHandler)
            break
        except OSError as err:
            print("IDLE Subprocess: OSError: " + err.args[1] +
                  ", retrying....", file=sys.__stderr__)
            socket_error = err
    else:
        print("IDLE Subprocess: Connection to "
              "IDLE GUI failed, exiting.", file=sys.__stderr__)
        show_socket_error(socket_error, address)
        global exit_now
        exit_now = True
        return
    server.handle_request()  # the connection is the only request


def show_socket_error(err, address):
    """Show why the connection to the GUI failed."""
    import tkinter
    from tkinter.messagebox import showerror
    root = tkinter.Tk()
    fix_scaling(root)
    root.withdraw()
    showerror(
        "Subprocess Connection Error",
        f"IDLE's subprocess can't connect to {address[0]}:{address[1]}.\n"
        f"Fatal OSError #{err.errno}: {err.strerror}.\n"
        "See the 'Startup failure' section of the IDLE doc, online at\n"
        "https://docs.python.org/3/library/idle.html#startup-failure",
        parent=root)
    root.destroy()


def get_message_lines(typ, exc, tb):
    """Return the lines of the exception message, including hints such
    as 'Did you mean ...?'."""
    if typ in (NameError, AttributeError):
        # Some suggestions need the traceback's frames, which
        # format_exception_only does not get: take the last line of the
        # interpreter's own report.
        report = io.StringIO()
        with contextlib.redirect_stderr(report):
            sys.__excepthook__(typ, exc, tb)
        lines = report.getvalue().rstrip('\n').split('\n')
        if lines and lines[-1].startswith(typ.__name__):
            return [lines[-1] + '\n']
    return traceback.format_exception_only(typ, exc)


# Exception groups are shown as a tree of boxed members; very wide or
# deep groups are cut off.
_MAX_GROUP_WIDTH = 15
_MAX_GROUP_DEPTH = 10

# Files of IDLE's machinery, hidden in tracebacks shown to the user.
_TRACEBACK_EXCLUDE = ("run.py", "rpc.py", "threading.py", "queue.py",
                      "debugger_r.py", "bdb.py")


def _exception_lines(exc, seen, depth):
    # The lines describing *exc*: chained exceptions first, then its
    # traceback and message, and for a group its members.
    typ = type(exc)
    if id(exc) in seen:
        return [f'<exception {typ.__name__} has printed>']
    seen.add(id(exc))
    lines = []
    cause, context = exc.__cause__, exc.__context__
    if cause is not None and id(cause) not in seen:
        lines += _exception_lines(cause, seen, depth)
        lines += ['', 'The above exception was the direct cause '
                      'of the following exception:', '']
    elif (context is not None and not exc.__suppress_context__ and
            id(context) not in seen):
        lines += _exception_lines(context, seen, depth)
        lines += ['', 'During handling of the above exception, '
                      'another exception occurred:', '']
    group = isinstance(exc, BaseExceptionGroup)
    tb = exc.__traceback__
    if tb:
        entries = traceback.extract_tb(tb)
        cleanup_traceback(entries, _TRACEBACK_EXCLUDE)
        lines.append(('Exception Group Traceback' if group else 'Traceback')
                     + ' (most recent call last):')
        lines += ''.join(traceback.format_list(entries)).splitlines()
    lines += ''.join(get_message_lines(typ, exc, tb)).splitlines()
    if group:
        lines += _group_member_lines(exc, seen, depth + 1)
    return lines


def _group_member_lines(group, seen, depth):
    # The members of an exception group, each in a box drawn with '+' and
    # '|' characters.
    if depth > _MAX_GROUP_DEPTH:
        return [f'... (max_group_depth is {_MAX_GROUP_DEPTH})']
    lines = []
    members = group.exceptions
    for number, member in enumerate(members[:_MAX_GROUP_WIDTH], 1):
        corner = '+-+' if number == 1 else '  +'
        lines.append(f'{corner}---------------- {number} ----------------')
        lines += ['  | ' + line for line in _exception_lines(member, seen, depth)]
    hidden = len(members) - _MAX_GROUP_WIDTH
    if hidden > 0:
        lines.append('  +---------------- ... ----------------')
        lines.append(f'  | and {hidden} more exception{"s" if hidden > 1 else ""}')
    lines.append('  +------------------------------------')
    return lines


def print_exception():
    """Show the current exception on sys.stderr, without the frames of
    IDLE's machinery."""
    linecache.checkcache()
    flush_stdout()
    typ, val, tb = excinfo = sys.exc_info()
    sys.last_type, sys.last_value, sys.last_traceback = excinfo
    sys.last_exc = val
    if val.__traceback__ is not tb:
        val = val.with_traceback(tb)
    for line in _exception_lines(val, set(), 0):
        print(line, file=sys.stderr)


def cleanup_traceback(tb, exclude):
    """Strip frames of the files in *exclude* from both ends of the
    extracted traceback *tb* (in place); fill in the source lines of code
    typed in the shell."""
    original = tb[:]

    def excluded(entry):
        return any(name in entry[0] for name in exclude)

    while tb and excluded(tb[0]):
        del tb[0]
    while tb and excluded(tb[-1]):
        del tb[-1]
    if not tb:
        # The error is in IDLE itself: show everything.
        tb[:] = original
        print("** IDLE Internal Exception: ", file=sys.stderr)
    rpchandler = rpc.objecttable['exec'].rpchandler
    for i, (fn, ln, nm, line) in enumerate(tb):
        if nm == '?':
            nm = "-toplevel-"
        if not line and fn.startswith("<pyshell#"):
            line = rpchandler.remotecall('linecache', 'getline', (fn, ln), {})
        tb[i] = fn, ln, nm, line


def flush_stdout():
    """Flush the output to the shell (nothing to do: it is unbuffered)."""


def exit():
    """End the user process (clearing atexit functions first when the
    'delete-exitfunc' option asks for it)."""
    if no_exitfunc:
        import atexit
        atexit._clear()
    capture_warnings(False)
    sys.exit(0)


def fix_scaling(root):
    """Turn named-font sizes in pixels into points on HiDPI displays."""
    import tkinter.font
    scaling = float(root.tk.call('tk', 'scaling'))
    if scaling > 1.4:
        for name in tkinter.font.names(root):
            font = tkinter.font.Font(root=root, name=name, exists=True)
            size = int(font['size'])
            if size < 0:
                font['size'] = round(-0.75 * size)


def fixdoc(fun, text):
    """Append the paragraph *text* to the docstring of *fun*."""
    head = (fun.__doc__ + '\n\n') if fun.__doc__ is not None else ''
    fun.__doc__ = head + textwrap.fill(textwrap.dedent(text))


# IDLE's frames on the stack of user code need room below the recursion
# limit; the wrappers below add this much and hide it from the user.
RECURSIONLIMIT_DELTA = 30


def install_recursionlimit_wrappers():
    """Make sys.setrecursionlimit/getrecursionlimit account for the
    frames IDLE adds."""

    @functools.wraps(sys.setrecursionlimit)
    def setrecursionlimit(*args, **kwargs):
        if kwargs:
            raise TypeError(
                "setrecursionlimit() takes no keyword arguments")
        if len(args) != 1:
            raise TypeError(f"setrecursionlimit() takes exactly one "
                            f"argument ({len(args)} given)")
        limit, = args
        if not limit > 0:
            raise ValueError(
                "recursion limit must be greater or equal than 1")
        return setrecursionlimit.__wrapped__(limit + RECURSIONLIMIT_DELTA)

    fixdoc(setrecursionlimit, f"""\
            In IDLE, {RECURSIONLIMIT_DELTA} frames are added to the limit
            for IDLE's own use.""")

    @functools.wraps(sys.getrecursionlimit)
    def getrecursionlimit():
        return getrecursionlimit.__wrapped__() - RECURSIONLIMIT_DELTA

    fixdoc(getrecursionlimit, f"""\
            In IDLE, the {RECURSIONLIMIT_DELTA} frames reserved for IDLE's
            own use are not included.""")

    sys.setrecursionlimit(sys.getrecursionlimit() + RECURSIONLIMIT_DELTA)
    sys.setrecursionlimit = setrecursionlimit
    sys.getrecursionlimit = getrecursionlimit


def uninstall_recursionlimit_wrappers():
    """Restore the original recursion limit functions."""
    if (getattr(sys.setrecursionlimit, '__wrapped__', None) and
            getattr(sys.getrecursionlimit, '__wrapped__', None)):
        sys.setrecursionlimit = sys.setrecursionlimit.__wrapped__
        sys.getrecursionlimit = sys.getrecursionlimit.__wrapped__
        sys.setrecursionlimit(sys.getrecursionlimit() - RECURSIONLIMIT_DELTA)


class MyRPCServer(rpc.RPCServer):

    def handle_error(self, request, client_address):
        """Handle an error of the socket thread: when the GUI is gone,
        let the main thread exit; report anything else and quit."""
        global quitting, exit_now
        try:
            raise
        except SystemExit:
            raise
        except EOFError:
            exit_now = True
            thread.interrupt_main()
        except:
            erf = sys.__stderr__
            line = '-' * 40
            print(f'\n{line}\nUnhandled exception in user code execution '
                  f'server!\nThread: {threading.current_thread().name}\n'
                  f'IDLE Client Address: {client_address}\n'
                  f'Request: {request!r}', file=erf)
            traceback.print_exc(limit=-20, file=erf)
            print(f'\n*** Unrecoverable, server exiting!\n\n'
                  f'This should not happen.  If it happens again, please '
                  f'report it\nwith this message and how to reproduce '
                  f'it.\n{line}', file=erf)
            quitting = True
            thread.interrupt_main()


# The shell's streams, used for sys.stdin/stdout/stderr in the user process
# (and by pyshell when there is no user process).

class StdioFile(io.TextIOBase):
    """A text stream of the shell window."""

    def __init__(self, shell, tags, encoding='utf-8', errors='strict'):
        self.shell = shell
        self.tags = tags  # the shell's tag (and name) of the stream
        self._encoding = encoding
        self._errors = errors

    @property
    def encoding(self):
        return self._encoding

    @property
    def errors(self):
        return self._errors

    @property
    def name(self):
        return '<%s>' % self.tags

    def isatty(self):
        return True


class StdOutputFile(StdioFile):
    """Output shown in the shell window."""

    def writable(self):
        return True

    def write(self, s):
        if self.closed:
            raise ValueError("write to closed file")
        # Apply the error handler as a real stream would; also turns str
        # subclasses into str.
        s = str.encode(s, self.encoding, self.errors).decode(self.encoding,
                                                               self.errors)
        return self.shell.write(s, self.tags)


class StdInputFile(StdioFile):
    """Input typed in the shell window, line by line."""

    _line_buffer = ''

    def readable(self):
        return True

    def _check_size(self, size):
        if self.closed:
            raise ValueError("read from closed file")
        if size is None:
            return -1
        if not isinstance(size, int):
            raise TypeError('must be int, not ' + type(size).__name__)
        return size

    def read(self, size=-1):
        size = self._check_size(size)
        result = self._line_buffer
        self._line_buffer = ''
        if size < 0:
            while line := self.shell.readline():
                result += line
            return result
        while len(result) < size:
            line = self.shell.readline()
            if not line:
                break
            result += line
        self._line_buffer = result[size:]
        return result[:size]

    def readline(self, size=-1):
        size = self._check_size(size)
        line = self._line_buffer or self.shell.readline()
        if size < 0:
            size = len(line)
        eol = line.find('\n', 0, size)
        if eol >= 0:
            size = eol + 1
        self._line_buffer = line[size:]
        return line[:size]

    def readlines(self, hint=-1):
        """Read lines until EOF, or until more than *hint* characters
        have been read (hint <= 0 or None: no limit)."""
        if hint is None:
            hint = -1
        elif not isinstance(hint, int):
            raise TypeError('must be int, not ' + type(hint).__name__)
        lines = []
        total = 0
        while line := self.readline():
            lines.append(line)
            total += len(line)
            if 0 < hint < total:
                break
        return lines

    def close(self):
        # User code cannot close the shell's input.
        self.shell.close()


class MyHandler(rpc.RPCHandler):

    def handle(self):
        """Set up the user process for the shell and serve requests."""
        executive = Executive(self)
        self.register("exec", executive)
        self.console = self.get_remote_proxy("console")
        sys.stdin = StdInputFile(self.console, "stdin",
                                 iomenu.encoding, iomenu.errors)
        sys.stdout = StdOutputFile(self.console, "stdout",
                                   iomenu.encoding, iomenu.errors)
        sys.stderr = StdOutputFile(self.console, "stderr",
                                   iomenu.encoding, "backslashreplace")
        sys.displayhook = rpc.displayhook
        # help() output goes to the shell, without a pager.
        import pydoc
        pydoc.pager = pydoc.plainpager
        # Keep the shell's stdin alive even if user code replaces
        # sys.stdin.
        self._keep_stdin = sys.stdin
        install_recursionlimit_wrappers()
        self.interp = self.get_remote_proxy("interp")
        rpc.RPCHandler.getresponse(self, myseq=None, wait=0.05)

    def exithook(self):
        # The main thread ends the process.
        time.sleep(10)

    def EOFhook(self):
        # The GUI is gone: stop the main thread.
        global quitting
        quitting = True
        thread.interrupt_main()

    def decode_interrupthook(self):
        global quitting
        quitting = True
        thread.interrupt_main()


class Executive:
    """The object (rpc id 'exec') through which the GUI runs code."""

    def __init__(self, rpchandler):
        self.rpchandler = rpchandler
        if idlelib.testing is False:
            self.locals = __main__.__dict__
            self.calltip = calltip.Calltip()
            self.autocomplete = autocomplete.AutoComplete()
        else:
            self.locals = {}

    def runcode(self, code):
        """Execute *code*; show uncaught exceptions in the shell."""
        global interruptible
        try:
            self.user_exc_info = None
            interruptible = True
            try:
                exec(code, self.locals)
            finally:
                interruptible = False
        except SystemExit as e:
            # Back to the prompt; show a non-integer exit status.
            if e.args:
                status = e.args[0]
                if not isinstance(status, (type(None), int)):
                    print('SystemExit: ' + str(status), file=sys.stderr)
        except:
            self.user_exc_info = sys.exc_info()  # kept for the stack viewer
            if quitting:
                exit()
            if sys.excepthook is sys.__excepthook__:
                print_exception()
            else:
                try:
                    sys.excepthook(*self.user_exc_info)
                except:
                    self.user_exc_info = sys.exc_info()
                    print_exception()
            jit = self.rpchandler.console.getvar("<<toggle-jit-stack-viewer>>")
            if jit:
                self.rpchandler.interp.open_remote_stack_viewer()
        else:
            flush_stdout()

    def interrupt_the_server(self):
        """Interrupt the user code (Shell > Interrupt Execution)."""
        if not interruptible:
            return
        import signal
        handler = signal.getsignal(signal.SIGINT)
        if handler in (signal.SIG_DFL, signal.SIG_IGN, None):
            thread.interrupt_main()
            return
        # A real signal also ends a blocking call such as time.sleep().
        try:
            signal.pthread_kill(threading.main_thread().ident, signal.SIGINT)
        except (AttributeError, OSError):
            thread.interrupt_main()

    def start_the_debugger(self, gui_adap_oid):
        return debugger_r.start_debugger(self.rpchandler, gui_adap_oid)

    def stop_the_debugger(self, idb_adap_oid):
        """Forget the debugger's objects."""
        self.rpchandler.unregister(idb_adap_oid)

    def get_the_calltip(self, name):
        return self.calltip.fetch_tip(name)

    def get_the_completion_list(self, what, mode):
        return self.autocomplete.fetch_completions(what, mode)

    def stackviewer(self, flist_oid=None):
        """Return a tree item for the stack of the last exception."""
        if not self.user_exc_info:
            return None
        _, exc, tb = self.user_exc_info
        flist = None
        if flist_oid is not None:
            flist = self.rpchandler.get_remote_proxy(flist_oid)
        while tb and tb.tb_frame.f_globals["__name__"] in ["rpc", "run"]:
            tb = tb.tb_next
        exc.__traceback__ = tb
        item = stackviewer.StackTreeItem(exc, flist)
        return debugobj_r.remote_object_tree_item(item)


_tcl = None


def handle_tk_events():
    """Let tkinter windows created by user code process their events."""
    global _tcl
    if _tcl is None:
        _tcl = tkinter.Tcl()
    _tcl.eval("update")


if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_run', verbosity=2)
