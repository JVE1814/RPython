"""The path browser (File > Path Browser): the directories of sys.path,
their packages and Python files, and the classes and functions of the
files (as in the module browser)."""
import importlib.machinery
import os
import sys

from idlelib.browser import ModuleBrowser, ModuleBrowserTreeItem
from idlelib.tree import TreeItem


class PathBrowser(ModuleBrowser):

    def __init__(self, master, *, _htest=False, _utest=False):
        """_htest places the window for the human test; _utest leaves
        the tree unexpanded."""
        self.master = master
        self._htest = _htest
        self._utest = _utest
        self.init()

    def settitle(self):
        self.top.wm_title("Path Browser")
        self.top.wm_iconname("Path Browser")

    def rootnode(self):
        return PathBrowserTreeItem()


class PathBrowserTreeItem(TreeItem):
    """The root: one child per sys.path entry."""

    def GetText(self):
        return "sys.path"

    def GetSubList(self):
        return [DirBrowserTreeItem(folder) for folder in sys.path]


class DirBrowserTreeItem(TreeItem):
    """A directory: its packages and Python modules."""

    def __init__(self, dir, packages=[]):
        self.dir = dir
        self.packages = packages  # the package names leading here

    def GetText(self):
        if not self.packages:
            return self.dir
        return self.packages[-1] + ": package"

    def GetSubList(self):
        try:
            names = os.listdir(self.dir or os.curdir)
        except OSError:
            return []
        packages = []
        for name in names:
            file = os.path.join(self.dir, name)
            if self.ispackagedir(file):
                nn = os.path.normcase(name)
                packages.append((nn, name, file))
        packages.sort()
        sublist = []
        for nn, name, file in packages:
            sublist.append(DirBrowserTreeItem(file, self.packages + [name]))
        for nn, name in self.listmodules(names):
            sublist.append(ModuleBrowserTreeItem(os.path.join(self.dir, name)))
        return sublist

    def ispackagedir(self, file):
        """Return True if *file* is a directory with an __init__.py."""
        if not os.path.isdir(file):
            return False
        init = os.path.join(file, "__init__.py")
        return os.path.exists(init)

    def listmodules(self, allnames):
        """Return (normalized name, file name) of the modules in
        *allnames*, one per module name (the first suffix wins)."""
        modules = {}
        suffixes = importlib.machinery.EXTENSION_SUFFIXES[:]
        suffixes += importlib.machinery.SOURCE_SUFFIXES
        suffixes += importlib.machinery.BYTECODE_SUFFIXES
        sorted_names = []
        for suffix in suffixes:
            i = -len(suffix)
            for name in allnames[:]:
                normed_name = os.path.normcase(name)
                if normed_name[i:] == suffix:
                    mod_name = name[:i]
                    if mod_name not in modules:
                        modules[mod_name] = None
                        sorted_names.append((normed_name, name))
                        allnames.remove(name)
        sorted_names.sort()
        return sorted_names


def _path_browser(parent):  # htest #
    PathBrowser(parent, _htest=True)
    parent.mainloop()


if __name__ == "__main__":
    from unittest import main
    main('idlelib.idle_test.test_pathbrowser', verbosity=2, exit=False)
