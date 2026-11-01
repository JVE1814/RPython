"""Common base of the Find, Replace and Find in Files dialogs.

The dialog window is created on first open() and then only hidden and
shown again.  Layout: a grid of labelled entries and option groups on the
left (built by create_entries, create_option_buttons and
create_other_buttons) and a column of command buttons on the right.
"""
from tkinter import Toplevel
from tkinter.ttk import Frame, Entry, Label, Button, Checkbutton, Radiobutton
from tkinter.simpledialog import _setup_dialog


class SearchDialogBase:
    """Base class for the search dialogs.

    Subclasses define title, icon, needwrapbutton, default_command and
    possibly create_entries/create_other_buttons/create_command_buttons.
    """

    title = "Search Dialog"
    icon = "Search"
    needwrapbutton = 1  # not wanted by Find in Files

    def __init__(self, root, engine):
        """*engine* is the SearchEngine holding the shared search state."""
        self.root = root
        self.bell = root.bell
        self.engine = engine
        self.top = None
        self._trace = None

    def open(self, text, searchphrase=None):
        """Show the dialog for the Text widget *text*."""
        self.text = text
        if not self.top:
            self.create_widgets()
        else:
            self.top.deiconify()
            self.top.tkraise()
        self.top.transient(text.winfo_toplevel())
        if searchphrase:
            self.ent.delete(0, "end")
            self.ent.insert("end", searchphrase)
        self.ent.focus_set()
        self.ent.selection_range(0, "end")
        self.ent.icursor(0)
        self.top.grab_set()
        # Pattern errors are shown in the dialog while it is open; editing
        # the pattern removes the message.
        self.engine.error_handler = self.show_error
        if self._trace is None:
            self._trace = self.engine.patvar.trace_add('write', self._clear_error)

    def close(self, event=None):
        """Hide the dialog."""
        self.engine.error_handler = None
        if self._trace is not None:
            try:
                self.engine.patvar.trace_remove('write', self._trace)
            except Exception:
                pass
            self._trace = None
        if self.top:
            self.top.grab_release()
            self.top.transient('')
            self.top.withdraw()

    def show_error(self, msg, pos=None):
        """Show a pattern error and put the cursor at its position."""
        self.error_label['text'] = msg
        if pos is not None:
            self.ent.icursor(pos)
        self.bell()

    def _clear_error(self, *args):
        if self.top:
            self.error_label['text'] = ''

    def create_widgets(self):
        """Build the dialog window."""
        top = Toplevel(self.root)
        top.bind("<Return>", self.default_command)
        top.bind("<Escape>", self.close)
        top.protocol("WM_DELETE_WINDOW", self.close)
        top.wm_title(self.title)
        top.wm_iconname(self.icon)
        _setup_dialog(top)
        self.top = top
        self.frame = Frame(top, padding=5)
        self.frame.grid(sticky="nwes")
        top.grid_columnconfigure(0, weight=100)
        top.grid_rowconfigure(0, weight=100)

        self.row = 0
        self.frame.grid_columnconfigure(0, pad=2, weight=0)
        self.frame.grid_columnconfigure(1, pad=2, minsize=100, weight=100)

        self.create_entries()
        self.error_label = Label(self.frame, text='', foreground='red')
        self.error_label.grid(row=self.row, column=1, sticky="w")
        self.row += 1
        self.create_option_buttons()
        self.create_other_buttons()
        self.create_command_buttons()

    def make_entry(self, label_text, var):
        """Add a labelled entry for *var* in a new row; return (entry,
        label)."""
        label = Label(self.frame, text=label_text)
        label.grid(row=self.row, column=0, sticky="nw")
        entry = Entry(self.frame, textvariable=var, exportselection=0)
        entry.grid(row=self.row, column=1, sticky="nwe")
        self.row += 1
        return entry, label

    def create_entries(self):
        """Create the entry for the search pattern."""
        self.ent = self.make_entry("Find:", self.engine.patvar)[0]

    def make_frame(self, labeltext=None):
        """Add a frame (with a label if *labeltext*) in a new row; return
        (frame, label or '')."""
        if labeltext:
            label = Label(self.frame, text=labeltext)
            label.grid(row=self.row, column=0, sticky="nw")
        else:
            label = ''
        frame = Frame(self.frame)
        frame.grid(row=self.row, column=1, columnspan=1, sticky="nwe")
        self.row += 1
        return frame, label

    def create_option_buttons(self):
        """Add the Options checkbuttons; return (frame, [(var, label)])."""
        frame = self.make_frame("Options")[0]
        engine = self.engine
        options = [(engine.revar, "Regular expression"),
                   (engine.casevar, "Match case"),
                   (engine.wordvar, "Whole word")]
        if self.needwrapbutton:
            options.append((engine.wrapvar, "Wrap around"))
        for var, label in options:
            btn = Checkbutton(frame, variable=var, text=label)
            btn.pack(side="left", fill="both")
        return frame, options

    def create_other_buttons(self):
        """Add the Direction radiobuttons; return (frame, [(value, label)])."""
        frame = self.make_frame("Direction")[0]
        var = self.engine.backvar
        others = [(1, 'Up'), (0, 'Down')]
        for val, label in others:
            btn = Radiobutton(frame, variable=var, value=val, text=label)
            btn.pack(side="left", fill="both")
        return frame, others

    def make_button(self, label, command, isdef=0):
        """Add a command button to the button column."""
        b = Button(self.buttonframe, text=label, command=command,
                   default=isdef and "active" or "normal")
        cols, rows = self.buttonframe.grid_size()
        b.grid(pady=1, row=rows, column=0, sticky="ew")
        self.buttonframe.grid(rowspan=rows + 1)
        return b

    def create_command_buttons(self):
        """Create the button column with a Close button."""
        f = self.buttonframe = Frame(self.frame)
        f.grid(row=0, column=2, padx=2, pady=2, ipadx=2, ipady=2)
        b = self.make_button("Close", self.close)
        b.lower()


class _searchbase(SearchDialogBase):  # htest #
    "Create auto-opening dialog with no text connection."

    def __init__(self, parent):
        import re
        from idlelib import searchengine

        self.root = parent
        self.engine = searchengine.get(parent)
        self.create_widgets()
        print(parent.geometry())
        width, height, x, y = list(map(int, re.split('[x+]',
                                                      parent.geometry())))
        self.top.geometry("+%d+%d" % (x + 40, y + 175))

    def default_command(self, dummy):
        pass


if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_searchbase', verbosity=2, exit=False)
