"""Show the block headers enclosing the top visible line.

When code context is on, a small Text widget above the editor text lists
the lines opening the blocks (class, def, if, for, ...) that contain the
first visible line, so that the structure stays visible while scrolling.
Clicking a header scrolls the editor to it.
"""
import re

from tkinter import Frame, SUNKEN, Text, TclError

from idlelib.config import idleConf

INFINITY = 1 << 30  # an indent larger than any real one

BLOCKOPENERS = {'class', 'def', 'if', 'elif', 'else', 'while', 'for',
                'try', 'except', 'finally', 'with', 'async', 'match', 'case'}


def get_spaces_firstword(codeline, c=re.compile(r"^(\s*)(\w*)")):
    """Return (leading whitespace, first word) of *codeline*."""
    return c.match(codeline).groups()


def get_line_info(codeline):
    """Return (indent, codeline, opener) for a line of code.

    The indent of blank and comment lines is INFINITY (they never close a
    block); opener is the block keyword starting the line, or False.
    """
    spaces, firstword = get_spaces_firstword(codeline)
    indent = len(spaces)
    if len(codeline) == indent or codeline[indent] == '#':
        indent = INFINITY
    opener = firstword in BLOCKOPENERS and firstword
    return indent, codeline, opener


class CodeContext:
    """Editor extension maintaining the code context widget."""

    UPDATEINTERVAL = 100  # milliseconds between checks of the scrolling

    def __init__(self, editwin):
        self.editwin = editwin
        self.text = editwin.text
        self._reset()

    def _reset(self):
        self.context = None
        self.cell00 = None
        self.t1 = None
        # The headers of the blocks containing the top line, outermost
        # first, as (lineno, indent, line, opener); the first entry is a
        # sentinel for the module level.
        self.info = [(0, -1, '', False)]
        self.topvisible = 1

    @classmethod
    def reload(cls):
        """Read the maximal number of context lines."""
        cls.context_depth = idleConf.GetOption(
            "extensions", "CodeContext", "maxlines", type="int", default=15)

    def __del__(self):
        """Cancel the update timer."""
        if self.t1 is not None:
            try:
                self.text.after_cancel(self.t1)
            except TclError:
                pass
            self.t1 = None

    def toggle_code_context_event(self, event=None):
        """Show or hide the context widget."""
        if self.context is None:
            frame = self.editwin.text_frame
            text = self.editwin.text
            # Align the context lines with the text lines.
            padx = int(text.cget('padx')) + int(text.cget('borderwidth'))
            self.context = context = Text(
                frame, height=1, width=1, highlightthickness=0,
                padx=padx, border=0, relief=SUNKEN, state='disabled')
            self.update_font()
            self.update_highlight_colors()
            context.bind('<ButtonRelease-1>', self.jumptoline)
            self.update_code_context()
            context.grid(row=0, column=1, sticky='nsew')

            # The cell left of the context, above the line numbers.
            colors = idleConf.GetHighlight(idleConf.CurrentTheme(), 'linenumber')
            self.cell00 = Frame(frame, bg=colors['background'])
            self.cell00.grid(row=0, column=0, sticky='nsew')
            menu_status = 'Hide'
            self.t1 = self.text.after(self.UPDATEINTERVAL, self.timer_event)
        else:
            self.context.destroy()
            self.cell00.destroy()
            self.text.after_cancel(self.t1)
            self._reset()
            menu_status = 'Show'
        self.editwin.update_menu_label(menu='options', index='*ode*ontext',
                                       label=f'{menu_status} Code Context')
        return "break"

    def get_context(self, new_topvisible, stopline=1, stopindent=0):
        """Return the block headers above line *new_topvisible*.

        Lines from new_topvisible-1 up to *stopline* are searched; the
        search ends early at a line indented at most *stopindent*.
        Return (headers in text order, smallest indent seen).
        """
        assert stopline > 0
        headers = []
        lastindent = INFINITY
        for linenum in range(new_topvisible, stopline - 1, -1):
            codeline = self.text.get(f'{linenum}.0', f'{linenum}.end')
            indent, line, opener = get_line_info(codeline)
            if indent < lastindent:
                lastindent = indent
                if opener in ('else', 'elif'):
                    # The 'if' (or 'try', ...) at the same indent belongs
                    # to the context too.
                    lastindent += 1
                if opener and linenum < new_topvisible and indent >= stopindent:
                    headers.append((linenum, indent, line, opener))
                if lastindent <= stopindent:
                    break
        headers.reverse()
        return headers, lastindent

    def update_code_context(self):
        """Update the context if the top visible line changed."""
        new_topvisible = self.editwin.getlineno("@0,0")
        if self.topvisible == new_topvisible:
            return
        if self.topvisible < new_topvisible:
            # Scrolled down: only lines between the old and the new top
            # can add headers; headers not enclosing the new top go.
            headers, lastindent = self.get_context(new_topvisible,
                                                   self.topvisible)
            while self.info[-1][1] >= lastindent:
                del self.info[-1]
        else:
            # Scrolled up: drop the headers below the new top and search
            # again above it, down to the remaining innermost header.
            stopindent = self.info[-1][1] + 1
            while self.info[-1][0] >= new_topvisible:
                stopindent = self.info[-1][1]
                del self.info[-1]
            headers, lastindent = self.get_context(
                new_topvisible, self.info[-1][0] + 1, stopindent)
        self.info.extend(headers)
        self.topvisible = new_topvisible

        shown = [header[2] for header in self.info[-self.context_depth:]]
        skip = 0 if shown[0] else 1  # the module level sentinel
        self.context['height'] = len(shown) - skip
        self.context['state'] = 'normal'
        self.context.delete('1.0', 'end')
        self.context.insert('end', '\n'.join(shown[skip:]))
        self.context['state'] = 'disabled'

    def jumptoline(self, event=None):
        """Scroll the editor to the header clicked (unless text of the
        context widget is being selected)."""
        try:
            self.context.index("sel.first")
        except TclError:
            pass
        else:
            return  # selecting, not jumping
        count = len(self.info)
        if count == 1:
            newtop = 1
        else:
            clicked = int(float(self.context.index('insert')))
            # Index in self.info of the first line shown.
            first = max(1, count - self.context_depth)
            newtop = self.info[first + clicked - 1][0]
        self.text.yview(f'{newtop}.0')
        self.update_code_context()

    def timer_event(self):
        """Update the context periodically while it is shown."""
        if self.context:
            self.update_code_context()
            self.t1 = self.text.after(self.UPDATEINTERVAL, self.timer_event)

    def update_font(self):
        if self.context is not None:
            font = idleConf.GetFont(self.text, 'main', 'EditorWindow')
            self.context['font'] = font

    def update_highlight_colors(self):
        if self.context is not None:
            colors = idleConf.GetHighlight(idleConf.CurrentTheme(), 'context')
            self.context['background'] = colors['background']
            self.context['foreground'] = colors['foreground']

        if self.cell00 is not None:
            colors = idleConf.GetHighlight(idleConf.CurrentTheme(), 'linenumber')
            self.cell00.config(bg=colors['background'])


CodeContext.reload()


if __name__ == "__main__":
    from unittest import main
    main('idlelib.idle_test.test_codecontext', verbosity=2, exit=False)
