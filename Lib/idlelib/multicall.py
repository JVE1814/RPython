"""Widgets whose event handlers learn the modifier state of the event.

MultiCallCreator(widget_class) returns a subclass of a tkinter widget class
whose bind() passes events with two extra attributes: mc_type (one of the
MC_ event type constants) and mc_state (the modifier bits, see MC_SHIFT
...).  Key sequences added to virtual events that Tk rejects are reported
on stderr instead of raising, so that a bad user key binding does not stop
IDLE from starting.
"""
import sys

import tkinter

# event types
(MC_KEYPRESS, MC_KEYRELEASE, MC_BUTTONPRESS, MC_BUTTONRELEASE, MC_ACTIVATE,
 MC_CIRCULATE, MC_COLORMAP, MC_CONFIGURE, MC_DEACTIVATE, MC_DESTROY,
 MC_ENTER, MC_EXPOSE, MC_FOCUSIN, MC_FOCUSOUT, MC_GRAVITY, MC_LEAVE,
 MC_MAP, MC_MOTION, MC_MOUSEWHEEL, MC_PROPERTY, MC_REPARENT, MC_UNMAP,
 MC_VISIBILITY) = range(23)

# modifier bits (the X11 state bits; on macOS Tk maps Option and Command)
MC_SHIFT = 1 << 0
MC_CONTROL = 1 << 2
MC_ALT = 1 << 3
MC_META = 1 << 5
MC_OPTION = 1 << 6
MC_COMMAND = 1 << 7
_MODIFIER_MASK = (MC_SHIFT | MC_CONTROL | MC_ALT | MC_META | MC_OPTION |
                  MC_COMMAND)

_TYPE_BY_EVENT = {
    tkinter.EventType.KeyPress: MC_KEYPRESS,
    tkinter.EventType.KeyRelease: MC_KEYRELEASE,
    tkinter.EventType.ButtonPress: MC_BUTTONPRESS,
    tkinter.EventType.ButtonRelease: MC_BUTTONRELEASE,
    tkinter.EventType.Activate: MC_ACTIVATE,
    tkinter.EventType.Circulate: MC_CIRCULATE,
    tkinter.EventType.Colormap: MC_COLORMAP,
    tkinter.EventType.Configure: MC_CONFIGURE,
    tkinter.EventType.Deactivate: MC_DEACTIVATE,
    tkinter.EventType.Destroy: MC_DESTROY,
    tkinter.EventType.Enter: MC_ENTER,
    tkinter.EventType.Expose: MC_EXPOSE,
    tkinter.EventType.FocusIn: MC_FOCUSIN,
    tkinter.EventType.FocusOut: MC_FOCUSOUT,
    tkinter.EventType.Gravity: MC_GRAVITY,
    tkinter.EventType.Leave: MC_LEAVE,
    tkinter.EventType.Map: MC_MAP,
    tkinter.EventType.Motion: MC_MOTION,
    tkinter.EventType.MouseWheel: MC_MOUSEWHEEL,
    tkinter.EventType.Property: MC_PROPERTY,
    tkinter.EventType.Reparent: MC_REPARENT,
    tkinter.EventType.Unmap: MC_UNMAP,
    tkinter.EventType.Visibility: MC_VISIBILITY,
}

# One subclass per widget class.
_multicall_dict = {}


def _report_bad_binding(virtual, sequence, err):
    print(f"Warning: IDLE ignores the invalid key binding {sequence!r} "
          f"of action {virtual!r}: {err}", file=sys.stderr)


def MultiCallCreator(widget):
    """Return the MultiCall subclass of the tkinter widget class *widget*."""
    if widget in _multicall_dict:
        return _multicall_dict[widget]

    class MultiCall(widget):
        assert issubclass(widget, tkinter.Misc)

        def __init__(self, *args, **kwargs):
            widget.__init__(self, *args, **kwargs)
            # (sequence, Tcl command name) of each Python binding
            self.__binders = []

        def bind(self, sequence=None, func=None, add=None):
            if func is None or isinstance(func, str):
                return widget.bind(self, sequence, func, add)

            def handler(event, func=func):
                state = getattr(event, 'state', 0)
                event.mc_state = state & _MODIFIER_MASK if isinstance(state, int) else 0
                event.mc_type = _TYPE_BY_EVENT.get(getattr(event, 'type', None))
                return func(event)

            try:
                funcid = widget.bind(self, sequence, handler, add)
            except tkinter.TclError as err:
                _report_bad_binding(sequence, sequence, err)
                return None
            self.__binders.append((sequence, funcid))
            return funcid

        def unbind(self, sequence, funcid=None):
            self.__binders = [(s, f) for s, f in self.__binders
                              if not (s == sequence and
                                      (funcid is None or f == funcid))]
            try:
                widget.unbind(self, sequence, funcid)
            except tkinter.TclError:
                pass

        def event_add(self, virtual, *sequences):
            for seq in sequences:
                try:
                    widget.event_add(self, virtual, seq)
                except tkinter.TclError as err:
                    _report_bad_binding(virtual, seq, err)

        def event_delete(self, virtual, *sequences):
            for seq in sequences:
                try:
                    widget.event_delete(self, virtual, seq)
                except tkinter.TclError:
                    pass  # a sequence that was never added

        def __del__(self):
            self.__binders = []

    _multicall_dict[widget] = MultiCall
    return MultiCall


def _multi_call(parent):  # htest #
    top = tkinter.Toplevel(parent)
    top.title("Test MultiCall")
    x, y = map(int, parent.geometry().split('+')[1:])
    top.geometry("+%d+%d" % (x, y + 175))
    text = MultiCallCreator(tkinter.Text)(top)
    text.pack()
    text.focus_set()

    def bindseq(seq, n=[0]):
        def handler(event):
            print(seq)
        text.bind("<<handler%d>>" % n[0], handler)
        text.event_add("<<handler%d>>" % n[0], seq)
        n[0] += 1

    for seq in ("<Key>", "<Control-Key>", "<Alt-Key-a>", "<Control-Key-a>",
                "<Alt-Control-Key-a>", "<Key-b>", "<Control-Button-1>",
                "<Button-2>", "<Alt-Button-1>", "<FocusOut>", "<Enter>",
                "<Leave>"):
        bindseq(seq)


if __name__ == "__main__":
    from unittest import main
    main('idlelib.idle_test.test_multicall', verbosity=2, exit=False)
