"""IDLE's debugger: a window to step through code run in the shell.

Idb is the bdb subclass that stops the program; Debugger is the window
(control buttons, the stack and the local/global variables of the
selected frame).  With a user process, Idb runs there and talks to the
window through debugger_r.
"""
import bdb
import os

from tkinter import *
from tkinter.ttk import Frame, Scrollbar

from idlelib import macosx
from idlelib.scrolledlist import ScrolledList
from idlelib.window import ListedToplevel


class Idb(bdb.Bdb):
    """A Bdb that reports each stop to the debugger window *gui*."""

    def __init__(self, gui):
        self.gui = gui
        super().__init__()

    def user_line(self, frame):
        # Step over IDLE's rpc machinery.
        if _in_rpc_code(frame):
            self.set_step()
            return
        message = _frame2message(frame)
        try:
            self.gui.interaction(message, frame)
        except TclError:  # the window was closed
            pass

    def user_exception(self, frame, exc_info):
        if _in_rpc_code(frame):
            self.set_step()
            return
        message = _frame2message(frame)
        self.gui.interaction(message, frame, exc_info)


def _in_rpc_code(frame):
    """Is *frame* executing (or called from) IDLE's rpc code?"""
    while True:
        if 'rpc.py' in frame.f_code.co_filename:
            return True
        caller = frame.f_back
        if caller is None:
            return False
        name = caller.f_code.co_filename
        if 'idlelib' in name and 'debugger' in name:
            # reached the debugger itself: user code is not below rpc
            return False
        frame = caller


def _frame2message(frame):
    """Return 'file.py:lineno: function()' for *frame*."""
    code = frame.f_code
    message = f"{os.path.basename(code.co_filename)}:{frame.f_lineno}"
    if code.co_name != "?":
        message = f"{message}: {code.co_name}()"
    return message


