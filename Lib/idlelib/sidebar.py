"""Sidebars left of the text: line numbers (editor) and prompts (shell).

Both follow the text's vertical scrolling, use the editor font and the
'linenumber' colors of the theme, and pass mouse wheel events on to the
text.
"""
import itertools
import tkinter as tk
from tkinter.font import Font

from idlelib.config import idleConf
from idlelib.delegator import Delegator
from idlelib import util


def get_lineno(text, index):
    """Return the line number of *index* of a Text widget."""
    textindex = text.index(index)
    return int(float(textindex)) if textindex else None


def get_end_linenumber(text):
    """Return the number of the last line of a Text widget."""
    return get_lineno(text, 'end-1c')


def get_displaylines(text, index):
    """Return the number of display lines (with wrapping) of the line
    containing *index*."""
    res = text.count(f"{index} linestart", f"{index} lineend", "displaylines")
    return res[0] if res else 0


def get_widget_padding(widget):
    """Return (padx, pady) of a widget, including its border."""
    manager = widget.winfo_manager()
    if manager == 'pack':
        info = widget.pack_info()
    elif manager == 'grid':
        info = widget.grid_info()
    else:
        info = {}
    padx = sum(map(widget.tk.getint, (widget.cget('padx'), widget.cget('border'))))
    pady = sum(map(widget.tk.getint, (widget.cget('pady'), widget.cget('border'))))
    for name in ('padx', 'pady'):
        value = info.get(name, 0)
        if isinstance(value, (tuple, list)):
            value = value[0]
        if name == 'padx':
            padx += int(str(value))
        else:
            pady += int(str(value))
    return padx, pady


class BaseSideBar:
    """A widget shown left of the text, in column 0 of the text frame."""

    def __init__(self, editwin):
        self.editwin = editwin
        self.parent = editwin.text_frame
        self.text = editwin.text
        self.is_shown = False
        self.main_widget = self.init_widgets()
        self.bind_events()
        self.update_font()
        self.update_colors()

    def init_widgets(self):
        """Create the widgets; return the one to grid."""
        raise NotImplementedError

    def update_font(self):
        raise NotImplementedError

    def update_colors(self):
        raise NotImplementedError

    def grid(self):
        self.main_widget.grid(row=1, column=0, sticky=tk.NSEW)

    def show_sidebar(self):
        if not self.is_shown:
            self.grid()
            self.is_shown = True

    def hide_sidebar(self):
        if self.is_shown:
            self.main_widget.grid_forget()
            self.is_shown = False

    def yscroll_event(self, *args, **kwargs):
        """Called with the text's yscrollcommand arguments."""
        raise NotImplementedError

    def redirect_yscroll_event(self, *args, **kwargs):
        """The text's yscrollcommand: update the scrollbar and the
        sidebar."""
        self.editwin.vbar.set(*args)
        return self.yscroll_event(*args, **kwargs)

    def redirect_focusin_event(self, event):
        """The focus belongs to the text."""
        self.text.focus_set()
        return 'break'

    def redirect_mousebutton_event(self, event, event_name):
        """Pass a mouse button event on to the text."""
        self.text.focus_set()
        self.text.event_generate(event_name, x=0, y=event.y)
        return 'break'

    def redirect_mousewheel_event(self, event):
        """Scroll the text with the wheel."""
        return util.wheel_event(event, self.text)

    def bind_events(self):
        self.text['yscrollcommand'] = self.redirect_yscroll_event
        widget = self.main_widget
        widget.bind('<FocusIn>', self.redirect_focusin_event)
        for sequence in ('<MouseWheel>', '<Button-4>', '<Button-5>'):
            widget.bind(sequence, self.redirect_mousewheel_event)
        # Clicks and drags select whole lines of the text.
        self._drag_start_line = None

        def press(event):
            line = self._line_at(event.y)
            self._drag_start_line = line
            self._select_lines(line, line)
            self.text.focus_set()
            return 'break'

        def motion(event):
            if self._drag_start_line is None:
                return 'break'
            self._select_lines(self._drag_start_line, self._line_at(event.y))
            return 'break'

        def release(event):
            self._drag_start_line = None
            return 'break'

        def double(event):
            return 'break'

        widget.bind('<Button-1>', press)
        widget.bind('<B1-Motion>', motion)
        widget.bind('<ButtonRelease-1>', release)
        widget.bind('<Double-Button-1>', double)

    def _line_at(self, y):
        return get_lineno(self.text, f"@0,{y}")

    def _select_lines(self, a, b):
        first, last = min(a, b), max(a, b)
        text = self.text
        text.tag_remove("sel", "1.0", "end")
        text.tag_add("sel", f"{first}.0", f"{last + 1}.0")
        text.mark_set("insert", f"{last + 1}.0")


