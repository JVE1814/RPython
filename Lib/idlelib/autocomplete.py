"""Completion of names, attributes and file names in editor and shell.

Tab (or Edit > Show Completions) completes the word before the cursor; a
popup list (autocomplete_w) shows the choices.  After a '.' or a path
separator inside a string, the list also opens by itself after a delay.
Attribute names come from the user's process when there is one.
"""
import __main__
import keyword
import os
import string
import sys

# Completion modes.
ATTRS, FILES = 0, 1
from idlelib import autocomplete_w
from idlelib.config import idleConf
from idlelib.hyperparser import HyperParser

# Arguments of open_completions: (evalfuncs, complete, wantwin, mode).
FORCE = True, False, True, None    # Edit > Show Completions
TAB = False, True, True, None      # the Tab key
TRY_A = False, False, False, ATTRS  # automatic, after '.'
TRY_F = False, False, False, FILES  # automatic, after a path separator

# Characters of identifiers (non-ASCII ones are accepted too).
ID_CHARS = string.ascii_letters + string.digits + "_"

SEPS = f"{os.sep}{os.altsep if os.altsep else ''}"
TRIGGERS = f".{SEPS}"

# Keywords offered with the names of the main module (True, False and
# None are already builtins).
completion_kwds = [s for s in keyword.kwlist
                   if s not in {'True', 'False', 'None'}]
completion_kwds.extend(('match', 'case'))
completion_kwds.sort()


class AutoComplete:

    def __init__(self, editwin=None, tags=None):
        self.editwin = editwin
        if editwin is not None:  # not in the user process
            self.text = editwin.text
        self.tags = tags
        self.autocompletewindow = None
        # A pending automatic completion: the after() id and the cursor
        # position it was requested at.
        self._delayed_completion_id = None
        self._delayed_completion_index = None

    @classmethod
    def reload(cls):
        cls.popupwait = idleConf.GetOption(
            "extensions", "AutoComplete", "popupwait", type="int", default=0)

    def _make_autocomplete_window(self):
        return autocomplete_w.AutoCompleteWindow(self.text, tags=self.tags)

    def _remove_autocomplete_window(self, event=None):
        if self.autocompletewindow:
            self.autocompletewindow.hide_window()
            self.autocompletewindow = None

    def force_open_completions_event(self, event):
        """Show the completions (Edit > Show Completions)."""
        self.open_completions(FORCE)
        return "break"

    def autocomplete_event(self, event):
        """Complete at Tab (None lets Tab indent instead)."""
        if hasattr(event, "mc_state") and event.mc_state:
            return None  # Tab with a modifier
        if not self.text.get("insert linestart", "insert").strip():
            return None  # Tab in the indentation
        if self.autocompletewindow and self.autocompletewindow.is_active():
            self.autocompletewindow.complete()
            return "break"
        return "break" if self.open_completions(TAB) else None

    def try_open_completions_event(self, event=None):
        """After a trigger character, open the list after a delay."""
        lastchar = self.text.get("insert-1c")
        if lastchar in TRIGGERS:
            args = TRY_A if lastchar == "." else TRY_F
            self._delayed_completion_index = self.text.index("insert")
            if self._delayed_completion_id is not None:
                self.text.after_cancel(self._delayed_completion_id)
            self._delayed_completion_id = self.text.after(
                self.popupwait, self._delayed_open_completions, args)

    def _delayed_open_completions(self, args):
        self._delayed_completion_id = None
        # Only if the cursor did not move in the meantime.
        if self.text.index("insert") == self._delayed_completion_index:
            self.open_completions(args)

    def open_completions(self, args):
        """Open the completion list; return True if it was shown (or the
        word completed).

        *args* is (evalfuncs, complete, wantwin, mode): whether calls may
        be evaluated to find the object, whether a unique match is inserted
        directly, whether the window is wanted, and ATTRS/FILES/None.
        """
        evalfuncs, complete, wantwin, mode = args
        if self._delayed_completion_id is not None:
            self.text.after_cancel(self._delayed_completion_id)
            self._delayed_completion_id = None

        hp = HyperParser(self.editwin, "insert")
        curline = self.text.get("insert linestart", "insert")
        end = len(curline)
        if hp.is_in_string() and (not mode or mode == FILES):
            self._remove_autocomplete_window()
            mode = FILES
            # The name being typed starts after the last separator; the
            # directory to list is what comes before it in the string.
            start = end
            while start and curline[start - 1] not in "'\"" + SEPS:
                start -= 1
            comp_start = curline[start:end]
            string_start = start
            while string_start and curline[string_start - 1] not in "'\"":
                string_start -= 1
            comp_what = curline[string_start:start]
        elif hp.is_in_code() and (not mode or mode == ATTRS):
            self._remove_autocomplete_window()
            mode = ATTRS
            start = end
            while start and (curline[start - 1] in ID_CHARS or
                             ord(curline[start - 1]) > 127):
                start -= 1
            comp_start = curline[start:end]
            if start and curline[start - 1] == '.':
                # completing an attribute: find the object's expression
                hp.set_index("insert-%dc" % (len(curline) - (start - 1)))
                comp_what = hp.get_expression()
                if not comp_what or (not evalfuncs and '(' in comp_what):
                    return None
            else:
                comp_what = ""
        else:
            return None

        if complete and not comp_what and not comp_start:
            return None
        comp_lists = self.fetch_completions(comp_what, mode)
        if not comp_lists[0]:
            return None
        if (mode == FILES and complete and comp_start and
                not any(name.startswith(comp_start) for name in comp_lists[1])):
            # Tab after a word no file name starts with: nothing to do.
            return None
        self.autocompletewindow = self._make_autocomplete_window()
        return not self.autocompletewindow.show_window(
            comp_lists, "insert-%dc" % len(comp_start),
            complete, mode, wantwin)

    def fetch_completions(self, what, mode):
        """Return (short list, full list) of completions.

        For ATTRS, the names in the object *what* ('' for the main module,
        including builtins and keywords); the short list omits private names
        (or follows __all__).  For FILES, the entries of directory *what*;
        the short list omits hidden files.
        """
        try:
            rpcclt = self.editwin.flist.pyshell.interp.rpcclt
        except:
            rpcclt = None
        if rpcclt:
            return rpcclt.remotecall("exec", "get_the_completion_list",
                                     (what, mode), {})
        if mode == ATTRS:
            if what == "":
                namespace = {**__main__.__builtins__.__dict__,
                             **__main__.__dict__}
                bigl = list(namespace)
                bigl.extend(completion_kwds)
                bigl.sort()
                if "__all__" in namespace:
                    smalll = sorted(namespace["__all__"])
                else:
                    smalll = [s for s in bigl if s[:1] != '_']
            else:
                try:
                    entity = self.get_entity(what)
                    bigl = sorted(dir(entity))
                    if "__all__" in bigl:
                        smalll = sorted(entity.__all__)
                    else:
                        smalll = [s for s in bigl if s[:1] != '_']
                except:
                    return [], []
        elif mode == FILES:
            try:
                bigl = sorted(os.listdir(os.path.expanduser(what or ".")))
            except OSError:
                return [], []
            smalll = [s for s in bigl if s[:1] != '.']
        else:
            return [], []
        if not smalll:
            smalll = bigl
        return smalll, bigl

    def get_entity(self, name):
        """Return the object named by *name* (in __main__ or sys.modules)."""
        return eval(name, {**sys.modules, **__main__.__dict__})


AutoComplete.reload()

if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_autocomplete', verbosity=2)
