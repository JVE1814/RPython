"""Tree items that show a Python object and its contents (used by the
stack viewer and the debugger)."""
import reprlib

from idlelib.tree import TreeItem, TreeNode, ScrolledCanvas

_repr = reprlib.Repr()
_repr.maxstring = 150
_repr.maxother = 150
myrepr = _repr.repr


class ObjectTreeItem(TreeItem):
    """An object shown as 'label = repr(object)'.

    *setfunction*, if given, is called with a new value when the text is
    edited.
    """

    def __init__(self, labeltext, object_, setfunction=None):
        self.labeltext = labeltext
        self.object = object_
        self.setfunction = setfunction

    def GetLabelText(self):
        return self.labeltext

    def GetText(self):
        return myrepr(self.object)

    def GetIconName(self):
        if not self.IsExpandable():
            return "python"

    def IsEditable(self):
        return self.setfunction is not None

    def SetText(self, text):
        try:
            value = eval(text)
            self.setfunction(value)
        except:
            pass
        else:
            self.object = value

    def IsExpandable(self):
        return not not dir(self.object)

    def GetSubList(self):
        items = []
        for key in dir(self.object):
            try:
                value = getattr(self.object, key)
            except AttributeError:
                continue
            item = make_objecttreeitem(
                str(key) + " =",
                value,
                lambda value, key=key, obj=self.object: setattr(obj, key, value))
            items.append(item)
        return items


class ClassTreeItem(ObjectTreeItem):
    """A class: its contents are its bases and attributes."""

    def IsExpandable(self):
        return True

    def GetSubList(self):
        items = ObjectTreeItem.GetSubList(self)
        if len(self.object.__bases__) == 1:
            bases_item = make_objecttreeitem("__bases__[0] =",
                                             self.object.__bases__[0])
        else:
            bases_item = make_objecttreeitem("__bases__ =", self.object.__bases__)
        items.insert(0, bases_item)
        return items


class AtomicObjectTreeItem(ObjectTreeItem):
    """An object without contents (numbers, strings, ...)."""

    def IsExpandable(self):
        return False


class SequenceTreeItem(ObjectTreeItem):
    """A sequence: its items are shown with their index."""

    def IsExpandable(self):
        return len(self.object) > 0

    def keys(self):
        return range(len(self.object))

    def GetSubList(self):
        items = []
        for key in self.keys():
            try:
                value = self.object[key]
            except KeyError:
                continue

            def setfunction(value, key=key, obj=self.object):
                obj[key] = value

            items.append(make_objecttreeitem(f"{key!r}:", value, setfunction))
        return items


class DictTreeItem(SequenceTreeItem):
    """A dict: its items are shown sorted by key (when possible)."""

    def keys(self):
        keys = list(self.object)
        try:
            keys.sort()
        except:
            pass
        return keys


# Tree item class by the type of the object shown.
dispatch = {
    int: AtomicObjectTreeItem,
    float: AtomicObjectTreeItem,
    str: AtomicObjectTreeItem,
    tuple: SequenceTreeItem,
    list: SequenceTreeItem,
    dict: DictTreeItem,
    type: ClassTreeItem,
}


def make_objecttreeitem(labeltext, object_, setfunction=None):
    """Return the tree item suitable for *object_*."""
    cls = dispatch.get(type(object_), ObjectTreeItem)
    return cls(labeltext, object_, setfunction)


def _debug_object_browser(parent):  # htest #
    import sys
    from tkinter import Toplevel
    top = Toplevel(parent)
    top.title("Test debug object browser")
    x, y = map(int, parent.geometry().split('+')[1:])
    top.geometry("+%d+%d" % (x + 100, y + 175))
    top.configure(bd=0, bg="yellow")
    top.focus_set()
    sc = ScrolledCanvas(top, bg="white", highlightthickness=0, takefocus=1)
    sc.frame.pack(expand=1, fill="both")
    item = make_objecttreeitem("sys", sys)
    node = TreeNode(sc.canvas, None, item)
    node.update()


if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_debugobj', verbosity=2, exit=False)