class Debugger:
    """The debugger window."""

    vstack = None
    vsource = None
    vlocals = None
    vglobals = None
    stackviewer = None
    localsviewer = None
    globalsviewer = None

    def __init__(self, pyshell, idb=None):
        """Create the window for *pyshell*; *idb* is the Idb (or its
        proxy in the user process)."""
        if idb is None:
            idb = Idb(self)
        self.pyshell = pyshell
        self.idb = idb
        self.frame = None
        self.make_gui()
        self.interacting = False
        self.nesting_level = 0

    def run(self, *args):
        """Run code under the debugger (idb.run(*args)).

        A call made while the debugger is waiting for the user (the user
        process asks again before the previous call returned) waits for
        that interaction to end first.
        """
        if self.nesting_level > 0:
            self.abort_loop()
            self.root.after(100, lambda: self.run(*args))
            return None
        try:
            self.interacting = True
            return self.idb.run(*args)
        finally:
            self.interacting = False

    def close(self, event=None):
        """Close the debugger window (stopping the program first)."""
        try:
            self.quit()
        except Exception:
            pass
        if self.interacting:
            self.top.bell()
            return
        if self.stackviewer:
            self.stackviewer.close()
            self.stackviewer = None
        # Let the shell forget the debugger (also when closed here).
        self.pyshell.close_debugger()
        self.top.destroy()

    def make_gui(self):
        """Build the window."""
        pyshell = self.pyshell
        self.flist = pyshell.flist
        self.root = root = pyshell.root
        self.top = top = ListedToplevel(root)
        self.top.wm_title("Debug Control")
        self.top.wm_iconname("Debug")
        top.wm_protocol("WM_DELETE_WINDOW", self.close)
        self.top.bind("<Escape>", self.close)

        self.bframe = bframe = Frame(top)
        self.bframe.pack(anchor="w")
        self.buttons = []
        for label, command in (("Go", self.cont), ("Step", self.step),
                               ("Over", self.next), ("Out", self.ret),
                               ("Quit", self.quit)):
            b = Button(bframe, text=label, command=command)
            b.pack(side="left")
            self.buttons.append(b)
        self.cframe = cframe = Frame(bframe)
        self.cframe.pack(side="left")

        if not self.vstack:
            self.__class__.vstack = BooleanVar(top)
            self.vstack.set(1)
        self.bstack = Checkbutton(cframe, text="Stack", command=self.show_stack,
                                  variable=self.vstack)
        self.bstack.grid(row=0, column=0)
        if not self.vsource:
            self.__class__.vsource = BooleanVar(top)
        self.bsource = Checkbutton(cframe, text="Source",
                                   command=self.show_source, variable=self.vsource)
        self.bsource.grid(row=0, column=1)
        if not self.vlocals:
            self.__class__.vlocals = BooleanVar(top)
            self.vlocals.set(1)
        self.blocals = Checkbutton(cframe, text="Locals",
                                   command=self.show_locals, variable=self.vlocals)
        self.blocals.grid(row=1, column=0)
        if not self.vglobals:
            self.__class__.vglobals = BooleanVar(top)
        self.bglobals = Checkbutton(cframe, text="Globals",
                                    command=self.show_globals,
                                    variable=self.vglobals)
        self.bglobals.grid(row=1, column=1)

        self.status = Label(top, anchor="w")
        self.status.pack(anchor="w")
        self.error = Label(top, anchor="w")
        self.error.pack(anchor="w", fill="x")
        self.errorbg = self.error.cget("background")

        self.fstack = Frame(top, height=1)
        self.fstack.pack(expand=1, fill="both")
        self.flocals = Frame(top)
        self.flocals.pack(expand=1, fill="both")
        self.fglobals = Frame(top, height=1)
        self.fglobals.pack(expand=1, fill="both")

        if self.vstack.get():
            self.show_stack()
        if self.vlocals.get():
            self.show_locals()
        if self.vglobals.get():
            self.show_globals()

    def interaction(self, message, frame, info=None):
        """Show where the program stopped and wait for a command."""
        self.frame = frame
        self.status.configure(text=message)

        if info:
            type, value, tb = info
            try:
                m1 = type.__name__
            except AttributeError:
                m1 = "%s" % str(type)
            if value is not None:
                try:
                    m1 = f"{m1}: {value}"
                except:
                    pass
            bg = "yellow"
        else:
            m1 = ""
            tb = None
            bg = self.errorbg
        self.error.configure(text=m1, background=bg)

        sv = self.stackviewer
        if sv:
            stack, i = self.idb.get_stack(self.frame, tb)
            sv.load_stack(stack, i)

        self.show_variables(1)

        if self.vsource.get():
            self.sync_source_line()

        for b in self.buttons:
            b.configure(state="normal")

        self.top.wakeup()
        # Wait in a nested event loop until a button sets the variable.
        self.nesting_level += 1
        self.root.tk.call('vwait', '::idledebugwait')
        self.nesting_level -= 1

        for b in self.buttons:
            b.configure(state="disabled")
        self.status.configure(text="")
        self.error.configure(text="", background=self.errorbg)
        self.frame = None

    def sync_source_line(self):
        """Show the current line of the program in an editor."""
        frame = self.frame
        if not frame:
            return
        filename, lineno = self.__frame2fileline(frame)
        if filename[:1] + filename[-1:] != "<>" and os.path.exists(filename):
            self.flist.gotofileline(filename, lineno)

    def __frame2fileline(self, frame):
        code = frame.f_code
        return code.co_filename, frame.f_lineno

    def cont(self):
        self.idb.set_continue()
        self.abort_loop()

    def step(self):
        self.idb.set_step()
        self.abort_loop()

    def next(self):
        self.idb.set_next(self.frame)
        self.abort_loop()

    def ret(self):
        self.idb.set_return(self.frame)
        self.abort_loop()

    def quit(self):
        self.idb.set_quit()
        self.abort_loop()

    def abort_loop(self):
        """End the wait of interaction()."""
        self.root.tk.call('set', '::idledebugwait', '1')

    def show_stack(self):
        if not self.stackviewer and self.vstack.get():
            self.stackviewer = sv = StackViewer(self.fstack, self.flist, self)
            if self.frame:
                stack, i = self.idb.get_stack(self.frame, None)
                sv.load_stack(stack, i)
        else:
            sv = self.stackviewer
            if sv and not self.vstack.get():
                self.stackviewer = None
                sv.close()
            self.fstack['height'] = 1

    def show_source(self):
        if self.vsource.get():
            self.sync_source_line()

    def show_frame(self, stackitem):
        """Select a frame of the stack (from the stack viewer)."""
        self.frame = stackitem[0]
        self.show_variables()

    def show_locals(self):
        lv = self.localsviewer
        if self.vlocals.get():
            if not lv:
                self.localsviewer = NamespaceViewer(self.flocals, "Locals")
        else:
            if lv:
                self.localsviewer = None
                lv.close()
                self.flocals['height'] = 1
        self.show_variables()

    def show_globals(self):
        gv = self.globalsviewer
        if self.vglobals.get():
            if not gv:
                self.globalsviewer = NamespaceViewer(self.fglobals, "Globals")
        else:
            if gv:
                self.globalsviewer = None
                gv.close()
                self.fglobals['height'] = 1
        self.show_variables()

    def show_variables(self, force=0):
        lv = self.localsviewer
        gv = self.globalsviewer
        frame = self.frame
        if not frame:
            ldict = gdict = None
        else:
            ldict = frame.f_locals
            gdict = frame.f_globals
            if lv and gv and ldict is gdict:
                ldict = None
        if lv:
            lv.load_dict(ldict, force, self.pyshell.interp.rpcclt)
        if gv:
            gv.load_dict(gdict, force, self.pyshell.interp.rpcclt)

    def set_breakpoint(self, filename, lineno):
        """Set a breakpoint at line *lineno* of *filename*."""
        self.idb.set_break(filename, lineno)

    def clear_breakpoint(self, filename, lineno):
        self.idb.clear_break(filename, lineno)

    def clear_file_breaks(self, filename):
        self.idb.clear_all_file_breaks(filename)

    def load_breakpoints(self):
        """Set the breakpoints of all editor windows."""
        for editwin in self.pyshell.flist.inversedict:
            filename = editwin.io.filename
            try:
                for lineno in editwin.breakpoints:
                    self.set_breakpoint(filename, lineno)
            except AttributeError:
                continue


