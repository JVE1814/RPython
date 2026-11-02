"""The module browser (File > Module Browser).

A tree of the classes and functions of a module (with nested ones), from
pyclbr.  Double-clicking an item opens the file at its definition.
"""
import os
import pyclbr
import sys

from idlelib.config import idleConf
from idlelib import pyshell
from idlelib.tree import TreeNode, TreeItem, ScrolledCanvas
from idlelib.util import py_extensions
from idlelib.window import ListedToplevel


file_open = None  # opens a file in an editor window (set by ModuleBrowser)
# Python files without code worth browsing.
browseable_extension_blocklist = ('.pyi',)


def is_browseable_extension(path):
    _, ext = os.path.splitext(path)
    ext = os.path.normcase(ext)
    return ext in py_extensions and ext not in browseable_extension_blocklist


def transform_children(child_dict, modname=None):
    """Return the pyclbr objects of *child_dict* in source order.

    With *modname*, only the objects defined in that module.  The names
    of classes get their bases appended ('C(Base, mod.Other)'), once.
    """
    objects = []
    for key, obj in child_dict.items():
        if modname is not None and obj.module != modname:
            continue
        if getattr(obj, 'super', None) and obj.name == key:
            bases = []
            for base in obj.super:
                if isinstance(base, str):
                    bases.append(base)
                elif base.module != obj.module:
                    bases.append(f'{base.module}.{base.name}')
                else:
                    bases.append(base.name)
            obj.name += f"({', '.join(bases)})"
        objects.append(obj)
    return sorted(objects, key=lambda o: o.lineno)


class ModuleBrowser:
    """The browser window of one module."""

    def __init__(self, master, path, *, _htest=False, _utest=False):
        """*path* is the module's file.  _htest places the window for
        the human test; _utest leaves the tree unexpanded."""
        self.master = master
        self.path = path
        self._htest = _htest
        self._utest = _utest
        self.init()

    def close(self, event=None):
        self.top.destroy()
        self.node.destroy()

    def init(self):
        global file_open
        root = self.master
        if self._htest or self._utest:
            flist = pyshell.PyShellFileList(root)
        else:
            flist = pyshell.flist
        file_open = flist.open
        pyclbr._modules.clear()

        self.top = top = ListedToplevel(root)
        top.protocol("WM_DELETE_WINDOW", self.close)
        top.bind("<Escape>", self.close)
        if self._htest:
            top.geometry("+%d+%d" % (root.winfo_rootx(),
                                     root.winfo_rooty() + 200))
        self.settitle()
        top.focus_set()

        theme = idleConf.CurrentTheme()
        background = idleConf.GetHighlight(theme, 'normal')['background']
        sc = ScrolledCanvas(top, bg=background, highlightthickness=0,
                            takefocus=1)
        sc.frame.pack(expand=1, fill="both")
        item = self.rootnode()
        self.node = node = TreeNode(sc.canvas, None, item)
        if not self._utest:
            node.update()
            node.expand()

    def settitle(self):
        self.top.wm_title("Module Browser - " + os.path.basename(self.path))
        self.top.wm_iconname("Module Browser")

    def rootnode(self):
        return ModuleBrowserTreeItem(self.path)


class ModuleBrowserTreeItem(TreeItem):
    """The module: its children are its top-level classes and
    functions."""

    def __init__(self, file):
        self.file = file

    def GetText(self):
        return os.path.basename(self.file)

    def GetIconName(self):
        return "python"

    def IsExpandable(self):
        return True

    def GetSubList(self):
        return [ChildBrowserTreeItem(obj) for obj in self.listchildren()]

    def OnDoubleClick(self):
        if not is_browseable_extension(self.file):
            return
        if not os.path.exists(self.file):
            return
        file_open(self.file)

    def listchildren(self):
        """Return the classes and functions of the module."""
        if not is_browseable_extension(self.file):
            return []
        folder, base = os.path.split(self.file)
        name, _ = os.path.splitext(base)
        try:
            tree = pyclbr.readmodule_ex(name, [folder] + sys.path)
        except ImportError:
            return []
        return transform_children(tree, name)


class ChildBrowserTreeItem(TreeItem):
    """A class or function (with its nested classes and functions)."""

    def __init__(self, obj):
        self.obj = obj
        self.name = obj.name
        self.isfunction = isinstance(obj, pyclbr.Function)

    def GetText(self):
        if self.isfunction:
            return "def " + self.name + "(...)"
        return "class " + self.name

    def GetIconName(self):
        return "python" if self.isfunction else "folder"

    def IsExpandable(self):
        return self.obj.children != {}

    def GetSubList(self):
        return [ChildBrowserTreeItem(obj)
                for obj in transform_children(self.obj.children)]

    def OnDoubleClick(self):
        """Open the file at the definition."""
        try:
            edit = file_open(self.obj.file)
            edit.gotoline(self.obj.lineno)
        except (OSError, AttributeError):
            pass


def _module_browser(parent):  # htest #
    if len(sys.argv) > 1:  # a file given on the command line
        file = sys.argv[1]
    else:
        file = __file__
        # Nested classes and functions to show.
        class Nested_in_func(TreeNode):
            def nested_in_class(): pass
        def closure():
            class Nested_in_closure: pass
    ModuleBrowser(parent, file, _htest=True)


if __name__ == "__main__":
    if len(sys.argv) == 1:  # not run with a file to browse
        from unittest import main
        main('idlelib.idle_test.test_browser', verbosity=2, exit=False)
