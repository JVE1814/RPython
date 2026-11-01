"""A tree widget drawn on a canvas (used by the module/path browsers,
the stack viewer and the debugger's object viewer).

TreeItem is the interface of the objects shown; TreeNode draws an item
and its expanded children; ScrolledCanvas is a canvas with scrollbars.
"""
import os

from tkinter import *
from tkinter.ttk import Frame, Scrollbar

from idlelib.config import idleConf
from idlelib import util, zoomheight

ICONDIR = "Icons"

# Look for the icons next to this file (the package may be moved).
try:
    _icondir = os.path.join(os.path.dirname(__file__), ICONDIR)
except NameError:
    _icondir = ICONDIR
if os.path.isdir(_icondir):
    ICONDIR = _icondir
elif not os.path.isdir(ICONDIR):
    raise RuntimeError(f"can't find icon directory ({ICONDIR!r})")


def listicons(icondir=ICONDIR):
    """Show all icons of *icondir* in a window (for testing)."""
    root = Tk()
    import glob
    names = sorted(glob.glob(os.path.join(icondir, "*.gif")))
    images = []
    for i, name in enumerate(names):
        image = PhotoImage(file=name, master=root)
        images.append(image)
        label = Label(root, image=image, bd=1, relief="raised")
        label.grid(row=i // 10 * 2, column=i % 10)
        label = Label(root, text=os.path.splitext(os.path.basename(name))[0])
        label.grid(row=i // 10 * 2 + 1, column=i % 10)
    root.images = images


# Simple built-in icons, drawn when no image file is found: a list of
# (color, [rows]) where each row is a string of '.' (transparent) or 'x'.
_BUILTIN_ICONS = {
    'folder': ('#c08000', ["..............", ".xxxx.........", "xxxxxxxxxxxxx.",
                           "x...........x.", "x...........x.", "x...........x.",
                           "x...........x.", "x...........x.", "xxxxxxxxxxxxx."]),
    'openfolder': ('#c08000', ["..............", ".xxxx.........", "xxxxxxxxxxxxx.",
                               "x............", "x..xxxxxxxxxxx", "x.x.........x",
                               "xx.........x.", "x.........x..", "xxxxxxxxxxx.."]),
    'python': ('#306090', ["....xxxxx.....", "...x..xxxx....", "...xxxxxxx....",
                           ".xxxxxxxxxxx..", "xxxxxxxxxxxxx.", "xxxxxxxxxxxxx.",
                           ".xxxxxxxxxxx..", "...xxxxxxx....", "....xxxxx....."]),
    'minusnode': ('#000000', ["xxxxxxxxx", "x.......x", "x.......x", "x.......x",
                              "x.xxxxx.x", "x.......x", "x.......x", "x.......x",
                              "xxxxxxxxx"]),
    'plusnode': ('#000000', ["xxxxxxxxx", "x.......x", "x...x...x", "x...x...x",
                             "x.xxxxx.x", "x...x...x", "x...x...x", "x.......x",
                             "xxxxxxxxx"]),
    'tk': ('#808080', ["xxxxxxxxxxx", "x.........x", "x.x.....x.x", "x..x...x..x",
                       "x...x.x...x", "x....x....x", "x...x.x...x", "x.........x",
                       "xxxxxxxxxxx"]),
}


def _builtin_icon(name, master):
    color, rows = _BUILTIN_ICONS.get(name, _BUILTIN_ICONS['tk'])
    image = PhotoImage(master=master, width=len(rows[0]), height=len(rows))
    for y, row in enumerate(rows):
        for x, c in enumerate(row):
            if c == 'x':
                image.put(color, (x, y))
    return image


def wheel_event(event, widget=None):
    """Scroll the tree's canvas (also when the wheel is used over an
    item label)."""
    return util.wheel_event(event, widget)


class TreeNode:
    """A node of the tree: draws its item and, when expanded, the
    children."""

    dy = 0

    def __init__(self, canvas, parent, item):
        self.canvas = canvas
        self.parent = parent
        self.item = item
        self.state = 'collapsed'
        self.selected = False
        self.children = []
        self.x = self.y = None
        self.iconimages = {}  # cache of PhotoImage instances for icons

    def destroy(self):
        for c in self.children[:]:
            self.children.remove(c)
            c.destroy()
        self.parent = None

    def geticonimage(self, name):
        try:
            return self.iconimages[name]
        except KeyError:
            pass
        file, ext = os.path.splitext(name)
        ext = ext or ".gif"
        fullname = os.path.join(ICONDIR, file + ext)
        if os.path.exists(fullname):
            image = PhotoImage(master=self.canvas, file=fullname)
        else:
            image = _builtin_icon(file, self.canvas)
        self.iconimages[name] = image
        return image

    def select(self, event=None):
        if self.selected:
            return
        self.deselectall()
        self.selected = True
        self.canvas.delete(self.image_id)
        self.drawicon()
        self.drawtext()

    def deselect(self, event=None):
        if not self.selected:
            return
        self.selected = False
        self.canvas.delete(self.image_id)
        self.drawicon()
        self.drawtext()

    def deselectall(self):
        if self.parent:
            self.parent.deselectall()
        else:
            self.deselecttree()

    def deselecttree(self):
        if self.selected:
            self.deselect()
        for child in self.children:
            child.deselecttree()

    def flip(self, event=None):
        if self.state == 'expanded':
            self.collapse()
        else:
            self.expand()
        self.item.OnDoubleClick()
        return "break"

    def expand(self, event=None):
        if not self.item._IsExpandable():
            return
        if self.state != 'expanded':
            self.state = 'expanded'
            self.update()
            self.view()

    def collapse(self, event=None):
        if self.state != 'collapsed':
            self.state = 'collapsed'
            self.update()

    def view(self):
        """Scroll so that this node and its children are visible."""
        top = self.y - 2
        bottom = self.lastvisiblechild().y + 17
        height = bottom - top
        visible_top = self.canvas.canvasy(0)
        visible_height = self.canvas.winfo_height()
        visible_bottom = self.canvas.canvasy(visible_height)
        if visible_top <= top and bottom <= visible_bottom:
            return
        x0, y0, x1, y1 = self.canvas._getints(self.canvas['scrollregion'])
        if top >= visible_top and height <= visible_height:
            fraction = top + height - visible_height
        else:
            fraction = top
        fraction = float(fraction) / y1
        self.canvas.yview_moveto(fraction)

    def lastvisiblechild(self):
        if self.children and self.state == 'expanded':
            return self.children[-1].lastvisiblechild()
        return self

    def update(self):
        """Redraw the whole tree."""
        if self.parent:
            self.parent.update()
            return
        oldcursor = self.canvas['cursor']
        self.canvas['cursor'] = "watch"
        self.canvas.update()
        self.canvas.delete(ALL)
        self.draw(7, 2)
        x0, y0, x1, y1 = self.canvas.bbox(ALL)
        self.canvas.configure(scrollregion=(0, 0, x1, y1))
        self.canvas['cursor'] = oldcursor

    def draw(self, x, y):
        """Draw the node at (x, y); return the y of the next node."""
        dy = 20
        self.x, self.y = x, y
        self.drawicon()
        self.drawtext()
        if self.state != 'expanded':
            return y + dy
        if not self.children:
            sublist = self.item._GetSubList()
            if not sublist:
                # the item turned out to have no children
                return y + 17
            for item in sublist:
                self.children.append(self.__class__(self.canvas, self, item))
        cx = x + 20
        cy = y + dy
        cylast = 0
        for child in self.children:
            cylast = cy
            self.canvas.create_line(x + 9, cy + 7, cx, cy + 7, fill="gray50")
            cy = child.draw(cx, cy)
            if child.item._IsExpandable():
                if child.state == 'expanded':
                    iconname = "minusnode"
                    callback = child.collapse
                else:
                    iconname = "plusnode"
                    callback = child.expand
                image = self.geticonimage(iconname)
                id = self.canvas.create_image(x + 9, cylast + 7, image=image)
                self.canvas.tag_bind(id, "<1>", callback)
                self.canvas.tag_bind(id, "<Double-1>", lambda x: None)
        id = self.canvas.create_line(x + 9, y + 10, x + 9, cylast + 7, fill="gray50")
        self.canvas.tag_lower(id)
        return cy

    def drawicon(self):
        if self.selected:
            imagename = (self.item.GetSelectedIconName() or
                         self.item.GetIconName() or "openfolder")
        else:
            imagename = self.item.GetIconName() or "folder"
        image = self.geticonimage(imagename)
        id = self.canvas.create_image(self.x, self.y, anchor="nw", image=image)
        self.image_id = id
        self.canvas.tag_bind(id, "<1>", self.select)
        self.canvas.tag_bind(id, "<Double-1>", self.flip)

    def drawtext(self):
        textx = self.x + 20 - 1
        texty = self.y - 4
        labeltext = self.item.GetLabelText()
        if labeltext:
            id = self.canvas.create_text(textx, texty, anchor="nw", text=labeltext)
            self.canvas.tag_bind(id, "<1>", self.select)
            self.canvas.tag_bind(id, "<Double-1>", self.flip)
            x0, y0, x1, y1 = self.canvas.bbox(id)
            textx = max(x1, 200) + 10
        text = self.item.GetText() or "<no text>"
        try:
            self.entry
        except AttributeError:
            pass
        else:
            self.edit_finish()
        try:
            self.label
        except AttributeError:
            self.label = Label(self.canvas, text=text, bd=0, padx=2, pady=2)
        theme = idleConf.CurrentTheme()
        if self.selected:
            self.label.configure(idleConf.GetHighlight(theme, 'hilite'))
        else:
            self.label.configure(idleConf.GetHighlight(theme, 'normal'))
        id = self.canvas.create_window(textx, texty, anchor="nw", window=self.label)
        self.label.bind("<1>", self.select_or_edit)
        self.label.bind("<Double-1>", self.flip)
        self.label.bind("<MouseWheel>", lambda e: wheel_event(e, self.canvas))
        if self.label._windowingsystem == 'x11':
            self.label.bind("<Button-4>", lambda e: wheel_event(e, self.canvas))
            self.label.bind("<Button-5>", lambda e: wheel_event(e, self.canvas))
        self.text_id = id

    def select_or_edit(self, event=None):
        if self.selected and self.item.IsEditable():
            self.edit(event)
        else:
            self.select(event)

    def edit(self, event=None):
        self.entry = Entry(self.label, bd=0, highlightthickness=1, width=0)
        self.entry.insert(0, self.label['text'])
        self.entry.selection_range(0, END)
        self.entry.pack(ipadx=5)
        self.entry.focus_set()
        self.entry.bind("<Return>", self.edit_finish)
        self.entry.bind("<Escape>", self.edit_cancel)

    def edit_finish(self, event=None):
        try:
            entry = self.entry
            del self.entry
        except AttributeError:
            return
        text = entry.get()
        entry.destroy()
        if text and text != self.item.GetText():
            self.item.SetText(text)
        text = self.item.GetText()
        self.label['text'] = text
        self.drawtext()
        self.canvas.focus_set()

    def edit_cancel(self, event=None):
        try:
            entry = self.entry
            del self.entry
        except AttributeError:
            return
        entry.destroy()
        self.drawtext()
        self.canvas.focus_set()


class TreeItem:
    """The interface of the objects shown in a tree (subclass it)."""

    def __init__(self):
        pass

    def GetText(self):
        """Return the text to show for this item."""

    def GetLabelText(self):
        """Return an optional label shown before the text."""

    expandable = None

    def _IsExpandable(self):
        """Cached IsExpandable()."""
        if self.expandable is None:
            self.expandable = self.IsExpandable()
        return self.expandable

    def IsExpandable(self):
        """Return whether the item has children."""
        return 1

    def _GetSubList(self):
        """GetSubList(), updating the expandable flag."""
        if not self.IsExpandable():
            return []
        sublist = self.GetSubList()
        if not sublist:
            self.expandable = 0
        return sublist

    def IsEditable(self):
        """Return whether the text can be edited."""

    def SetText(self, text):
        """Change the item's text (if it is editable)."""

    def GetIconName(self):
        """Return the name of the icon (without extension)."""

    def GetSelectedIconName(self):
        """Return the name of the icon of a selected item."""

    def GetSubList(self):
        """Return the child items."""

    def OnDoubleClick(self):
        """Called on a double-click on the item."""


class FileTreeItem(TreeItem):
    """A file system path as a tree item."""

    def __init__(self, path):
        self.path = path

    def GetText(self):
        return os.path.basename(self.path) or self.path

    def IsEditable(self):
        return os.path.basename(self.path) != ""

    def SetText(self, text):
        newpath = os.path.dirname(self.path)
        newpath = os.path.join(newpath, text)
        if os.path.dirname(newpath) != os.path.dirname(self.path):
            return
        try:
            os.rename(self.path, newpath)
            self.path = newpath
        except OSError:
            pass

    def GetIconName(self):
        if not self.IsExpandable():
            return "python"

    def IsExpandable(self):
        return os.path.isdir(self.path)

    def GetSubList(self):
        try:
            names = os.listdir(self.path)
        except OSError:
            return []
        names.sort(key=os.path.normcase)
        return [FileTreeItem(os.path.join(self.path, name)) for name in names]


class ScrolledCanvas:
    """A canvas in a frame with scrollbars."""

    def __init__(self, master, **opts):
        if 'yscrollincrement' not in opts:
            opts['yscrollincrement'] = 17
        self.master = master
        self.frame = Frame(master)
        self.frame.rowconfigure(0, weight=1)
        self.frame.columnconfigure(0, weight=1)
        self.canvas = Canvas(self.frame, **opts)
        self.canvas.grid(row=0, column=0, sticky="nsew")
        self.vbar = Scrollbar(self.frame, name="vbar")
        self.vbar.grid(row=0, column=1, sticky="nse")
        self.hbar = Scrollbar(self.frame, name="hbar", orient="horizontal")
        self.hbar.grid(row=1, column=0, sticky="ews")
        self.canvas['yscrollcommand'] = self.vbar.set
        self.vbar['command'] = self.canvas.yview
        self.canvas['xscrollcommand'] = self.hbar.set
        self.hbar['command'] = self.canvas.xview
        self.canvas.bind("<Key-Prior>", self.page_up)
        self.canvas.bind("<Key-Next>", self.page_down)
        self.canvas.bind("<Key-Up>", self.unit_up)
        self.canvas.bind("<Key-Down>", self.unit_down)
        util.bind_wheel(self.canvas, wheel_event)
        self.canvas.bind("<Alt-Key-2>", self.zoom_height)
        self.canvas.focus_set()

    def page_up(self, event):
        self.canvas.yview_scroll(-1, "page")
        return "break"

    def page_down(self, event):
        self.canvas.yview_scroll(1, "page")
        return "break"

    def unit_up(self, event):
        self.canvas.yview_scroll(-1, "unit")
        return "break"

    def unit_down(self, event):
        self.canvas.yview_scroll(1, "unit")
        return "break"

    def zoom_height(self, event):
        zoomheight.zoom_height(self.master)
        return "break"


def _tree_widget(parent):  # htest #
    top = Toplevel(parent)
    x, y = map(int, parent.geometry().split('+')[1:])
    top.geometry("+%d+%d" % (x + 50, y + 175))
    sc = ScrolledCanvas(top, bg="white", highlightthickness=0, takefocus=1)
    sc.frame.pack(expand=1, fill="both", side=LEFT)
    item = FileTreeItem(ICONDIR)
    node = TreeNode(sc.canvas, None, item)
    node.expand()


if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_tree', verbosity=2, exit=False)