class StackViewer(ScrolledList):
    """The list of the frames of the stack."""

    def __init__(self, master, flist, gui):
        if macosx.isAquaTk():
            # Aqua Tk lacks a working right-click menu here.
            ScrolledList.__init__(self, master)
        else:
            ScrolledList.__init__(self, master, width=80)
        self.flist = flist
        self.gui = gui
        self.stack = []

    def load_stack(self, stack, index=None):
        """Show *stack* (a list of (frame, lineno)), selecting *index*."""
        self.stack = stack
        self.clear()
        for frame, lineno in stack:
            try:
                modname = frame.f_globals["__name__"]
            except:
                modname = "?"
            code = frame.f_code
            filename = code.co_filename
            funcname = code.co_name
            import linecache
            sourceline = linecache.getline(filename, lineno)
            sourceline = sourceline.strip()
            if funcname in ("?", "", None):
                item = f"{modname}, line {lineno}: {sourceline}"
            else:
                item = f"{modname}.{funcname}(), line {lineno}: {sourceline}"
            if index is not None and lineno == index:
                item = "> " + item
            self.append(item)
        if index is not None:
            self.select(index)

    def popup_event(self, event):
        # A context menu only for a non-empty stack.
        if self.stack:
            return ScrolledList.popup_event(self, event)

    def fill_menu(self):
        menu = self.menu
        menu.add_command(label="Go to source line",
                         command=self.goto_source_line)
        menu.add_command(label="Show stack frame",
                         command=self.show_stack_frame)

    def on_select(self, index):
        if 0 <= index < len(self.stack):
            self.gui.show_frame(self.stack[index])

    def on_double(self, index):
        self.show_source(index)

    def goto_source_line(self):
        index = self.listbox.index("active")
        self.show_source(index)

    def show_stack_frame(self):
        index = self.listbox.index("active")
        if 0 <= index < len(self.stack):
            self.gui.show_frame(self.stack[index])

    def show_source(self, index):
        """Open the source of frame *index* at its line."""
        if not (0 <= index < len(self.stack)):
            return
        frame, lineno = self.stack[index]
        filename = frame.f_code.co_filename
        if os.path.isfile(filename):
            edit = self.flist.open(filename)
            if edit:
                edit.gotoline(lineno)


class NamespaceViewer:
    """Shows the names and values of a namespace."""

    def __init__(self, master, title, odict=None):
        width = 0
        height = 40
        if odict:
            height = 20 * len(odict)
        self.master = master
        self.title = title
        import reprlib
        self.repr = reprlib.Repr()
        self.repr.maxstring = 60
        self.repr.maxother = 60
        self.frame = frame = Frame(master)
        self.frame.pack(expand=1, fill="both")
        self.label = Label(frame, text=title, borderwidth=2, relief="groove")
        self.label.pack(fill="x")
        self.vbar = vbar = Scrollbar(frame, name="vbar")
        vbar.pack(side="right", fill="y")
        self.canvas = canvas = Canvas(frame, height=min(300, max(40, height)),
                                      scrollregion=(0, 0, width, height))
        canvas.pack(side="left", fill="both", expand=1)
        vbar["command"] = canvas.yview
        canvas["yscrollcommand"] = vbar.set
        self.subframe = subframe = Frame(canvas)
        self.sfid = canvas.create_window(0, 0, window=subframe, anchor="nw")
        self.load_dict(odict)

    prev_odict = -1  # anything but None or a dict

    def load_dict(self, odict, force=0, rpc_client=None):
        """Show the namespace *odict* (a dict, a proxy or None)."""
        if odict is self.prev_odict and not force:
            return
        subframe = self.subframe
        frame = self.frame
        for c in list(subframe.children.values()):
            c.destroy()
        self.prev_odict = None
        if not odict:
            label = Label(subframe, text="None")
            label.grid(row=0, column=0)
        else:
            keys_list = odict.keys()
            names = sorted(keys_list)
            row = 0
            for name in names:
                value = odict[name]
                svalue = self.repr.repr(value)
                if rpc_client:
                    # A proxy: its repr is the string itself.
                    svalue = svalue[1:-1]
                label = Label(subframe, text=name)
                label.grid(row=row, column=0, sticky="nw")
                entry = Entry(subframe, width=0, borderwidth=0)
                entry.insert(0, svalue)
                entry.grid(row=row, column=1, sticky="nw")
                row = row + 1
        self.prev_odict = odict
        subframe.update_idletasks()
        width = subframe.winfo_reqwidth()
        height = subframe.winfo_reqheight()
        canvas = self.canvas
        self.canvas["scrollregion"] = (0, 0, width, height)
        if height > 300:
            canvas["height"] = 300
            frame.pack(expand=1)
        else:
            canvas["height"] = height
            frame.pack(expand=0)

    def close(self):
        self.frame.destroy()


if __name__ == "__main__":
    from unittest import main
    main('idlelib.idle_test.test_debugger', verbosity=2, exit=False)
