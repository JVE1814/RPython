"""The list of IDLE's top-level windows, shown in the Window menu."""
from tkinter import Toplevel, TclError
import sys


class WindowList:
    """Registry of open windows and of callbacks run when it changes."""

    def __init__(self):
        self.dict = {}       # str(window) -> window
        self.callbacks = []

    def add(self, window):
        window.after_idle(self.call_callbacks)
        self.dict[str(window)] = window

    def delete(self, window):
        try:
            del self.dict[str(window)]
        except KeyError:
            pass  # already removed
        self.call_callbacks()

    def add_windows_to_menu(self, menu):
        """Add an item for every window, sorted by title, to *menu*."""
        entries = []
        for key, window in self.dict.items():
            try:
                title = window.get_title()
            except TclError:
                continue
            entries.append((title, key, window))
        entries.sort()
        for title, key, window in entries:
            menu.add_command(label=title, command=window.wakeup)

    def register_callback(self, callback):
        self.callbacks.append(callback)

    def unregister_callback(self, callback):
        try:
            self.callbacks.remove(callback)
        except ValueError:
            pass

    def call_callbacks(self):
        for callback in self.callbacks:
            try:
                callback()
            except:
                t, v, tb = sys.exc_info()
                print("warning: callback failed in WindowList", t, ":", v)


registry = WindowList()

add_windows_to_menu = registry.add_windows_to_menu
register_callback = registry.register_callback
unregister_callback = registry.unregister_callback


class ListedToplevel(Toplevel):
    """A top-level window listed in the Window menu; the application ends
    when the last one is closed."""

    def __init__(self, master, **kw):
        kw.setdefault('class_', 'Idle')
        Toplevel.__init__(self, master, kw)
        registry.add(self)
        self.focused_widget = self

    def destroy(self):
        registry.delete(self)
        Toplevel.destroy(self)
        # If this is Idle's last window then quit the mainloop
        # (Needed for clean exit on Windows 98)
        if not registry.dict:
            self.quit()

    def update_windowlist_registry(self, window):
        registry.call_callbacks()

    def get_title(self):
        # Subclasses may show something else than the window title.
        return self.wm_title()

    def wakeup(self):
        """Show the window (deiconified, raised and focused)."""
        try:
            if self.wm_state() == "iconic":
                self.wm_withdraw()
                self.wm_deiconify()
            self.tkraise()
            self.focused_widget.focus_set()
        except TclError:
            # This can happen when the window menu was torn off.
            # Simply ignore it.
            pass


if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_window', verbosity=2)