class EndLineDelegator(Delegator):
    """A Percolator filter calling a function with the number of the
    last line after every change of the text."""

    def __init__(self, changed_callback):
        Delegator.__init__(self)
        self.changed_callback = changed_callback

    def insert(self, index, chars, tags=None):
        self.delegate.insert(index, chars, tags)
        self.changed_callback(get_end_linenumber(self.delegate))

    def delete(self, index1, index2=None):
        self.delegate.delete(index1, index2)
        self.changed_callback(get_end_linenumber(self.delegate))


class LineNumbers(BaseSideBar):
    """The line numbers of an editor window, in a disabled Text widget
    as narrow as the largest number allows."""

    def __init__(self, editwin):
        super().__init__(editwin)
        delegator = EndLineDelegator(self.update_sidebar_text)
        # Below the undo delegator: line numbers do not change undo.
        editwin.per.insertfilterafter(delegator, after=editwin.undo)

    def init_widgets(self):
        _padx, pady = get_widget_padding(self.text)
        self.sidebar_text = tk.Text(self.parent, width=1, wrap=tk.NONE,
                                    padx=2, pady=pady, borderwidth=0,
                                    highlightthickness=0)
        self.sidebar_text.config(state=tk.DISABLED)
        self.prev_end = 1
        self._sidebar_width_type = type(self.sidebar_text['width'])
        self.sidebar_text.config(state=tk.NORMAL)
        self.sidebar_text.insert('insert', '1', 'linenumber')
        self.sidebar_text.config(state=tk.DISABLED)
        self.sidebar_text.tag_config('linenumber', justify=tk.RIGHT)
        end = get_end_linenumber(self.text)
        self.update_sidebar_text(end)
        return self.sidebar_text

    def grid(self):
        super().grid()
        # Show the lines currently visible in the text.
        self.sidebar_text.yview_moveto(self.text.yview()[0])

    def update_font(self):
        font = idleConf.GetFont(self.text, 'main', 'EditorWindow')
        self.sidebar_text['font'] = font

    def update_colors(self):
        colors = idleConf.GetHighlight(idleConf.CurrentTheme(), 'linenumber')
        foreground = colors['foreground']
        background = colors['background']
        self.sidebar_text.config(
            fg=foreground, bg=background,
            selectforeground=foreground, selectbackground=background,
            inactiveselectbackground=background,
        )

    def update_sidebar_text(self, end):
        """Show the numbers 1 to *end*."""
        if end == self.prev_end:
            return
        width_change = len(str(end)) - len(str(self.prev_end))
        if width_change:
            width = int(float(self.sidebar_text['width']))
            self.sidebar_text['width'] = self._sidebar_width_type(
                width + width_change)
        self.sidebar_text.config(state=tk.NORMAL)
        if end > self.prev_end:
            new_text = '\n'.join(itertools.chain(
                [''], map(str, range(self.prev_end + 1, end + 1))))
            self.sidebar_text.insert('end -1c', new_text, 'linenumber')
        else:
            self.sidebar_text.delete(f'{end + 1}.0 -1c', 'end -1c')
        self.sidebar_text.config(state=tk.DISABLED)
        self.prev_end = end

    def yscroll_event(self, *args, **kwargs):
        self.sidebar_text.yview_moveto(args[0])
        return 'break'


class WrappedLineHeightChangeDelegator(Delegator):
    """A Percolator filter calling a function after changes that may
    change the number of display lines of a wrapped line."""

    def __init__(self, callback):
        super().__init__()
        self.callback = callback

    def insert(self, index, chars, tags=None):
        self.delegate.insert(index, chars, tags)
        self.callback()

    def delete(self, index1, index2=None):
        self.delegate.delete(index1, index2)
        self.callback()


