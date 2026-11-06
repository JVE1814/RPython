"""A Text widget with a vertical scrollbar.

ScrolledText is a Text widget placed in a Frame next to a Scrollbar.  The
keyword options configure the Text; the pack/grid/place methods act on the
frame, so the whole composite is laid out as one widget.
"""

from tkinter import Frame, Text, Scrollbar, Pack, Grid, Place
from tkinter.constants import RIGHT, LEFT, Y, BOTH

__all__ = ['ScrolledText']


class ScrolledText(Text):

    def __init__(self, master=None, **kw):
        self.frame = Frame(master)
        self.vbar = Scrollbar(self.frame)
        self.vbar.pack(side=RIGHT, fill=Y)
        kw['yscrollcommand'] = self.vbar.set
        Text.__init__(self, self.frame, **kw)
        self.pack(side=LEFT, fill=BOTH, expand=True)
        self.vbar['command'] = self.yview

        # Route the geometry-manager methods to the frame, except those that
        # Text itself defines and the configuration methods.
        text_methods = set(vars(Text))
        geometry = set(vars(Pack)) | set(vars(Grid)) | set(vars(Place))
        for name in geometry - text_methods:
            if name[0] != '_' and name not in ('config', 'configure'):
                setattr(self, name, getattr(self.frame, name))

    def __str__(self):
        return str(self.frame)


def example():
    from tkinter.constants import END

    stext = ScrolledText(bg='white', height=10)
    stext.insert(END, __doc__)
    stext.pack(fill=BOTH, side=LEFT, expand=True)
    stext.focus_set()
    stext.mainloop()


if __name__ == "__main__":
    example()
