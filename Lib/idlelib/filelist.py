"""The set of open editor windows; a file is open in at most one."""
import os
from tkinter import messagebox


class FileList:
    """Open editor windows, by file name.

    dict maps canonical file names to windows; inversedict maps every
    window to its key (None for untitled windows); vars holds the Tk
    variables of the menu checkbuttons, shared by all windows.
    """

    from idlelib.editor import EditorWindow  # overridden by the shell

    def __init__(self, root):
        self.root = root
        self.dict = {}
        self.inversedict = {}
        self.vars = {}

    def open(self, filename, action=None):
        """Show the window of *filename*, opening it if needed.

        *action* (if given) is called with the file name instead of
        creating a new window.  Return the window, or None.
        """
        assert filename
        filename = self.canonize(filename)
        if os.path.isdir(filename):
            messagebox.showerror(
                "File Error",
                f"{filename!r} is a directory.",
                master=self.root)
            return None
        key = os.path.normcase(filename)
        if key in self.dict:
            edit = self.dict[key]
            edit.top.wakeup()
            return edit
        if action:
            # Let the caller open it (for example in the current window).
            return action(filename)
        edit = self.EditorWindow(self, filename, key)
        if edit.good_load:
            return edit
        edit._close()
        return None

    def gotofileline(self, filename, lineno=None):
        edit = self.open(filename)
        if edit is not None and lineno is not None:
            edit.gotoline(lineno)

    def new(self, filename=None):
        return self.EditorWindow(self, filename)

    def close_all_callback(self, *args, **kwds):
        """Close every window (each may ask about saving)."""
        for edit in list(self.inversedict):
            reply = edit.close()
            if reply == "cancel":
                break
        return "break"

    def unregister_maybe_terminate(self, edit):
        """Forget a closed window; quit when it was the last one."""
        try:
            key = self.inversedict[edit]
        except KeyError:
            print("Don't know this EditorWindow object.  (close)")
            return
        if key:
            del self.dict[key]
        del self.inversedict[edit]
        if not self.inversedict:
            self.root.quit()

    def filename_changed_edit(self, edit):
        """Update the key of a window whose file name changed."""
        edit.saved_change_hook()
        try:
            key = self.inversedict[edit]
        except KeyError:
            print("Don't know this EditorWindow object.  (rename)")
            return
        filename = edit.io.filename
        if not filename:
            if key:
                del self.dict[key]
            self.inversedict[edit] = None
            return
        filename = self.canonize(filename)
        newkey = os.path.normcase(filename)
        if newkey == key:
            return
        if newkey in self.dict:
            conflict = self.dict[newkey]
            self.inversedict[conflict] = None
            messagebox.showerror(
                "Name Conflict",
                f"You now have multiple edit windows open for {filename!r}",
                master=self.root)
        self.dict[newkey] = edit
        self.inversedict[edit] = newkey
        if key:
            try:
                del self.dict[key]
            except KeyError:
                pass

    def canonize(self, filename):
        """Return the absolute, normalized form of *filename*."""
        if not os.path.isabs(filename):
            try:
                pwd = os.getcwd()
            except OSError:
                pass
            else:
                filename = os.path.join(pwd, filename)
        return os.path.normpath(filename)



if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_filelist', verbosity=2)
