"""Intercept the Tcl commands of a Tk widget.

A Tk widget is a Tcl command named after its path.  WidgetRedirector
renames that command to PATH_orig and installs its own command in its
place, so that every operation on the widget -- whether it comes from
Python or from Tk's own bindings -- passes through dispatch().  Operations
registered with register() are handled by a Python function; all others go
to the original command.
"""
from tkinter import TclError


class WidgetRedirector:
    """Route a widget's Tcl operations through Python functions."""

    def __init__(self, widget):
        self._operations = {}
        self.widget = widget
        self.tk = tk = widget.tk
        path = widget._w
        self.orig = path + "_orig"
        tk.call("rename", path, self.orig)

        def dispatch(operation, *args):
            return self.dispatch(operation, *args)

        tk.createcommand(path, dispatch)

    def __repr__(self):
        return "%s(%s<%s>)" % (self.__class__.__name__,
                               self.widget.__class__.__name__, self.widget._w)

    def close(self):
        """Unregister all operations and restore the original command."""
        for operation in list(self._operations):
            self.unregister(operation)
        widget = self.widget
        tk = widget.tk
        path = widget._w
        tk.deletecommand(path)
        tk.call("rename", self.orig, path)
        del self.widget, self.tk

    def register(self, operation, function):
        """Handle *operation* (e.g. 'insert') with *function*.

        The function also becomes the widget's Python method of that name.
        Returns an OriginalCommand that performs the original operation.
        """
        self._operations[operation] = function
        setattr(self.widget, operation, function)
        return OriginalCommand(self, operation)

    def unregister(self, operation):
        """Stop handling *operation*; return its function (or None)."""
        function = self._operations.pop(operation, None)
        if function is None:
            return None
        try:
            delattr(self.widget, operation)
        except AttributeError:
            pass
        return function

    def dispatch(self, operation, *args):
        """Called for every Tcl operation on the widget.

        Errors from Tk are swallowed (the result is '') since an operation
        may legitimately fail, e.g. deleting a selection that is empty.
        """
        handler = self._operations.get(operation)
        try:
            if handler:
                return handler(*args)
            return self.tk.call((self.orig, operation) + args)
        except TclError:
            return ""


class OriginalCommand:
    """Callable performing *operation* with the widget's original command."""

    def __init__(self, redir, operation):
        self.redir = redir
        self.operation = operation
        self.tk = redir.tk
        self.orig = redir.orig
        self.tk_call = redir.tk.call
        self.orig_and_operation = (redir.orig, operation)

    def __repr__(self):
        return "%s(%r, %r)" % (self.__class__.__name__,
                               self.redir, self.operation)

    def __call__(self, *args):
        return self.tk_call(self.orig_and_operation + args)


def _widget_redirector(parent):  # htest #
    from tkinter import Toplevel, Text

    top = Toplevel(parent)
    top.title("Test WidgetRedirector")
    x, y = map(int, parent.geometry().split('+')[1:])
    top.geometry("+%d+%d" % (x, y + 175))
    text = Text(top)
    text.pack()
    text.focus_set()
    redir = WidgetRedirector(text)

    def my_insert(*args):
        print("insert", args)
        original_insert(*args)

    original_insert = redir.register("insert", my_insert)


if __name__ == "__main__":
    from unittest import main
    main('idlelib.idle_test.test_redirector', verbosity=2, exit=False)

    from idlelib.idle_test.htest import run
    run(_widget_redirector)
