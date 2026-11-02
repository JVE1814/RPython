"""A listbox with a scrollbar, single selection and a context menu.

Subclasses react to selections through on_select(index) and
on_double(index) and fill the context menu in fill_menu().
"""
from tkinter import *
from tkinter.ttk import Frame, Scrollbar

from idlelib import macosx


class ScrolledList:

    default = "(None)"  # shown when the list is empty
    menu = None

    def __init__(self, master, **options):
        self.master = master
        self.frame = Frame(master)
        self.frame.pack(fill="both", expand=1)
        self.vbar = Scrollbar(self.frame, name="vbar")
        self.vbar.pack(side="right", fill="y")
        self.listbox = Listbox(self.frame, exportselection=0, background="white")
        if options:
            self.listbox.configure(options)
        self.listbox.pack(expand=1, fill="both")
        self.vbar["command"] = self.listbox.yview
        self.listbox["yscrollcommand"] = self.vbar.set
        self.listbox.bind("<ButtonRelease-1>", self.click_event)
        self.listbox.bind("<Double-ButtonRelease-1>", self.double_click_event)
        if macosx.isAquaTk():
            self.listbox.bind("<ButtonPress-2>", self.popup_event)
            self.listbox.bind("<Control-Button-1>", self.popup_event)
        else:
            self.listbox.bind("<ButtonPress-3>", self.popup_event)
        self.listbox.bind("<Key-Up>", self.up_event)
        self.listbox.bind("<Key-Down>", self.down_event)
        self.clear()

    def close(self):
        self.frame.destroy()

    def clear(self):
        """Remove all items (the list shows the default text)."""
        self.listbox.delete(0, "end")
        self.empty = 1
        self.listbox.insert("end", self.default)

    def append(self, item):
        if self.empty:
            self.listbox.delete(0, "end")
            self.empty = 0
        self.listbox.insert("end", str(item))

    def get(self, index):
        return self.listbox.get(index)

    def _activate_at(self, event):
        self.listbox.activate("@%d,%d" % (event.x, event.y))
        index = self.listbox.index("active")
        self.select(index)
        return index

    def click_event(self, event):
        self.on_select(self._activate_at(event))
        return "break"

    def double_click_event(self, event):
        index = self.listbox.index("active")
        self.select(index)
        self.on_double(index)
        return "break"

    def popup_event(self, event):
        if not self.menu:
            self.make_menu()
        self._activate_at(event)
        self.menu.tk_popup(event.x_root, event.y_root)
        return "break"

    def make_menu(self):
        self.menu = Menu(self.listbox, tearoff=0)
        self.fill_menu()

    def _move(self, step):
        if self.listbox.curselection():
            index = self.listbox.index("active") + step
        else:
            index = 0 if step > 0 else self.listbox.size() - 1
        if not 0 <= index < self.listbox.size():
            self.listbox.bell()
            return "break"
        self.select(index)
        self.on_select(index)
        return "break"

    def up_event(self, event):
        return self._move(-1)

    def down_event(self, event):
        return self._move(1)

    def select(self, index):
        """Select and show the item at *index*."""
        self.listbox.focus_set()
        self.listbox.activate(index)
        self.listbox.selection_clear(0, "end")
        self.listbox.selection_set(index)
        self.listbox.see(index)

    # Methods for subclasses

    def fill_menu(self):
        pass

    def on_select(self, index):
        pass

    def on_double(self, index):
        pass


def _scrolled_list(parent):  # htest #
    top = Toplevel(parent)
    x, y = map(int, parent.geometry().split('+')[1:])
    top.geometry("+%d+%d" % (x + 200, y + 175))

    class MyScrolledList(ScrolledList):
        def fill_menu(self):
            self.menu.add_command(label="right click")

        def on_select(self, index):
            print("select", self.get(index))

        def on_double(self, index):
            print("double", self.get(index))

    scrolled_list = MyScrolledList(top)
    for i in range(30):
        scrolled_list.append("Item %02d" % i)


if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_scrolledlist', verbosity=2, exit=False)
