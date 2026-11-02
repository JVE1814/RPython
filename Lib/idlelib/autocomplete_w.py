"""The completion popup: a list below the cursor with the possible
completions of the word being typed.

Typing narrows the selection; Tab completes the common prefix (twice:
the selection), Return or a double click inserts the selection, and
Escape, a click elsewhere or moving the cursor away closes the list.
The short list (without private names or hidden files) is replaced by the
full list when the typed text matches nothing in it.
"""
import platform

from tkinter import *
from tkinter.ttk import Scrollbar

from idlelib.autocomplete import FILES, ATTRS
from idlelib.multicall import MC_SHIFT

HIDE_VIRTUAL_EVENT_NAME = "<<autocompletewindow-hide>>"
HIDE_FOCUS_OUT_SEQUENCE = "<FocusOut>"
HIDE_SEQUENCES = (HIDE_FOCUS_OUT_SEQUENCE, "<ButtonPress>")
KEYPRESS_VIRTUAL_EVENT_NAME = "<<autocompletewindow-keypress>>"
KEYPRESS_SEQUENCES = ("<Key>", "<Key-BackSpace>", "<Key-Return>", "<Key-Tab>",
                      "<Key-Up>", "<Key-Down>", "<Key-Home>", "<Key-End>",
                      "<Key-Prior>", "<Key-Next>", "<Key-Escape>")
KEYRELEASE_VIRTUAL_EVENT_NAME = "<<autocompletewindow-keyrelease>>"
KEYRELEASE_SEQUENCE = "<KeyRelease>"
LISTUPDATE_SEQUENCE = "<B1-ButtonRelease>"
WINCONFIG_SEQUENCE = "<Configure>"
DOUBLECLICK_SEQUENCE = "<B1-Double-ButtonRelease>"

_MODIFIER_KEYS = ("Shift", "Control", "Alt", "Meta", "Command", "Option")
_MOVE_KEYS = ("Home", "End", "Prior", "Next", "Up", "Down")
# Keys that end a word: they close the list, inserting the selection when
# it extends what was typed.
_ATTR_END_KEYS = ("period", "space", "parenleft", "parenright",
                  "bracketleft", "bracketright")
_FILE_END_KEYS = ("slash", "backslash", "quotedbl", "apostrophe")
# Keys that add a character to the word.
_KEYSYM_CHARS = {"underscore": "_", "period": ".", "minus": "-"}


