"""Simple modal dialogs.

SimpleDialog -- a message with a row of buttons (returns a button index).
Dialog       -- base class for custom modal dialogs with OK/Cancel buttons.
askinteger(), askfloat(), askstring() -- ask the user for one value.
"""

from tkinter import *
from tkinter import _get_temp_root, _destroy_temp_root
from tkinter import messagebox


class SimpleDialog:
    """A message with buttons; go() waits for a choice and returns the
    index of the button pressed."""

    def __init__(self, master, text='', buttons=[], default=None, cancel=None,
                 title=None, class_=None):
        if class_:
            self.root = Toplevel(master, class_=class_)
        else:
            self.root = Toplevel(master)
        if title:
            self.root.title(title)
            self.root.iconname(title)
        _setup_dialog(self.root)
        self.message = Message(self.root, text=text, aspect=400)
        self.message.pack(expand=1, fill=BOTH)
        self.frame = Frame(self.root)
        self.frame.pack()
        self.num = default
        self.cancel = cancel
        self.default = default
        self.root.bind('<Return>', self.return_event)
        for num, label in enumerate(buttons):
            b = Button(self.frame, text=label,
                       command=lambda num=num: self.done(num))
            if num == default:
                b.config(relief=RIDGE, borderwidth=8)
            b.pack(side=LEFT, fill=BOTH, expand=1)
        self.root.protocol('WM_DELETE_WINDOW', self.wm_delete_window)
        self.root.transient(master)
        _place_window(self.root, master)

    def go(self):
        """Run the dialog until a button is chosen; return its index."""
        self.root.wait_visibility()
        self.root.grab_set()
        self.root.mainloop()
        self.root.destroy()
        return self.num

    def return_event(self, event):
        if self.default is None:
            self.root.bell()
        else:
            self.done(self.default)

    def wm_delete_window(self):
        if self.cancel is None:
            self.root.bell()
        else:
            self.done(self.cancel)

    def done(self, num):
        self.num = num
        self.root.quit()


class Dialog(Toplevel):
    """Base class for custom modal dialogs.

    Override body() to create the contents, validate() to check them and
    apply() to process them; `result` holds the outcome (None when
    cancelled).
    """

    def __init__(self, parent, title=None):
        """Create, show and run the dialog (returns when it is closed)."""
        master = parent
        if master is None:
            master = _get_temp_root()
        Toplevel.__init__(self, master)
        self.withdraw()
        if parent is not None and parent.winfo_viewable():
            self.transient(parent)
        if title:
            self.title(title)
        _setup_dialog(self)
        self.parent = parent
        self.result = None
        body = Frame(self)
        self.initial_focus = self.body(body)
        body.pack(padx=5, pady=5)
        self.buttonbox()
        if self.initial_focus is None:
            self.initial_focus = self
        self.protocol("WM_DELETE_WINDOW", self.cancel)
        _place_window(self, parent)
        self.initial_focus.focus_set()
        # the window must be visible before it can grab the input
        self.wait_visibility()
        self.grab_set()
        self.wait_window(self)

    def destroy(self):
        """Destroy the dialog window."""
        self.initial_focus = None
        Toplevel.destroy(self)
        _destroy_temp_root(self.master)

    def body(self, master):
        """Create the dialog body in *master*; return the widget that
        should get the initial focus (or None)."""
        pass

    def buttonbox(self):
        """Create the OK and Cancel buttons."""
        box = Frame(self)
        w = Button(box, text="OK", width=10, command=self.ok, default=ACTIVE)
        w.pack(side=LEFT, padx=5, pady=5)
        w = Button(box, text="Cancel", width=10, command=self.cancel)
        w.pack(side=LEFT, padx=5, pady=5)
        self.bind("<Return>", self.ok)
        self.bind("<Escape>", self.cancel)
        box.pack()

    def ok(self, event=None):
        """Validate, apply and close."""
        if not self.validate():
            self.initial_focus.focus_set()
            return
        self.withdraw()
        self.update_idletasks()
        try:
            self.apply()
        finally:
            self.cancel()

    def cancel(self, event=None):
        """Close the dialog."""
        if self.parent is not None:
            self.parent.focus_set()
        self.destroy()

    def validate(self):
        """Return true if the dialog contents are acceptable."""
        return 1

    def apply(self):
        """Process the dialog contents (called after validation)."""
        pass


def _setup_dialog(w):
    if w._windowingsystem == "aqua":
        w.tk.call("::tk::unsupported::MacWindowStyle", "style",
                  w, "moveableModal", "")
    elif w._windowingsystem == "x11":
        w.wm_attributes(type="dialog")


