"""Tooltips: small borderless windows shown next to a widget.

TooltipBase   -- shows a window at a position computed by subclasses.
OnHoverTooltipBase -- shown when the mouse rests on the widget.
Hovertip      -- a hover tooltip with a text label.
"""
from tkinter import *


class TooltipBase:
    """Base class: a borderless window placed relative to a widget.

    Subclasses define showcontents() to fill the window.
    """

    def __init__(self, anchor_widget):
        """Create a tooltip for *anchor_widget* (not shown yet)."""
        self.anchor_widget = anchor_widget
        self.tipwindow = None

    def __del__(self):
        self.hidetip()

    def showtip(self):
        """Show the tooltip (only once while it is shown)."""
        if self.tipwindow:
            return
        self.tipwindow = tw = Toplevel(self.anchor_widget)
        # no window manager decorations
        tw.wm_overrideredirect(1)
        try:
            # Make the window a "help" window on macOS (no shadow, no title).
            tw.tk.call("::tk::unsupported::MacWindowStyle", "style", tw._w,
                       "help", "noActivates")
        except TclError:
            pass
        self.position_window()
        self.showcontents()
        self.tipwindow.update_idletasks()  # needed on macOS
        self.tipwindow.lift()

    def position_window(self):
        """Move the window to the position returned by
        get_position() (relative to the anchor widget)."""
        x, y = self.get_position()
        root_x = self.anchor_widget.winfo_rootx() + x
        root_y = self.anchor_widget.winfo_rooty() + y
        self.tipwindow.wm_geometry("+%d+%d" % (root_x, root_y))

    def get_position(self):
        """Return (x, y) of the tooltip relative to the anchor widget:
        below its bottom-left corner by default."""
        return 20, self.anchor_widget.winfo_height() + 1

    def showcontents(self):
        """Fill self.tipwindow (subclasses must define this)."""
        raise NotImplementedError

    def hidetip(self):
        """Hide (destroy) the tooltip window."""
        tw = self.tipwindow
        self.tipwindow = None
        if tw:
            try:
                tw.destroy()
            except TclError:
                pass


class OnHoverTooltipBase(TooltipBase):
    """A tooltip that appears when the mouse rests on the anchor widget
    for *hover_delay* milliseconds (immediately for None)."""

    def __init__(self, anchor_widget, hover_delay=1000):
        super().__init__(anchor_widget)
        self.hover_delay = hover_delay
        self._after_id = None
        self._id1 = self.anchor_widget.bind("<Enter>", self._show_event)
        self._id2 = self.anchor_widget.bind("<Leave>", self._hide_event)
        self._id3 = self.anchor_widget.bind("<Button>", self._hide_event)

    def __del__(self):
        try:
            self.anchor_widget.unbind("<Enter>", self._id1)
            self.anchor_widget.unbind("<Leave>", self._id2)
            self.anchor_widget.unbind("<Button>", self._id3)
        except TclError:
            pass
        super().__del__()

    def _show_event(self, event=None):
        if self.hover_delay:
            self.schedule()
        else:
            self.showtip()

    def _hide_event(self, event=None):
        self.hidetip()

    def schedule(self):
        """Show the tooltip after the hover delay."""
        self.unschedule()
        self._after_id = self.anchor_widget.after(self.hover_delay,
                                                  self.showtip)

    def unschedule(self):
        """Cancel a scheduled showing."""
        after_id = self._after_id
        self._after_id = None
        if after_id:
            self.anchor_widget.after_cancel(after_id)

    def hidetip(self):
        try:
            self.unschedule()
        except TclError:
            pass
        super().hidetip()


class Hovertip(OnHoverTooltipBase):
    """A hover tooltip showing a text."""

    def __init__(self, anchor_widget, text, hover_delay=1000):
        """Show *text* when the mouse rests on *anchor_widget* for
        *hover_delay* ms (None or 0: at once)."""
        super().__init__(anchor_widget, hover_delay=hover_delay)
        self.text = text

    def showcontents(self):
        label = Label(self.tipwindow, text=self.text, justify=LEFT,
                      relief=SOLID, borderwidth=1)
        label.pack()


def _tooltip(parent):  # htest #
    top = Toplevel(parent)
    top.title("Test tooltip")
    x, y = map(int, parent.geometry().split('+')[1:])
    top.geometry("+%d+%d" % (x, y + 150))
    label = Label(top, text="Place your mouse over buttons")
    label.pack()
    button1 = Button(top, text="Button 1 -- 1/2 second hover delay")
    button1.pack()
    Hovertip(button1, "This is tooltip text for button1.", hover_delay=500)
    button2 = Button(top, text="Button 2 -- no hover delay")
    button2.pack()
    Hovertip(button2, "This is tooltip\ntext for button2.", hover_delay=None)


if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_tooltip', verbosity=2, exit=False)
