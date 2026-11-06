"""Base class for the dialogs that wrap Tk's standard dialog commands
(tk_messageBox, tk_getOpenFile, tk_chooseColor, ...)."""

__all__ = ["Dialog"]

from tkinter import _get_temp_root, _destroy_temp_root


class Dialog:
    """A modal Tk standard dialog.

    Subclasses set `command` to the Tk command and may override
    _fixoptions() (adjust the options before the call) and _fixresult()
    (convert the result).
    """

    command = None

    def __init__(self, master=None, **options):
        if master is None:
            master = options.get('parent')
        self.master = master
        self.options = options

    def _fixoptions(self):
        pass

    def _fixresult(self, widget, result):
        return result

    def _test_callback(self, master):
        # Hook called just before the dialog is shown (used by tests).
        pass

    def show(self, **options):
        """Show the dialog and return its (converted) result."""
        self.options.update(options)
        self._fixoptions()
        master = self.master
        if master is None:
            master = _get_temp_root()
        try:
            self._test_callback(master)
            result = master.tk.call(self.command, *master._options(self.options))
            result = self._fixresult(master, result)
        finally:
            _destroy_temp_root(master)
        return result
