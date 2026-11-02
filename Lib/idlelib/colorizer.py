"""Syntax coloring for editor and shell windows.

ColorDelegator is a Percolator filter: inserted and deleted text is marked
'TODO' and recolored a little later, in chunks, from a synchronization
point (a line end outside strings, tagged 'SYNC') onwards, so that typing
stays responsive in large files.
"""
import builtins
import keyword
import re
import time

from idlelib.config import idleConf
from idlelib.delegator import Delegator

DEBUG = False


def any(name, alternates):
    """Return a named regex group matching any of *alternates*."""
    return "(?P<%s>" % name + "|".join(alternates) + ")"


def make_pat():
    """Build the coloring regex.

    Groups: KEYWORD, BUILTIN, COMMENT, STRING, SYNC and the soft keyword
    groups (match, case, type, lazy and the '_' of case patterns), which
    only count in the statement positions where they are keywords.
    """
    kw = r"\b" + any("KEYWORD", keyword.kwlist) + r"\b"
    # 'match' and 'case' starting a line are keywords when what follows
    # cannot continue an expression using them as names: the end of the
    # line, or a blank and then a name (not a keyword), a bracket or a
    # string.
    hard_keywords = "|".join(keyword.kwlist)
    soft_follow = (r"(?=[ \t]*$|[ \t]+(?!(?:" + hard_keywords +
                   r")\b)[\w(\[{'\"])")
    match_softkw = (r"^[ \t]*" + any("MATCH_SOFTKW", [r"match"]) +
                    soft_follow)
    case_softkw = (r"^[ \t]*" + any("CASE_SOFTKW", [r"case"]) +
                   soft_follow)
    # The wildcard pattern of 'case _:' (or 'case _ if ...:').
    case_default = (r"(?<=\bcase)[ \t]+" +
                    any("CASE_DEFAULT_UNDERSCORE", [r"_"]) +
                    r"(?=[ \t]*(?::|if\b))")
    type_softkw = (r"^[ \t]*" + any("TYPE_SOFTKW", [r"type"]) +
                   r"(?=[ \t]+\w+[ \t]*[\[=])")
    lazy_softkw = (r"^[ \t]*" + any("LAZY_SOFTKW", [r"lazy"]) +
                   r"(?=[ \t]+(?:import|from)\b)")
    builtinlist = [str(name) for name in dir(builtins)
                   if not name.startswith('_') and name not in keyword.kwlist]
    # (A builtin name after a newline is found at the line start, so that
    # the newline stays a SYNC point.)
    builtin = r"([^.'\"\\#\n]\b|^)" + any("BUILTIN", builtinlist) + r"\b"
    comment = any("COMMENT", [r"#[^\n]*"])
    prefix = r"(?i:r|u|f|fr|rf|b|br|rb|t|tr|rt)?"
    sqstring = prefix + r"'[^'\\\n]*(\\.[^'\\\n]*)*'?"
    dqstring = prefix + r'"[^"\\\n]*(\\.[^"\\\n]*)*"?'
    sq3string = prefix + r"'''[^'\\]*((\\.|'(?!''))[^'\\]*)*(''')?"
    dq3string = prefix + r'"""[^"\\]*((\\.|"(?!""))[^"\\]*)*(""")?'
    string = any("STRING", [sq3string, dq3string, sqstring, dqstring])
    sync = any("SYNC", [r"\n"])
    return "|".join([kw, match_softkw, case_softkw, case_default,
                     type_softkw, lazy_softkw, builtin, comment, string, sync])


prog = re.compile(make_pat(), re.DOTALL | re.MULTILINE)
idprog = re.compile(r"\s+(\w+)")

# Characters of a line colorized at most.
MAX_COLORIZED_LINE = 1000
_long_line_re = re.compile(r"(?m)^([^\n]{%d})([^\n]+)" % MAX_COLORIZED_LINE)

# Regex groups colored with the KEYWORD tag.
_KEYWORD_GROUPS = {"KEYWORD", "MATCH_SOFTKW", "CASE_SOFTKW",
                   "CASE_DEFAULT_UNDERSCORE", "TYPE_SOFTKW", "LAZY_SOFTKW"}