class ShellSidebar(BaseSideBar):
    """The prompts of the shell ('>>>' and '...') on a canvas.

    A line gets '>>>' if the newline before it has the 'console' tag
    (the end of the output before a prompt), '...' if that newline was
    typed ('stdin' tag).
    """

    def __init__(self, editwin):
        self.canvas = None
        self.line_prompts = {}
        self.font = None
        self.colors = ('black', 'white')
        super().__init__(editwin)
        change_delegator = WrappedLineHeightChangeDelegator(self.change_callback)
        # Above the undo delegator, so that undo and redo update too.
        editwin.per.insertfilter(change_delegator)
        self.text.bind('<<squeezed-expanded>>', lambda e: self.change_callback(),
                       add=True)
        self.is_shown = False
        self.show_sidebar()

    def init_widgets(self):
        self.canvas = tk.Canvas(self.parent, width=30,
                                borderwidth=0, highlightthickness=0,
                                takefocus=False)
        return self.canvas

    def bind_events(self):
        super().bind_events()
        self.main_widget.bind(
            '<Button-3>' if self.main_widget._windowingsystem != 'aqua'
            else '<Button-2>', self.context_menu_event)

    def context_menu_event(self, event):
        rmenu = tk.Menu(self.main_widget, tearoff=0)
        has_selection = bool(self.text.tag_nextrange('sel', '1.0'))

        def mkcmd(eventname):
            return lambda: self.text.event_generate(eventname)
        rmenu.add_command(label='Copy', command=mkcmd('<<copy>>'),
                          state='normal' if has_selection else 'disabled')
        rmenu.add_command(label='Copy with prompts',
                          command=mkcmd('<<copy-with-prompts>>'),
                          state='normal' if has_selection else 'disabled')
        rmenu.tk_popup(event.x_root, event.y_root)
        return "break"

    def change_callback(self):
        if self.is_shown:
            self.update_sidebar()
        else:
            self._compute_line_prompts()

    def _compute_line_prompts(self):
        """Compute the prompt of every line (line number -> prompt)."""
        text = self.text
        prompts = {}
        ranges = text.tag_ranges('stdin')
        for start, end in zip(ranges[0::2], ranges[1::2]):
            chunk = text.get(start, end)
            base = text.index(start)
            pos = chunk.find('\n')
            while pos >= 0:
                lineno = get_lineno(text, f"{base}+{pos}c")
                prompts[lineno + 1] = '...'
                pos = chunk.find('\n', pos + 1)
        ranges = text.tag_ranges('console')
        for start, end in zip(ranges[0::2], ranges[1::2]):
            chunk = text.get(start, end)
            base = text.index(start)
            pos = chunk.find('\n')
            while pos >= 0:
                lineno = get_lineno(text, f"{base}+{pos}c")
                prompts[lineno + 1] = '>>>'
                pos = chunk.find('\n', pos + 1)
        last = get_end_linenumber(text)
        self.line_prompts = {line: prompt for line, prompt in prompts.items()
                             if line <= last}
        return self.line_prompts

    def update_sidebar(self):
        """Redraw the prompts of the visible lines."""
        text = self.text
        canvas = self.canvas
        line_prompts = self._compute_line_prompts()
        canvas.delete(tk.ALL)

        index = text.index("@0,0")
        if index.split('.', 1)[1] != '0':
            index = text.index(f'{index}+1line linestart')
        while True:
            lineinfo = text.dlineinfo(index)
            if lineinfo is None:
                break
            y = lineinfo[1]
            prompt = line_prompts.get(get_lineno(text, index))
            if prompt:
                canvas.create_text(2, y, anchor=tk.NW, text=prompt,
                                   font=self.font, fill=self.colors[0])
            index = text.index(f'{index}+1line')
            if text.compare(index, '>=', 'end'):
                break

    def yscroll_event(self, *args, **kwargs):
        """Redraw when the text scrolls."""
        self.change_callback()
        return 'break'

    def update_font(self):
        """Use the editor font; the canvas is wide enough for '>>>'."""
        font = idleConf.GetFont(self.text, 'main', 'EditorWindow')
        tk_font = Font(self.text, font=font)
        char_width = max(tk_font.measure(char) for char in ['>', '.'])
        self.canvas.configure(width=char_width * 3 + 4)
        self.font = font
        self.change_callback()

    def update_colors(self):
        """Use the line number colors (and the console background)."""
        linenumbers_colors = idleConf.GetHighlight(idleConf.CurrentTheme(),
                                                   'linenumber')
        prompt_colors = idleConf.GetHighlight(idleConf.CurrentTheme(),
                                              'console')
        foreground = prompt_colors['foreground']
        background = linenumbers_colors['background']
        self.colors = (foreground, background)
        self.canvas.configure(background=background)
        self.change_callback()


def _sidebar_number_scrolling(parent):  # htest #
    from idlelib.idle_test.test_sidebar import Dummy_editwin

    top = tk.Toplevel(parent)
    text_frame = tk.Frame(top)
    text_frame.grid_rowconfigure(0, weight=1)
    text_frame.grid_columnconfigure(1, weight=1)

    font = idleConf.GetFont(top, 'main', 'EditorWindow')
    text = tk.Text(text_frame, width=80, height=24, wrap=tk.NONE, font=font)
    text.grid(row=1, column=1, sticky=tk.NSEW)

    editwin = Dummy_editwin(text)
    editwin.vbar = tk.Scrollbar(text_frame)

    linenumbers = LineNumbers(editwin)
    linenumbers.show_sidebar()

    text.insert('1.0', '\n'.join('a'*i for i in range(1, 101)))


if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_sidebar', verbosity=2, exit=False)