def _place_window(w, parent=None):
    """Center *w* over *parent* (or the screen) and show it."""
    w.wm_withdraw()
    w.update_idletasks()
    minwidth = w.winfo_reqwidth()
    minheight = w.winfo_reqheight()
    maxwidth = w.winfo_vrootwidth()
    maxheight = w.winfo_vrootheight()
    if parent is not None and parent.winfo_ismapped():
        x = parent.winfo_rootx() + (parent.winfo_width() - minwidth) // 2
        y = parent.winfo_rooty() + (parent.winfo_height() - minheight) // 2
        vrootx = w.winfo_vrootx()
        vrooty = w.winfo_vrooty()
        x = max(min(x, vrootx + maxwidth - minwidth), vrootx)
        y = max(min(y, vrooty + maxheight - minheight), vrooty)
        if w._windowingsystem == 'aqua':
            # leave room for the menu bar
            y = max(y, 22)
    else:
        x = (w.winfo_screenwidth() - minwidth) // 2
        y = (w.winfo_screenheight() - minheight) // 2
    w.wm_maxsize(maxwidth, maxheight)
    w.wm_geometry('+%d+%d' % (x, y))
    w.wm_deiconify()


class _QueryDialog(Dialog):

    def __init__(self, title, prompt, initialvalue=None, minvalue=None,
                 maxvalue=None, parent=None):
        self.prompt = prompt
        self.minvalue = minvalue
        self.maxvalue = maxvalue
        self.initialvalue = initialvalue
        Dialog.__init__(self, parent, title)

    def destroy(self):
        self.entry = None
        Dialog.destroy(self)

    def body(self, master):
        w = Label(master, text=self.prompt, justify=LEFT)
        w.grid(row=0, padx=5, sticky=W)
        self.entry = Entry(master, name="entry")
        self.entry.grid(row=1, padx=5, sticky=W + E)
        if self.initialvalue is not None:
            self.entry.insert(0, self.initialvalue)
            self.entry.select_range(0, END)
        return self.entry

    def validate(self):
        try:
            result = self.getresult()
        except ValueError:
            messagebox.showwarning("Illegal value",
                                   self.errormessage + "\nPlease try again",
                                   parent=self)
            return 0
        if self.minvalue is not None and result < self.minvalue:
            messagebox.showwarning("Too small",
                                   "The allowed minimum value is %s. "
                                   "Please try again." % self.minvalue,
                                   parent=self)
            return 0
        if self.maxvalue is not None and result > self.maxvalue:
            messagebox.showwarning("Too large",
                                   "The allowed maximum value is %s. "
                                   "Please try again." % self.maxvalue,
                                   parent=self)
            return 0
        self.result = result
        return 1


class _QueryInteger(_QueryDialog):
    errormessage = "Not an integer."

    def getresult(self):
        return self.getint(self.entry.get())


def askinteger(title, prompt, **kw):
    """Ask for an integer; return it or None.

    Keywords: initialvalue, minvalue, maxvalue, parent."""
    d = _QueryInteger(title, prompt, **kw)
    return d.result


class _QueryFloat(_QueryDialog):
    errormessage = "Not a floating-point value."

    def getresult(self):
        return self.getdouble(self.entry.get())


def askfloat(title, prompt, **kw):
    """Ask for a float; return it or None.

    Keywords: initialvalue, minvalue, maxvalue, parent."""
    d = _QueryFloat(title, prompt, **kw)
    return d.result


class _QueryString(_QueryDialog):

    def __init__(self, *args, **kw):
        self.__show = kw.pop("show", None)
        _QueryDialog.__init__(self, *args, **kw)

    def body(self, master):
        entry = _QueryDialog.body(self, master)
        if self.__show is not None:
            entry.configure(show=self.__show)
        return entry

    def getresult(self):
        return self.entry.get()


def askstring(title, prompt, **kw):
    """Ask for a string; return it or None.

    Keywords: initialvalue, show, parent."""
    d = _QueryString(title, prompt, **kw)
    return d.result


if __name__ == '__main__':

    def test():
        root = Tk()
        def doit(root=root):
            d = SimpleDialog(root, text="This is a test dialog.",
                             buttons=["Yes", "No", "Cancel"],
                             default=0, cancel=2, title="Test Dialog")
            print(d.go())
            print(askinteger("Spam", "Egg count", initialvalue=12*12))
            print(askfloat("Spam", "Egg weight\n(in tons)", minvalue=1,
                           maxvalue=100))
            print(askstring("Spam", "Egg label"))
        t = Button(root, text='Test', command=doit)
        t.pack()
        q = Button(root, text='Quit', command=t.quit)
        q.pack()
        t.mainloop()

    test()