def matched_named_groups(re_match):
    """Yield (group name, text) of the named groups that matched."""
    return ((name, text) for name, text in re_match.groupdict().items()
            if text)


def color_config(text):
    """Give a Text widget the colors of the current theme."""
    theme = idleConf.CurrentTheme()
    normal = idleConf.GetHighlight(theme, 'normal')
    cursor = idleConf.GetHighlight(theme, 'cursor')['foreground']
    select = idleConf.GetHighlight(theme, 'hilite')
    text.config(
        foreground=normal['foreground'],
        background=normal['background'],
        insertbackground=cursor,
        selectforeground=select['foreground'],
        selectbackground=select['background'],
        inactiveselectbackground=select['background'],
    )


class ColorDelegator(Delegator):
    """Percolator filter that colors Python syntax.

    State: after_id (pending recolorize call), allow_colorizing (coloring
    enabled), colorizing (recolorize running), stop_colorizing (abort the
    running recolorize at the next chunk).
    """

    def __init__(self):
        Delegator.__init__(self)
        self.init_state()
        self.prog = prog
        self.idprog = idprog
        self.LoadTagDefs()

    def init_state(self):
        self.after_id = None
        self.allow_colorizing = True
        self.stop_colorizing = False
        self.colorizing = False

    def setdelegate(self, delegate):
        """Attach to the next filter; start coloring the whole text."""
        if self.delegate is not None:
            self.unbind("<<toggle-auto-coloring>>")
        Delegator.setdelegate(self, delegate)
        if delegate is not None:
            self.config_colors()
            self.bind("<<toggle-auto-coloring>>", self.toggle_colorize_event)
            self.notify_range("1.0", "end")
        else:
            self.stop_colorizing = True
            self.allow_colorizing = False

    def config_colors(self):
        for tag, cnf in self.tagdefs.items():
            self.tag_configure(tag, **cnf)
        self.tag_raise('sel')

    def LoadTagDefs(self):
        """Read the tag colors of the current theme."""
        theme = idleConf.CurrentTheme()
        self.tagdefs = {
            "COMMENT": idleConf.GetHighlight(theme, "comment"),
            "KEYWORD": idleConf.GetHighlight(theme, "keyword"),
            "BUILTIN": idleConf.GetHighlight(theme, "builtin"),
            "STRING": idleConf.GetHighlight(theme, "string"),
            "DEFINITION": idleConf.GetHighlight(theme, "definition"),
            "SYNC": {'background': None, 'foreground': None},
            "TODO": {'background': None, 'foreground': None},
            "ERROR": idleConf.GetHighlight(theme, "error"),
            "hit": idleConf.GetHighlight(theme, "hit"),
        }
        if DEBUG:
            print('tagdefs', self.tagdefs)

    def insert(self, index, chars, tags=None):
        index = self.index(index)
        self.delegate.insert(index, chars, tags)
        self.notify_range(index, index + "+%dc" % len(chars))

    def delete(self, index1, index2=None):
        index1 = self.index(index1)
        self.delegate.delete(index1, index2)
        self.notify_range(index1)

    def notify_range(self, index1, index2=None):
        """Mark a range for recoloring and schedule it."""
        self.tag_add("TODO", index1, index2)
        if self.after_id:
            return  # already scheduled
        if self.colorizing:
            self.stop_colorizing = True  # restart with the new text
        if self.allow_colorizing:
            self.after_id = self.after(1, self.recolorize)

    def close(self):
        """Stop coloring."""
        if self.after_id:
            after_id = self.after_id
            self.after_id = None
            self.after_cancel(after_id)
        self.allow_colorizing = False
        self.stop_colorizing = True

    def toggle_colorize_event(self, event=None):
        """Switch coloring off (stopping a recolorize) or on."""
        if self.after_id:
            after_id = self.after_id
            self.after_id = None
            self.after_cancel(after_id)
        if self.allow_colorizing and self.colorizing:
            self.stop_colorizing = True
        self.allow_colorizing = not self.allow_colorizing
        if self.allow_colorizing and not self.colorizing:
            self.after_id = self.after(1, self.recolorize)
        if DEBUG:
            print("auto colorizing turned",
                  "on" if self.allow_colorizing else "off")
        return "break"

    def recolorize(self):
        """Recolor the TODO ranges (rescheduling if some are left)."""
        self.after_id = None
        if not self.delegate or not self.allow_colorizing or self.colorizing:
            return
        try:
            self.stop_colorizing = False
            self.colorizing = True
            t0 = time.perf_counter()
            self.recolorize_main()
            t1 = time.perf_counter()
            if DEBUG:
                print("%.3f seconds" % (t1 - t0))
        finally:
            self.colorizing = False
        if self.allow_colorizing and self.tag_nextrange("TODO", "1.0"):
            self.after_id = self.after(1, self.recolorize)

    def recolorize_main(self):
        """Recolor from the last SYNC point before each TODO range until a
        SYNC point is reached again, a growing number of lines at a time."""
        start = "1.0"
        while todo := self.tag_nextrange("TODO", start):
            self.tag_remove("SYNC", todo[0], todo[1])
            sync = self.tag_prevrange("SYNC", todo[0])
            head = sync[1] if sync else "1.0"  # where *chars* starts
            chars = ""
            pos = head
            nlines = 1
            done = False
            while not done:
                end = self.index(pos + "+%d lines linestart" % nlines)
                nlines = min(nlines * 2, 100)
                # The old coloring after a sync point is still valid if the
                # new coloring also syncs there.
                was_synced = "SYNC" in self.tag_names(end + "-1c")
                line = self.get(pos, end)
                if not line:
                    return
                for tag in self.tagdefs:
                    self.tag_remove(tag, pos, end)
                chars += line
                self._add_tags_in_section(chars, head)
                now_synced = "SYNC" in self.tag_names(end + "-1c")
                if now_synced:
                    head = end
                    chars = ""
                done = was_synced and now_synced
                if not done:
                    self.tag_add("TODO", end)
                self.update()
                if self.stop_colorizing:
                    if DEBUG:
                        print("colorizing stopped")
                    return
                pos = end
            start = pos

    def _add_tag(self, start, end, head, matched_group_name):
        """Tag chars[start:end] of the section starting at *head*."""
        tag = "KEYWORD" if matched_group_name in _KEYWORD_GROUPS else matched_group_name
        self.tag_add(tag, f"{head}+{start:d}c", f"{head}+{end:d}c")

    def _add_tags_in_section(self, chars, head):
        """Tag the syntax elements of *chars* (the text from *head*).

        Only the first MAX_COLORIZED_LINE characters of each line are
        colorized (very long lines would make IDLE slow); the rest is
        masked with characters no pattern starts with.
        """
        if len(chars) > MAX_COLORIZED_LINE:
            chars = _long_line_re.sub(
                lambda m: m.group(1) + '\0' * len(m.group(2)), chars)
        for m in self.prog.finditer(chars):
            for name, matched_text in matched_named_groups(m):
                a, b = m.span(name)
                self._add_tag(a, b, head, name)
                if matched_text in ("def", "class"):
                    if m1 := self.idprog.match(chars, b):
                        a, b = m1.span(1)
                        self._add_tag(a, b, head, "DEFINITION")

    def removecolors(self):
        """Remove all coloring tags."""
        for tag in self.tagdefs:
            self.tag_remove(tag, "1.0", "end")


def _color_delegator(parent):  # htest #
    from tkinter import Toplevel, Text
    from idlelib.idle_test.test_colorizer import source
    from idlelib.percolator import Percolator

    top = Toplevel(parent)
    top.title("Test ColorDelegator")
    x, y = map(int, parent.geometry().split('+')[1:])
    top.geometry("700x550+%d+%d" % (x + 20, y + 175))

    text = Text(top, background="white")
    text.pack(expand=1, fill="both")
    text.insert("insert", source)
    text.focus_set()

    color_config(text)
    p = Percolator(text)
    d = ColorDelegator()
    p.insertfilter(d)


if __name__ == "__main__":
    from unittest import main
    main('idlelib.idle_test.test_colorizer', verbosity=2, exit=False)
