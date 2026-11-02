"""The stack viewer: the traceback of the last exception as a tree whose
frames show their local and global variables."""
import linecache
import os

import tkinter as tk

from idlelib.debugobj import ObjectTreeItem, make_objecttreeitem
from idlelib.tree import TreeNode, TreeItem, ScrolledCanvas


def StackBrowser(root, exc, flist=None, top=None):
    """Show the stack of exception *exc* in a new window."""
    global sc, item, node  # kept for testing
    if top is None:
        from idlelib.window import ListedToplevel
        top = ListedToplevel(root)
        top.title("Stack Viewer")
        top.iconname("Stack")
    sc = ScrolledCanvas(top, bg="white", highlightthickness=0)
    sc.frame.pack(expand=1, fill="both")
    item = StackTreeItem(exc, flist)
    node = TreeNode(sc.canvas, None, item)
    node.expand()


class StackTreeItem(TreeItem):
    """The traceback of an exception."""

    def __init__(self, exc, flist=None):
        self.flist = flist
        self.stack = self.get_stack(None if exc is None else exc.__traceback__)
        self.text = f"{type(exc).__name__}: {str(exc)}"

    def get_stack(self, tb):
        # (frame, line number) pairs, without frames of IDLE's machinery.
        stack = []
        if tb and tb.tb_frame is None:
            tb = tb.tb_next
        while tb is not None:
            stack.append((tb.tb_frame, tb.tb_lineno))
            tb = tb.tb_next
        return stack

    def GetText(self):
        return self.text

    def GetSubList(self):
        return [FrameTreeItem(info, self.flist) for info in self.stack]


class FrameTreeItem(TreeItem):
    """A frame of the stack: shows its location and source line."""

    def __init__(self, info, flist):
        self.info = info
        self.flist = flist

    def GetText(self):
        frame, lineno = self.info
        try:
            modname = frame.f_globals["__name__"]
        except:
            modname = "?"
        code = frame.f_code
        filename = code.co_filename
        funcname = code.co_name
        sourceline = linecache.getline(filename, lineno).strip()
        if funcname in ("?", "", None):
            return f"{modname}, line {lineno}: {sourceline}"
        return f"{modname}.{funcname}(...), line {lineno}: {sourceline}"

    def GetSubList(self):
        frame, lineno = self.info
        items = []
        if frame.f_globals is not frame.f_locals:
            items.append(VariablesTreeItem("<locals>", frame.f_locals, self.flist))
        items.append(VariablesTreeItem("<globals>", frame.f_globals, self.flist))
        return items

    def OnDoubleClick(self):
        if self.flist:
            frame, lineno = self.info
            filename = frame.f_code.co_filename
            if os.path.isfile(filename):
                self.flist.gotofileline(filename, lineno)


class VariablesTreeItem(ObjectTreeItem):
    """A namespace (dict) of a frame."""

    def GetText(self):
        return self.labeltext

    def GetLabelText(self):
        return None

    def IsExpandable(self):
        return len(self.object) > 0

    def GetSubList(self):
        items = []
        for key in self.object.keys():
            try:
                value = self.object[key]
            except KeyError:
                continue

            def setfunction(value, key=key, object_=self.object):
                object_[key] = value

            items.append(make_objecttreeitem(key + " =", value, setfunction))
        return items


def _stackbrowser(parent):  # htest #
    from idlelib.pyshell import PyShellFileList
    top = tk.Toplevel(parent)
    top.title("Test StackViewer")
    x, y = map(int, parent.geometry().split('+')[1:])
    top.geometry("+%d+%d" % (x + 50, y + 175))
    flist = PyShellFileList(top)
    try:  # to obtain a traceback object
        intentional_name_error
    except NameError as e:
        StackBrowser(top, e, flist=flist, top=top)


if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_stackviewer', verbosity=2, exit=False)