class AutoCompleteWindow:

    def __init__(self, widget, tags):
        """The list completes in the Text *widget*; inserted text gets
        *tags*."""
        self.widget = widget
        self.tags = tags
        self.autocompletewindow = self.listbox = self.scrollbar = None
        self.origselforeground = self.origselbackground = None
        self.completions = None       # the list shown
        self.morecompletions = None   # the full list, until shown
        self.mode = None              # ATTRS or FILES
        self.start = None             # the word as it is in the text
        self.startindex = None        # where the word begins
        self.lasttypedstart = None    # the word as typed by the user
        self.userwantswindow = None
        self.hideaid = self.hidewid = None
        self.keypressid = self.keyreleaseid = None
        self.listupdateid = self.winconfigid = self.doubleclickid = None
        self.lastkey_was_tab = False
        self.is_configuring = False

    # -- the word in the text

    def _change_start(self, newstart):
        """Make the word in the text *newstart*, editing only the part that
        differs."""
        common = 0
        limit = min(len(self.start), len(newstart))
        while common < limit and self.start[common] == newstart[common]:
            common += 1
        if common < len(self.start):
            self.widget.delete("%s+%dc" % (self.startindex, common),
                               "%s+%dc" % (self.startindex, len(self.start)))
        if common < len(newstart):
            self.widget.insert("%s+%dc" % (self.startindex, common),
                               newstart[common:], self.tags)
        self.start = newstart

    def _binary_search(self, s):
        """Index of the first completion not less than *s* (clamped to the
        last one)."""
        lo, hi = 0, len(self.completions)
        while lo < hi:
            mid = (lo + hi) // 2
            if self.completions[mid] < s:
                lo = mid + 1
            else:
                hi = mid
        return min(lo, len(self.completions) - 1)

    def _complete_string(self, s):
        """Return the longest prefix shared by all completions starting
        with *s* (*s* if none does)."""
        matches = [c for c in self.completions if c.startswith(s)]
        if not matches:
            return s
        first, last = matches[0], matches[-1]
        n = len(s)
        while n < min(len(first), len(last)) and first[n] == last[n]:
            n += 1
        return first[:n]

    def _selection_changed(self):
        """Update the word and the selection colors for the selection."""
        cursel = int(self.listbox.curselection()[0])
        self.listbox.see(cursel)
        typed = self.lasttypedstart
        selected = self.completions[cursel]
        if self._binary_search(typed) == cursel:
            newstart = typed
        else:
            n = 0
            while n < min(len(typed), len(selected)) and typed[n] == selected[n]:
                n += 1
            newstart = selected[:n]
        self._change_start(newstart)

        if self.completions[cursel].startswith(self.start):
            self.listbox.configure(selectbackground=self.origselbackground,
                                   selectforeground=self.origselforeground)
            return
        # Nothing matches: show the selection unhighlighted and switch to
        # the full list, if not shown yet.
        self.listbox.configure(selectbackground=self.listbox.cget("bg"),
                               selectforeground=self.listbox.cget("fg"))
        if self.morecompletions:
            self.completions = self.morecompletions
            self.morecompletions = None
            self.listbox.delete(0, END)
            for item in self.completions:
                self.listbox.insert(END, item)
            self.listbox.select_set(self._binary_search(self.start))
            self._selection_changed()

    def _select(self, index):
        self.listbox.select_clear(0, END)
        self.listbox.select_set(index)
        self._selection_changed()

    # -- showing and hiding

    def show_window(self, comp_lists, index, complete, mode, userWantsWin):
        """Show the list for the word starting at *index*.

        comp_lists is (short list, full list).  With *complete*, a word
        with a single completion is completed without showing the list;
        the result is then True if nothing changed.
        """
        self.completions, self.morecompletions = comp_lists
        self.mode = mode
        self.startindex = self.widget.index(index)
        self.start = self.widget.get(self.startindex, "insert")
        if complete:
            completed = self._complete_string(self.start)
            original = self.start
            self._change_start(completed)
            i = self._binary_search(completed)
            unique = (self.completions[i] == completed and
                      (i == len(self.completions) - 1 or
                       not self.completions[i + 1].startswith(completed)))
            if unique:
                return completed == original
        self.userwantswindow = userWantsWin
        self.lasttypedstart = self.start

        self.autocompletewindow = acw = Toplevel(self.widget)
        acw.withdraw()
        acw.wm_overrideredirect(1)
        try:
            # On macOS, do not take the focus from the text.
            acw.tk.call("::tk::unsupported::MacWindowStyle", "style", acw._w,
                        "help", "noActivates")
        except TclError:
            pass
        self.scrollbar = Scrollbar(acw, orient=VERTICAL)
        self.listbox = Listbox(acw, yscrollcommand=self.scrollbar.set,
                               exportselection=False)
        for item in self.completions:
            self.listbox.insert(END, item)
        self.origselforeground = self.listbox.cget("selectforeground")
        self.origselbackground = self.listbox.cget("selectbackground")
        self.scrollbar.config(command=self.listbox.yview)
        self.scrollbar.pack(side=RIGHT, fill=Y)
        self.listbox.pack(side=LEFT, fill=BOTH, expand=True)
        acw.update_idletasks()
        acw.lift()
        self.listbox.select_set(self._binary_search(self.start))
        self._selection_changed()
        self._bind_events()
        return None

    def _bind_events(self):
        acw = self.autocompletewindow
        self.hideaid = acw.bind(HIDE_VIRTUAL_EVENT_NAME, self.hide_event)
        self.hidewid = self.widget.bind(HIDE_VIRTUAL_EVENT_NAME, self.hide_event)
        acw.event_add(HIDE_VIRTUAL_EVENT_NAME, HIDE_FOCUS_OUT_SEQUENCE)
        for seq in HIDE_SEQUENCES:
            self.widget.event_add(HIDE_VIRTUAL_EVENT_NAME, seq)
        self.keypressid = self.widget.bind(KEYPRESS_VIRTUAL_EVENT_NAME,
                                           self.keypress_event)
        for seq in KEYPRESS_SEQUENCES:
            self.widget.event_add(KEYPRESS_VIRTUAL_EVENT_NAME, seq)
        self.keyreleaseid = self.widget.bind(KEYRELEASE_VIRTUAL_EVENT_NAME,
                                             self.keyrelease_event)
        self.widget.event_add(KEYRELEASE_VIRTUAL_EVENT_NAME, KEYRELEASE_SEQUENCE)
        self.listupdateid = self.listbox.bind(LISTUPDATE_SEQUENCE,
                                              self.listselect_event)
        self.is_configuring = False
        self.winconfigid = acw.bind(WINCONFIG_SEQUENCE, self.winconfig_event)
        self.doubleclickid = self.listbox.bind(DOUBLECLICK_SEQUENCE,
                                               self.doubleclick_event)

    def winconfig_event(self, event):
        """Place the list below the word (above it if there is no room)."""
        if self.is_configuring or not self.is_active():
            return
        self.is_configuring = True
        acw = self.autocompletewindow
        try:
            text = self.widget
            text.see(self.startindex)
            x, y, cx, cy = text.bbox(self.startindex)
            if platform.system().startswith('Windows'):
                acw.update()
            width, height = acw.winfo_width(), acw.winfo_height()
            new_x = text.winfo_rootx() + min(x, max(0, text.winfo_width() - width))
            new_y = text.winfo_rooty() + y
            if text.winfo_height() - (y + cy) >= height or y < height:
                new_y += cy
            else:
                new_y -= height
            acw.wm_geometry("+%d+%d" % (new_x, new_y))
            acw.deiconify()
            acw.update_idletasks()
        except TclError:
            pass  # the list was closed meanwhile
        if platform.system().startswith('Windows') and self.winconfigid:
            # Tk on Windows repeats <Configure>; placing once is enough.
            try:
                acw.unbind(WINCONFIG_SEQUENCE, self.winconfigid)
            except TclError:
                pass
            self.winconfigid = None
        self.is_configuring = False

    def _hide_event_check(self):
        if not self.autocompletewindow:
            return
        try:
            if not self.autocompletewindow.focus_get():
                self.hide_window()
        except KeyError:
            self.hide_window()

    def hide_event(self, event):
        if not self.is_active():
            return
        if event.type == EventType.FocusOut:
            # The focus may be moving to the list itself: check later.
            self.widget.after(1, self._hide_event_check)
        elif event.type == EventType.ButtonPress:
            self.hide_window()

    def listselect_event(self, event):
        if self.is_active():
            self.userwantswindow = True
            cursel = int(self.listbox.curselection()[0])
            self._change_start(self.completions[cursel])

    def doubleclick_event(self, event):
        cursel = int(self.listbox.curselection()[0])
        self._change_start(self.completions[cursel])
        self.hide_window()

    # -- keys

    def keypress_event(self, event):
        if not self.is_active():
            return None
        keysym = event.keysym
        state = getattr(event, "mc_state", 0)
        if keysym != "Tab":
            self.lastkey_was_tab = False
        plain = not (state & ~MC_SHIFT)

        typed = None
        if len(keysym) == 1:
            typed = keysym
        elif keysym == "underscore":
            typed = "_"
        elif self.mode == FILES and keysym in ("period", "minus"):
            typed = _KEYSYM_CHARS[keysym]
        if (typed is not None or keysym == "BackSpace") and plain:
            if typed is None:  # BackSpace
                if not self.start:
                    self.hide_window()
                    return None
                self._change_start(self.start[:-1])
            else:
                self._change_start(self.start + typed)
            self.lasttypedstart = self.start
            self._select(self._binary_search(self.start))
            return "break"

        if keysym == "Return":
            self.complete()
            self.hide_window()
            return "break"

        if ((self.mode == ATTRS and keysym in _ATTR_END_KEYS) or
                (self.mode == FILES and keysym in _FILE_END_KEYS and plain)):
            cursel = int(self.listbox.curselection()[0])
            selected = self.completions[cursel]
            if selected.startswith(self.start) and (self.mode == ATTRS or self.start):
                self._change_start(selected)
            self.hide_window()
            return None

        if keysym in _MOVE_KEYS and not state:
            self.userwantswindow = True
            cursel = int(self.listbox.curselection()[0])
            last = len(self.completions) - 1
            if keysym == "Home":
                newsel = 0
            elif keysym == "End":
                newsel = last
            elif keysym == "Up":
                newsel = max(0, cursel - 1)
            elif keysym == "Down":
                newsel = min(last, cursel + 1)
            else:
                page = (self.listbox.nearest(self.listbox.winfo_height()) -
                        self.listbox.nearest(0))
                if keysym == "Prior":
                    newsel = max(0, cursel - page)
                else:
                    newsel = min(last, cursel + page)
            self._select(newsel)
            self._change_start(self.completions[newsel])
            return "break"

        if keysym == "Tab" and not state:
            if self.lastkey_was_tab:
                # second Tab: take the selection
                cursel = int(self.listbox.curselection()[0])
                self._change_start(self.completions[cursel])
                self.hide_window()
                return "break"
            # first Tab: AutoComplete completes the common prefix
            self.userwantswindow = True
            self.lastkey_was_tab = True
            return None

        if any(name in keysym for name in _MODIFIER_KEYS):
            return None

        if event.char and event.char >= ' ':
            # a printable character without a one-character keysym
            self._change_start(self.start + event.char)
            self.lasttypedstart = self.start
            self._select(self._binary_search(self.start))
            return "break"

        # Any other key closes the list and has its normal effect.
        self.hide_window()
        return None

    def keyrelease_event(self, event):
        if not self.is_active():
            return
        end_of_word = self.widget.index("%s+%dc" % (self.startindex,
                                                    len(self.start)))
        if self.widget.index("insert") != end_of_word:
            # the cursor left the word
            self.hide_window()

    def is_active(self):
        return self.autocompletewindow is not None

    def complete(self):
        """Complete the word to the longest common prefix."""
        self._change_start(self._complete_string(self.start))

    def hide_window(self):
        """Close the list and remove its bindings."""
        if not self.is_active():
            return
        acw = self.autocompletewindow
        acw.event_delete(HIDE_VIRTUAL_EVENT_NAME, HIDE_FOCUS_OUT_SEQUENCE)
        for seq in HIDE_SEQUENCES:
            self.widget.event_delete(HIDE_VIRTUAL_EVENT_NAME, seq)
        acw.unbind(HIDE_VIRTUAL_EVENT_NAME, self.hideaid)
        self.widget.unbind(HIDE_VIRTUAL_EVENT_NAME, self.hidewid)
        self.hideaid = self.hidewid = None
        for seq in KEYPRESS_SEQUENCES:
            self.widget.event_delete(KEYPRESS_VIRTUAL_EVENT_NAME, seq)
        self.widget.unbind(KEYPRESS_VIRTUAL_EVENT_NAME, self.keypressid)
        self.keypressid = None
        self.widget.event_delete(KEYRELEASE_VIRTUAL_EVENT_NAME,
                                 KEYRELEASE_SEQUENCE)
        self.widget.unbind(KEYRELEASE_VIRTUAL_EVENT_NAME, self.keyreleaseid)
        self.keyreleaseid = None
        self.listbox.unbind(LISTUPDATE_SEQUENCE, self.listupdateid)
        self.listupdateid = None
        if self.winconfigid:
            acw.unbind(WINCONFIG_SEQUENCE, self.winconfigid)
            self.winconfigid = None
        self.widget.focus_set()
        self.scrollbar.destroy()
        self.scrollbar = None
        self.listbox.destroy()
        self.listbox = None
        acw.destroy()
        self.autocompletewindow = None


if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_autocomplete_w', verbosity=2, exit=False)
