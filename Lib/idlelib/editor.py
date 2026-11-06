"""The editor window: a Text widget with menus, key bindings, a status
bar and the editing features of IDLE.

The shell (pyshell.PyShell) and the output windows (outwin) are editor
windows too.
"""
import importlib
import importlib.abc
import importlib.util
import os
import platform
import re
import string
import sys
import tokenize
import traceback
import webbrowser

from tkinter import *
from tkinter.font import Font
from tkinter.ttk import Scrollbar
from tkinter import simpledialog
from tkinter import messagebox

from idlelib.config import idleConf
from idlelib import grep
from idlelib import help_about
from idlelib import macosx
from idlelib.multicall import MultiCallCreator
from idlelib import pyparse
from idlelib import query
from idlelib import replace
from idlelib import search
from idlelib.tree import wheel_event
from idlelib.util import py_extensions
from idlelib import window

# The default tab width of Tk (8 columns, like Python itself).
TK_TABWIDTH_DEFAULT = 8
_py_version = ' (%s)' % platform.python_version()
darwin = sys.platform == 'darwin'


def _sphinx_version():
    """Return the version string used in documentation URLs."""
    major, minor, micro, level, serial = sys.version_info
    release = f'{major}{minor}'
    release += f'{micro}'
    if level == 'candidate':
        release += f'rc{serial}'
    elif level != 'final':
        release += f'{level[0]}{serial}'
    return release


def get_line_indent(line, tabwidth):
    """Return (characters, columns) of the leading blanks of *line*."""
    indent = re.match(r'[ \t]*', line).group()
    return len(indent), len(indent.expandtabs(tabwidth))


class EditorWindow:
    """A window editing one file (or an untitled text)."""

    from idlelib.percolator import Percolator
    from idlelib.colorizer import ColorDelegator, color_config
    from idlelib.undo import UndoDelegator
    from idlelib.iomenu import IOBinding
    from idlelib import mainmenu
    from idlelib.statusbar import MultiStatusBar
    from idlelib.autocomplete import AutoComplete
    from idlelib.autoexpand import AutoExpand
    from idlelib.calltip import Calltip
    from idlelib.codecontext import CodeContext
    from idlelib.sidebar import LineNumbers
    from idlelib.format import FormatParagraph, FormatRegion, Indents, Rstrip
    from idlelib.parenmatch import ParenMatch
    from idlelib.zoomheight import ZoomHeight

    filesystemencoding = sys.getfilesystemencoding()
    help_url = None

    allow_code_context = True
    allow_line_numbers = True
    # Tags given to typed text (the shell uses 'stdin').
    user_input_insert_tags = None
    rmenu = None

    menu_specs = [
        ("file", "_File"),
        ("edit", "_Edit"),
        ("format", "F_ormat"),
        ("run", "_Run"),
        ("options", "_Options"),
        ("window", "_Window"),
        ("help", "_Help"),
    ]

    rmenu_specs = [
        # (label, virtual event, method returning the item state)
        ("Cut", "<<cut>>", "rmenu_check_cut"),
        ("Copy", "<<copy>>", "rmenu_check_copy"),
        ("Paste", "<<paste>>", "rmenu_check_paste"),
    ]

    def __init__(self, flist=None, filename=None, key=None, root=None):
        if EditorWindow.help_url is None:
            EditorWindow.help_url = self._find_help_url()
        self.flist = flist
        root = root or flist.root
        self.root = root
        self.menubar = Menu(root)
        self.top = top = window.ListedToplevel(root, menu=self.menubar)
        if flist:
            self.tkinter_vars = flist.vars
            # Windows sharing this flist (for menus such as recent files).
            self.top.instance_dict = flist.inversedict
        else:
            self.tkinter_vars = {}
            self.top.instance_dict = {}
        self.recent_files_path = idleConf.userdir and os.path.join(
            idleConf.userdir, 'recent-files.lst')

        self.prompt_last_line = ''  # the shell's prompt
        self.text_frame = text_frame = Frame(top)
        self.vbar = vbar = Scrollbar(text_frame, name='vbar')
        width = idleConf.GetOption('main', 'EditorWindow', 'width', type='int')
        text_options = {
            'name': 'text',
            'padx': 5,
            'wrap': 'none',
            'highlightthickness': 0,
            'width': width,
            'tabstyle': 'wordprocessor',
            'height': idleConf.GetOption(
                'main', 'EditorWindow', 'height', type='int'),
        }
        self.text = text = MultiCallCreator(Text)(text_frame, **text_options)
        self.top.focused_widget = self.text

        self.createmenubar()
        self.apply_bindings()

        self.top.protocol("WM_DELETE_WINDOW", self.close)
        self.top.bind("<<close-window>>", self.close_event)
        if macosx.isAquaTk():
            # The close event of the window menu on macOS.
            text.bind('<<close-window>>', self.close_event)
            # Command-W closes, Control-click posts the context menu.
            text.bind("<Control-Button-1>", self.right_menu_event)
            text.bind("<2>", self.right_menu_event)
        else:
            text.bind("<3>", self.right_menu_event)

        for wheel in ('<MouseWheel>', '<Button-4>', '<Button-5>'):
            text.bind(wheel, wheel_event)
        text.bind('<FocusIn>', self.focus_in_event, add='+')

        bindings = {
            "<<cut>>": self.cut,
            "<<copy>>": self.copy,
            "<<paste>>": self.paste,
            "<<center-insert>>": self.center_insert_event,
            "<<help>>": self.help_dialog,
            "<<python-docs>>": self.python_docs,
            "<<about-idle>>": self.about_dialog,
            "<<open-config-dialog>>": self.config_dialog,
            "<<open-module>>": self.open_module_event,
            "<<do-nothing>>": lambda event: "break",
            "<<select-all>>": self.select_all,
            "<<remove-selection>>": self.remove_selection,
            "<<find>>": self.find_event,
            "<<find-again>>": self.find_again_event,
            "<<find-in-files>>": self.find_in_files_event,
            "<<find-selection>>": self.find_selection_event,
            "<<replace>>": self.replace_event,
            "<<goto-line>>": self.goto_line_event,
            "<<smart-backspace>>": self.smart_backspace_event,
            "<<newline-and-indent>>": self.newline_and_indent_event,
            "<<smart-indent>>": self.smart_indent_event,
            "<<del-word-left>>": self.del_word_left,
            "<<del-word-right>>": self.del_word_right,
            "<<beginning-of-line>>": self.home_callback,
        }
        for event, handler in bindings.items():
            text.bind(event, handler)
        text.bind("<Left>", self.move_at_edge_if_selection(0))
        text.bind("<Right>", self.move_at_edge_if_selection(1))

        # The region commands of the Format menu.
        self.fregion = fregion = self.FormatRegion(self)
        for event, handler in (
                ("<<indent-region>>", fregion.indent_region_event),
                ("<<dedent-region>>", fregion.dedent_region_event),
                ("<<comment-region>>", fregion.comment_region_event),
                ("<<uncomment-region>>", fregion.uncomment_region_event),
                ("<<tabify-region>>", fregion.tabify_region_event),
                ("<<untabify-region>>", fregion.untabify_region_event)):
            text.bind(event, handler)
        indents = self.Indents(self)
        text.bind("<<toggle-tabs>>", indents.toggle_tabs_event)
        text.bind("<<change-indentwidth>>", indents.change_indentwidth_event)

        if flist:
            flist.inversedict[self] = key
            if key:
                flist.dict[key] = self
            text.bind("<<open-new-window>>", self.new_callback)
            text.bind("<<close-all-windows>>", self.flist.close_all_callback)
            text.bind("<<open-class-browser>>", self.open_module_browser)
            text.bind("<<open-path-browser>>", self.open_path_browser)
            text.bind("<<open-turtle-demo>>", self.open_turtle_demo)

        self.set_status_bar()
        text_frame.pack(side=LEFT, fill=BOTH, expand=1)
        text_frame.rowconfigure(1, weight=1)
        text_frame.columnconfigure(1, weight=1)
        vbar['command'] = self.handle_yview
        vbar.grid(row=1, column=2, sticky=NSEW)
        text['yscrollcommand'] = vbar.set
        text['font'] = idleConf.GetFont(self.root, 'main', 'EditorWindow')
        text.grid(row=1, column=1, sticky=NSEW)
        text.focus_set()
        self.set_width()

        # Indentation: usetabs selects tabs or spaces; indentwidth is the
        # width of an indent level; tabwidth is the width of a tab
        # (always 8 except in odd files).
        usespaces = idleConf.GetOption('main', 'Indent', 'use-spaces',
                                       type='bool')
        self.usetabs = not usespaces
        self.tabwidth = TK_TABWIDTH_DEFAULT
        self.indentwidth = idleConf.GetOption('main', 'Indent', 'num-spaces',
                                              type='int')
        # Whether code before the cursor starts at a prompt (shell).
        self.context_use_ps1 = False
        # Lines searched backwards for a good place to start parsing.
        self.num_context_lines = 50, 500, 5000000

        self.per = per = self.Percolator(text)
        self.undo = undo = self.UndoDelegator()
        per.insertfilter(undo)
        text.undo_block_start = undo.undo_block_start
        text.undo_block_stop = undo.undo_block_stop
        undo.set_saved_change_hook(self.saved_change_hook)
        # The io binding sets the filename and loads the file.
        self.io = io = self.IOBinding(self)
        io.set_filename_change_hook(self.filename_change_hook)
        self.good_load = False
        self.set_indentation_params(False)
        self.color = None  # the ColorDelegator, if colorizing
        self.code_context = None
        self.line_numbers = None
        self.mtime = None
        if filename:
            if os.path.exists(filename) and not os.path.isdir(filename):
                if io.loadfile(filename):
                    self.good_load = True
                    is_py_src = self.ispythonsource(filename)
                    self.set_indentation_params(is_py_src)
            else:
                io.set_filename(filename)
                self.good_load = True

        self.ResetColorizer()
        self.saved_change_hook()
        self.update_recent_files_list()
        self.load_extensions()
        menu = self.menudict.get('window')
        if menu:
            end = menu.index("end")
            if end is None:
                end = -1
            if end >= 0:
                menu.add_separator()
                end = end + 1
            self.wmenu_end = end
            window.register_callback(self.postwindowsmenu)

        # The former extensions, now always present.
        self.askinteger = simpledialog.askinteger
        self.showerror = messagebox.showerror

        self.format_paragraph = self.FormatParagraph(self)
        text.bind("<<format-paragraph>>",
                  self.format_paragraph.format_paragraph_event)
        parenmatch = self.ParenMatch(self)
        text.bind("<<flash-paren>>", parenmatch.flash_paren_event)
        text.bind("<<paren-closed>>", parenmatch.paren_closed_event)
        scriptbinding = self._scriptbinding()
        if scriptbinding is not None:
            text.bind("<<check-module>>", scriptbinding.check_module_event)
            text.bind("<<run-module>>", scriptbinding.run_module_event)
            text.bind("<<run-custom>>", scriptbinding.run_custom_event)
        text.bind("<<do-rstrip>>", self.Rstrip(self).do_rstrip)
        self.ctip = ctip = self.Calltip(self)
        text.bind("<<try-open-calltip>>", ctip.try_open_calltip_event)
        text.bind("<<refresh-calltip>>", ctip.refresh_calltip_event)
        text.bind("<<force-open-calltip>>", ctip.force_open_calltip_event)
        text.bind("<<zoom-height>>", self.ZoomHeight(self).zoom_height_event)
        if self.allow_code_context:
            self.code_context = self.CodeContext(self)
            text.bind("<<toggle-code-context>>",
                      self.code_context.toggle_code_context_event)
        else:
            self.update_menu_state('options', '*ode*ontext', 'disabled')
        if self.allow_line_numbers:
            self.line_numbers = self.LineNumbers(self)
            if idleConf.GetOption('main', 'EditorWindow',
                                  'line-numbers-default', type='bool'):
                self.toggle_line_numbers_event()
            text.bind("<<toggle-line-numbers>>", self.toggle_line_numbers_event)
        else:
            self.update_menu_state('options', '*ine*umbers', 'disabled')
        autocomplete = self.AutoComplete(self, self.user_input_insert_tags)
        text.bind("<<autocomplete>>", autocomplete.autocomplete_event)
        text.bind("<<try-open-completions>>",
                  autocomplete.try_open_completions_event)
        text.bind("<<force-open-completions>>",
                  autocomplete.force_open_completions_event)
        text.bind("<<expand-word>>", self.AutoExpand(self).expand_word_event)
        self.mtime = self.last_mtime()

    def _scriptbinding(self):
        # Run and Check Module (not available without the run module).
        try:
            from idlelib.runscript import ScriptBinding
        except ImportError:
            return None
        return ScriptBinding(self)

    @staticmethod
    def _find_help_url():
        """Return the local documentation if installed, else the web."""
        url = "https://docs.python.org/%d.%d/" % sys.version_info[:2]
        if sys.platform.count('linux'):
            local = '/usr/share/doc/python%s.%s/html/index.html' % sys.version_info[:2]
            if os.path.isfile(local):
                url = local
        elif sys.platform[:3] == 'win':
            chm = os.path.join(sys.base_prefix, 'Doc',
                               'Python%s.chm' % _sphinx_version())
            if os.path.isfile(chm):
                url = chm
        elif darwin:
            local = os.path.join(sys.base_prefix, 'Resources/English.lproj/'
                                 'Documentation/index.html')
            if os.path.isfile(local):
                url = local
        return url

    # --- the file on disk

    def last_mtime(self):
        """Modification time of the file, or None (no file, or deleted)."""
        filename = self.io.filename
        if not filename:
            return None
        try:
            return os.path.getmtime(filename)
        except OSError:
            return None

    def focus_in_event(self, event):
        """Check whether the file changed on disk while the window was
        not focused."""
        if self.mtime is None:
            return None
        current = self.last_mtime()
        if current is None:
            return self.deleted_file_event(event)
        if current != self.mtime:
            self.mtime = current
            if self.askyesno(
                    'File changed',
                    f'{self.io.filename}\nwas changed by another program.  '
                    'Reload it (losing the changes made here)?'):
                self.io.loadfile(self.io.filename)
                self.mtime = self.last_mtime()
        return None

    def deleted_file_event(self, event):
        """Ask what to do about a file deleted by another program."""
        dialog = simpledialog.SimpleDialog(
            self.text,
            text=f'{self.io.filename}\nhas been deleted or moved by '
                 'another program.',
            buttons=['Close', 'Save As...', 'Ignore'],
            default=1, cancel=2, title='File deleted')
        choice = dialog.go()
        # Clear first: the dialogs below give the focus back and would
        # ask again.
        self.mtime = None
        if choice == 0:
            self.close()
        elif choice == 1:
            self.io.save_as(event)
        return "break"

    def askyesno(self, title, message):
        return messagebox.askyesno(title, message, parent=self.text)

    # --- callbacks of the text

    def handle_yview(self, event, *args):
        """Scrollbar command; dragging scrolls by whole lines."""
        if event == 'moveto':
            fraction = float(args[0])
            lines = (round(self.getlineno('end') * fraction) -
                     self.getlineno('@0,0'))
            event = 'scroll'
            args = (lines, 'units')
        self.text.yview(event, *args)
        return 'break'

    def right_menu_event(self, event):
        """Post the context menu.  A click outside the selection moves
        the cursor there (and removes the selection), like a left click."""
        text = self.text
        newdex = text.index(f'@{event.x},{event.y}')
        try:
            in_selection = (text.compare('sel.first', '<=', newdex) and
                            text.compare(newdex, '<=', 'sel.last'))
        except TclError:
            in_selection = False
        if not in_selection:
            text.tag_remove("sel", "1.0", "end")
            text.mark_set("insert", newdex)
        if not self.rmenu:
            self.make_rmenu()
        rmenu = self.rmenu
        self.event = event
        iswin = sys.platform[:3] == 'win'
        if iswin:
            text.config(cursor="arrow")
        rmenu.tk_popup(event.x_root, event.y_root)
        if iswin:
            text.config(cursor="ibeam")
        return "break"

    def make_rmenu(self):
        rmenu = Menu(self.text, tearoff=0, postcommand=self._update_rmenu)
        for item in self.rmenu_specs:
            label, eventname = item[0], item[1]
            if label is None:
                rmenu.add_separator()
                continue

            def command(text=self.text, eventname=eventname):
                text.event_generate(eventname)
            rmenu.add_command(label=label, command=command)
        self.rmenu = rmenu

    def _update_rmenu(self):
        # Enable or disable the items before the menu is shown.
        for item in self.rmenu_specs:
            label, eventname, verify_state = (tuple(item) + (None,))[:3]
            if label is None or verify_state is None:
                continue
            state = getattr(self, verify_state)()
            self.rmenu.entryconfigure(label, state=state)

    def rmenu_check_cut(self):
        return self.rmenu_check_copy()

    def rmenu_check_copy(self):
        try:
            index = self.text.index('sel.first')
        except TclError:
            return 'disabled'
        return 'normal' if index else 'disabled'

    def rmenu_check_paste(self):
        try:
            self.text.tk.call('tk::GetSelection', self.text, 'CLIPBOARD')
        except TclError:
            return 'disabled'
        return 'normal'

    def about_dialog(self, event=None):
        """Help > About IDLE."""
        help_about.AboutDialog(self.top)
        return "break"

    def config_dialog(self, event=None):
        """Options > Configure IDLE."""
        # Settings changes are applied to all windows of instance_dict.
        from idlelib import configdialog
        configdialog.ConfigDialog(self.top, 'Settings')
        return "break"

    def help_dialog(self, event=None):
        """Help > IDLE Help."""
        from idlelib import help
        if self.root:
            parent = self.root
        else:
            parent = self.top
        help.show_idlehelp(parent)
        return "break"

    def python_docs(self, event=None):
        if sys.platform[:3] == 'win':
            try:
                os.startfile(self.help_url)
            except OSError as why:
                messagebox.showerror(title='Document Start Failure',
                                     message=str(why), parent=self.text)
        else:
            webbrowser.open(self.help_url)
        return "break"

    def cut(self, event):
        self.text.event_generate("<<Cut>>")
        return "break"

    def copy(self, event):
        if not self.text.tag_ranges("sel"):
            return None  # nothing to copy; let Tk do what it does
        self.text.event_generate("<<Copy>>")
        return "break"

    def paste(self, event):
        self.text.event_generate("<<Paste>>")
        self.text.see("insert")
        return "break"

    def select_all(self, event=None):
        self.text.tag_add("sel", "1.0", "end-1c")
        self.text.mark_set("insert", "1.0")
        self.text.see("insert")
        return "break"

    def remove_selection(self, event=None):
        self.text.tag_remove("sel", "1.0", "end")
        self.text.see("insert")
        return "break"

    def move_at_edge_if_selection(self, edge_index):
        """Return a <Left>/<Right> handler: with a selection and no
        modifier, the cursor goes to (just inside) the selection's edge
        before Tk moves it by one."""
        text = self.text
        edges = ("sel.first+1c", "sel.last-1c")

        def move_at_edge(event):
            if event.state & 5 == 0:  # neither Shift nor Control
                try:
                    text.index("sel.first")
                    text.mark_set("insert", edges[edge_index])
                except TclError:
                    pass
        return move_at_edge

    def del_word_left(self, event):
        self.text.event_generate('<Meta-Delete>')
        return "break"

    def del_word_right(self, event):
        self.text.event_generate('<Meta-d>')
        return "break"

    def find_event(self, event):
        search.find(self.text)
        return "break"

    def find_again_event(self, event):
        search.find_again(self.text)
        return "break"

    def find_selection_event(self, event):
        search.find_selection(self.text)
        return "break"

    def find_in_files_event(self, event):
        grep.grep(self.text, self.io, self.flist)
        return "break"

    def replace_event(self, event):
        replace.replace(self.text)
        return "break"

    def goto_line_event(self, event):
        text = self.text
        lineno = query.Goto(
            text, "Go To Line",
            "Enter a positive integer\n"
            "('big' = end of file):").result
        if lineno is not None:
            text.tag_remove("sel", "1.0", "end")
            text.mark_set("insert", f'{lineno}.0')
            text.see("insert")
            self.set_line_and_column()
        return "break"

    def open_module(self):
        """Ask for a module name; open its source; return the path."""
        try:
            name = self.text.get("sel.first", "sel.last").strip()
        except TclError:
            name = ''
        file_path = query.ModuleName(
            self.text, "Open Module",
            "Enter the name of a Python module\n"
            "to search on sys.path and open:",
            name).result
        if file_path is not None:
            if self.flist:
                self.flist.open(file_path)
            else:
                self.io.loadfile(file_path)
        return file_path

    def open_module_event(self, event):
        self.open_module()
        return "break"

    def open_module_browser(self, event=None):
        filename = self.io.filename
        if not (self.__class__.__name__ == 'PyShellEditorWindow'
                and filename):
            filename = self.open_module()
            if filename is None:
                return "break"
        from idlelib import browser
        browser.ModuleBrowser(self.root, filename)
        return "break"

    def open_path_browser(self, event=None):
        from idlelib import pathbrowser
        pathbrowser.PathBrowser(self.root)
        return "break"

    def open_turtle_demo(self, event=None):
        import subprocess
        cmd = [sys.executable, '-c',
               'from turtledemo.__main__ import main; main()']
        subprocess.Popen(cmd, shell=False)
        return "break"

    def home_callback(self, event):
        """Home goes to the first non-blank of the line (or after the
        prompt in the shell); pressed again, to the start of the line."""
        if event.state & 4 and event.keysym == "Home":
            return None  # Control-Home: Tk's default
        text = self.text
        try:
            iomark = text.index("iomark")
            at_prompt = (text.compare("iomark", "<=", "insert lineend") and
                         text.compare("insert linestart", "<=", "iomark"))
        except TclError:
            at_prompt = False
        if at_prompt:
            dest_col = int(iomark.split(".")[1])
        else:
            line = text.get("insert linestart", "insert lineend")
            dest_col = len(line) - len(line.lstrip(" \t"))
            if dest_col == int(text.index("insert").split('.')[1]):
                dest_col = 0
        dest = f"insert linestart+{dest_col}c"
        if event.state & 1 == 0:  # no Shift: just move
            text.tag_remove("sel", "1.0", "end")
        else:  # Shift: extend the selection from its anchor
            if not text.tag_ranges("sel"):
                text.mark_set("my_anchor", "insert")
            else:
                if text.compare(text.index("sel.first"), "<",
                                text.index("insert")):
                    text.mark_set("my_anchor", "sel.first")
                else:
                    text.mark_set("my_anchor", "sel.last")
            first = text.index(dest)
            last = text.index("my_anchor")
            if text.compare(first, ">", last):
                first, last = last, first
            text.tag_remove("sel", "1.0", "end")
            text.tag_add("sel", first, last)
        text.mark_set("insert", dest)
        text.see("insert")
        return "break"

    def set_status_bar(self):
        self.status_bar = self.MultiStatusBar(self.top)
        sep = Frame(self.top, height=1, borderwidth=1, background='grey75')
        if darwin:
            # Room for the resize handle of the window.
            self.status_bar.set_label('_padding1', '    ', side=RIGHT)
        self.status_bar.set_label('column', 'Col: ?', side=RIGHT)
        self.status_bar.set_label('line', 'Ln: ?', side=RIGHT)
        self.status_bar.pack(side=BOTTOM, fill=X)
        sep.pack(side=BOTTOM, fill=X)
        self.text.bind("<<set-line-and-column>>", self.set_line_and_column)
        self.text.event_add("<<set-line-and-column>>",
                            "<KeyRelease>", "<ButtonRelease>")
        self.text.after_idle(self.set_line_and_column)

    def set_line_and_column(self, event=None):
        if self.text is None:
            return  # closed
        line, column = self.text.index(INSERT).split('.')
        self.status_bar.set_label('column', f'Col: {column}')
        self.status_bar.set_label('line', f'Ln: {line}')

    # --- menus

    def createmenubar(self):
        mbar = self.menubar
        self.menudict = menudict = {}
        for name, label in self.menu_specs:
            underline, label = prepstr(label)
            postcommand = self.postwindowsmenu if name == 'window' else None
            menudict[name] = menu = Menu(mbar, name=name, tearoff=0,
                                         postcommand=postcommand)
            mbar.add_cascade(label=label, menu=menu, underline=underline)
        if macosx.isCarbonTk():
            # Carbon Tk puts the application menu first.
            menudict['application'] = menu = Menu(mbar, name='apple',
                                                  tearoff=0)
            mbar.add_cascade(label='IDLE', menu=menu)
        self.fill_menus()
        self.recent_files_menu = Menu(self.menubar, tearoff=0)
        self.menudict['file'].insert_cascade(3, label='Recent Files',
                                             underline=0,
                                             menu=self.recent_files_menu)
        self.base_helpmenu_length = self.menudict['help'].index(END)
        self.reset_help_menu_entries()

    def postwindowsmenu(self):
        # Rebuild the window list at the end of the Window menu.
        menu = self.menudict['window']
        end = menu.index("end")
        if end is None:
            end = -1
        if end > self.wmenu_end:
            menu.delete(self.wmenu_end + 1, end)
        window.add_windows_to_menu(menu)

    def update_menu_label(self, menu, index, label):
        """Change the label of item *index* (a pattern) of *menu*."""
        menuitem = self.menudict[menu]
        menuitem.entryconfig(index, label=label)

    def update_menu_state(self, menu, index, state):
        menuitem = self.menudict[menu]
        menuitem.entryconfig(index, state=state)

    def reset_help_menu_entries(self):
        """Put the additional help sources after the standard items."""
        help_list = idleConf.GetAllExtraHelpSourcesList()
        helpmenu = self.menudict['help']
        helpmenu_length = helpmenu.index(END)
        if helpmenu_length > self.base_helpmenu_length:
            helpmenu.delete(self.base_helpmenu_length + 1, helpmenu_length)
        if help_list:
            helpmenu.add_separator()
            for label, url in help_list:
                helpmenu.add_command(label=label,
                                     command=self._extra_help_callback(url))
        self.menu_extra_help = help_list

    def _extra_help_callback(self, resource):
        """Return a command opening a help file or URL."""
        def display_extra_help(helpfile=resource):
            if not helpfile.startswith(('www', 'http')):
                helpfile = os.path.normpath(helpfile)
            if sys.platform[:3] == 'win':
                try:
                    os.startfile(helpfile)
                except OSError as why:
                    messagebox.showerror(title='Document Start Failure',
                                         message=str(why), parent=self.text)
            else:
                webbrowser.open(helpfile)
        return display_extra_help

    def update_recent_files_list(self, new_file=None):
        """Add *new_file* to the recent files; update all windows' menus."""
        rf_list = []
        file_path = self.recent_files_path
        if file_path and os.path.exists(file_path):
            with open(file_path, encoding='utf_8', errors='replace') as f:
                rf_list = f.readlines()
        if new_file:
            new_file = os.path.abspath(new_file) + '\n'
            if new_file in rf_list:
                rf_list.remove(new_file)
            rf_list.insert(0, new_file)
        # Drop files that no longer exist.
        rf_list = [path for path in rf_list
                   if '\0' not in path and os.path.exists(path[:-1])]
        ulchars = "1234567890ABCDEFGHIJK"
        rf_list = rf_list[:len(ulchars)]
        if file_path:
            try:
                with open(file_path, 'w', encoding='utf_8',
                          errors='replace') as f:
                    f.writelines(rf_list)
            except OSError as err:
                if not getattr(self.root, "recentfiles_message", False):
                    self.root.recentfiles_message = True
                    messagebox.showwarning(
                        title='IDLE Warning',
                        message="Cannot save Recent Files list to disk.\n"
                                f"  {err}\n"
                                "Select OK to continue.",
                        parent=self.text)
        for instance in self.top.instance_dict:
            menu = instance.recent_files_menu
            menu.delete(0, END)
            for i, file_name in enumerate(rf_list):
                file_name = file_name.rstrip()
                menu.add_command(label=ulchars[i] + " " + file_name,
                                 command=instance._recent_file_callback(file_name),
                                 underline=0)

    def _recent_file_callback(self, file_name):
        def open_recent_file(fn_closure=file_name):
            self.io.open(editFile=fn_closure)
        return open_recent_file

    # --- titles and saving

    def saved_change_hook(self):
        short = self.short_title()
        long = self.long_title()
        if short and long:
            title = short + " - " + long + _py_version
        elif short:
            title = short + _py_version
        elif long:
            title = long
        else:
            title = "untitled"
        icon = short or long or title
        if not self.get_saved():
            title = f"*{title}*"
            icon = f"*{icon}"
        self.top.wm_title(title)
        self.top.wm_iconname(icon)

    def get_saved(self):
        return self.undo.get_saved()

    def set_saved(self, flag):
        self.undo.set_saved(flag)
        if flag:
            # Written (or loaded): this is the file's state on disk.
            self.mtime = self.last_mtime()

    def reset_undo(self):
        self.undo.reset_undo()

    def short_title(self):
        filename = self.io.filename
        return os.path.basename(filename) if filename else "untitled"

    def long_title(self):
        return self.io.filename or ""

    def center_insert_event(self, event):
        self.center()
        return "break"

    def center(self, mark="insert"):
        """Scroll so that *mark* is in the middle of the window."""
        top, bot = self.getwindowlines()
        lineno = self.getlineno(mark)
        height = bot - top
        newtop = max(1, lineno - height // 2)
        self.text.yview(float(newtop))

    def getwindowlines(self):
        """Return the first and last visible line numbers."""
        text = self.text
        top = self.getlineno("@0,0")
        bot = self.getlineno("@0,65535")
        if top == bot and text.winfo_height() == 1:
            # Not mapped yet: use the configured height.
            height = int(text['height'])
            bot = top + height - 1
        return top, bot

    def gotoline(self, lineno):
        """Select line *lineno* and scroll it to the middle."""
        if lineno is not None and lineno > 0:
            self.text.mark_set("insert", f"{lineno}.0")
            self.text.tag_remove("sel", "1.0", "end")
            self.text.tag_add("sel", "insert", "insert +1l")
            self.center()

    def getlineno(self, mark="insert"):
        return int(float(self.text.index(mark)))

    def get_geometry(self):
        """Return (width, height, x, y) of the window."""
        geom = self.top.wm_geometry()
        m = re.match(r"(\d+)x(\d+)\+(-?\d+)\+(-?\d+)", geom)
        return list(map(int, m.groups()))

    def close_event(self, event):
        self.close()
        return "break"

    def maybesave(self):
        if self.io:
            if not self.get_saved():
                if self.top.state() != 'normal':
                    self.top.deiconify()
                self.top.lower()
                self.top.lift()
            return self.io.maybesave()
        return None

    def close(self):
        try:
            reply = self.maybesave()
            if str(reply) != "cancel":
                self._close()
            return reply
        except AttributeError:  # already closed
            return None

    def _close(self):
        """Close without asking."""
        if self.io.filename:
            self.update_recent_files_list(new_file=self.io.filename)
        window.unregister_callback(self.postwindowsmenu)
        self.unload_extensions()
        self.io.close()
        self.io = None
        self.undo = None
        if self.color:
            self.color.close()
            self.color = None
        if self.code_context is not None:
            self.code_context.__del__()
        self.text = None
        self.tkinter_vars = None
        self.per.close()
        self.per = None
        self.top.destroy()
        if self.close_hook:
            self.close_hook()
        elif self.flist:
            self.flist.unregister_maybe_terminate(self)

    close_hook = None

    def set_close_hook(self, close_hook):
        self.close_hook = close_hook

    def filename_change_hook(self):
        if self.flist:
            self.flist.filename_changed_edit(self)
        self.saved_change_hook()
        self.top.update_windowlist_registry(self)
        self.ResetColorizer()

    def new_callback(self, event):
        dirname, basename = self.io.defaultfilename()
        self.flist.new(dirname)
        return "break"

    # --- colorizing, fonts, key bindings (also after configuration
    # changes)

    def ispythonsource(self, filename):
        """Guess whether *filename* (with the text) is Python code."""
        if not filename or os.path.isdir(filename):
            return True
        base, ext = os.path.splitext(os.path.basename(filename))
        if os.path.normcase(ext) in py_extensions:
            return True
        line = self.text.get('1.0', '1.0 lineend')
        return line.startswith('#!') and 'python' in line

    def _addcolorizer(self):
        if self.color:
            return
        if self.ispythonsource(self.io.filename):
            self.color = self.ColorDelegator()
        # The colorizer goes below the undo delegator: coloring changes
        # are not undone.
        if self.color:
            self.per.insertfilterafter(filter=self.color, after=self.undo)

    def _rmcolorizer(self):
        if not self.color:
            return
        self.color.removecolors()
        self.per.removefilter(self.color)
        self.color = None

    def ResetColorizer(self):
        """Apply the current theme (and file type)."""
        self._rmcolorizer()
        self._addcolorizer()
        EditorWindow.color_config(self.text)
        if self.code_context is not None:
            self.code_context.update_highlight_colors()
        if self.line_numbers is not None:
            self.line_numbers.update_colors()

    IDENTCHARS = string.ascii_letters + string.digits + "_"

    def colorize_syntax_error(self, text, pos):
        text.tag_add("ERROR", pos)
        char = text.get(pos)
        if char and char in self.IDENTCHARS:
            text.tag_add("ERROR", pos + " wordstart", pos)
        if '\n' == text.get(pos):  # at the end of a line
            text.mark_set("insert", pos)
        else:
            text.mark_set("insert", pos + "+1c")
        text.see(pos)

    def update_cursor_blink(self):
        cursorblink = idleConf.GetOption(
            'main', 'EditorWindow', 'cursor-blink', type='bool')
        if not cursorblink:
            self.text['insertofftime'] = 0
        else:
            self.text['insertofftime'] = getattr(idleConf, 'blink_off_time', 300)

    def ResetFont(self):
        """Apply the configured font."""
        if self.code_context is not None:
            self.code_context.update_font()
        if self.line_numbers is not None:
            self.line_numbers.update_font()
        new_font = idleConf.GetFont(self.root, 'main', 'EditorWindow')
        self.text['font'] = new_font
        self.set_width()

    def RemoveKeybindings(self):
        """Remove the bindings of the key set (before changing it)."""
        self.mainmenu.default_keydefs = keydefs = idleConf.GetCurrentKeySet()
        for event, keylist in keydefs.items():
            self.text.event_delete(event, *keylist)
        for name in self.get_standard_extension_names():
            xkeydefs = idleConf.GetExtensionBindings(name)
            for event, keylist in xkeydefs.items():
                self.text.event_delete(event, *keylist)

    def ApplyKeybindings(self):
        """Bind the keys of the current key set; update the menus."""
        self.mainmenu.default_keydefs = keydefs = idleConf.GetCurrentKeySet()
        self.apply_bindings()
        for name in self.get_standard_extension_names():
            xkeydefs = idleConf.GetExtensionBindings(name)
            if xkeydefs:
                self.apply_bindings(xkeydefs)
        # The accelerators shown in the menus.
        events_by_label = {}
        for menuname, items in self.mainmenu.menudefs:
            labels = events_by_label[menuname] = {}
            for item in items:
                if item:
                    labels[prepstr(item[0].lstrip('!'))[1]] = item[1]
        for menuname, menu in self.menudict.items():
            end = menu.index(END)
            if end is None:
                continue
            for index in range(end + 1):
                if menu.type(index) not in ('command', 'checkbutton'):
                    continue
                if not menu.entrycget(index, 'accelerator'):
                    continue
                label = menu.entrycget(index, 'label')
                event = events_by_label.get(menuname, {}).get(label)
                if event:
                    menu.entryconfig(index,
                                     accelerator=get_accelerator(keydefs, event))

    def set_notabs_indentwidth(self):
        """Use the configured indent width (unless indenting with tabs)."""
        if not self.usetabs:
            self.indentwidth = idleConf.GetOption(
                'main', 'Indent', 'num-spaces', type='int')

    def set_width(self):
        """Compute the width in characters of the text area."""
        text = self.text
        inner_padding = sum(map(text.tk.getint, [text.cget('border'),
                                                 text.cget('padx')]))
        pixel_width = text.winfo_reqwidth() - 2 * inner_padding
        zero_char_width = Font(text, font=text.cget('font')).measure('0')
        if zero_char_width > 0:
            self.width = pixel_width // zero_char_width
        else:
            # A font without a visible '0': use the configured width.
            self.width = text.tk.getint(text.cget('width'))

    def apply_bindings(self, keydefs=None):
        """Bind virtual events to key sequences.

        Caps Lock changes the case of letter keysyms, so each sequence
        whose keys include letters is also bound with their case swapped.
        """
        if keydefs is None:
            keydefs = self.mainmenu.default_keydefs
        text = self.text
        text.keydefs = keydefs
        for event, keylist in keydefs.items():
            sequences = [seq for seq in keylist if seq]
            for seq in keylist:
                variant = _swap_letter_case(seq)
                # A variant given explicitly is not added twice.
                if variant and variant not in sequences:
                    sequences.append(variant)
            if sequences:
                text.event_add(event, *sequences)

    def fill_menus(self, menudefs=None, keydefs=None):
        """Add the menu items of *menudefs* to the menus.

        Labels with a leading '!' become checkbuttons; '_' marks the
        underlined letter.
        """
        if menudefs is None:
            menudefs = self.mainmenu.menudefs
        if keydefs is None:
            keydefs = self.mainmenu.default_keydefs
        menudict = self.menudict
        text = self.text
        for mname, entrylist in menudefs:
            menu = menudict.get(mname)
            if not menu:
                continue
            for entry in entrylist:
                if entry is None:
                    menu.add_separator()
                    continue
                label, eventname = entry
                checkbutton = label[:1] == '!'
                if checkbutton:
                    label = label[1:]
                underline, label = prepstr(label)
                accelerator = get_accelerator(keydefs, eventname)

                def command(text=text, eventname=eventname):
                    text.event_generate(eventname)
                if checkbutton:
                    var = self.get_var_obj(eventname, BooleanVar)
                    menu.add_checkbutton(label=label, underline=underline,
                                         command=command,
                                         accelerator=accelerator,
                                         variable=var)
                else:
                    menu.add_command(label=label, underline=underline,
                                     command=command,
                                     accelerator=accelerator)

    def getvar(self, name):
        var = self.get_var_obj(name)
        if var:
            return var.get()
        raise NameError(name)

    def setvar(self, name, value, vartype=None):
        var = self.get_var_obj(name, vartype)
        if var:
            var.set(value)
        else:
            raise NameError(name)

    def get_var_obj(self, eventname, vartype=None):
        """The Tk variable of a checkbutton menu item (created if
        *vartype* is given)."""
        var = self.tkinter_vars.get(eventname)
        if not var and vartype:
            self.tkinter_vars[eventname] = var = vartype(self.text)
        return var

    # --- extensions (configured in config-extensions.def)

    extfiles = {}  # extension name -> module name, if not its lowercase

    def load_extensions(self):
        self.extensions = {}
        self.load_standard_extensions()

    def unload_extensions(self):
        for ins in list(self.extensions.values()):
            if hasattr(ins, "close"):
                ins.close()
        self.extensions = {}

    def load_standard_extensions(self):
        for name in self.get_standard_extension_names():
            try:
                self.load_extension(name)
            except Exception:
                print("Failed to load extension", repr(name))
                traceback.print_exc()

    def get_standard_extension_names(self):
        return idleConf.GetExtensions(editor_only=True)

    def load_extension(self, name):
        fname = self.extfiles.get(name, name.lower())
        try:
            mod = importlib.import_module('.' + fname, package=__package__)
        except (ImportError, TypeError):
            try:
                mod = importlib.import_module(fname)
            except ImportError:
                print("\nFailed to import extension: ", name)
                raise
        cls = getattr(mod, name)
        keydefs = idleConf.GetExtensionBindings(name)
        if hasattr(cls, "menudefs"):
            self.fill_menus(cls.menudefs, keydefs)
        ins = cls(self)
        self.extensions[name] = ins
        if keydefs:
            self.apply_bindings(keydefs)
            for vevent in keydefs:
                # <<z-in>> is handled by z_in_event.
                methodname = vevent.strip('<>').replace('-', '_') + '_event'
                if hasattr(ins, methodname):
                    self.text.bind(vevent, getattr(ins, methodname))

    # --- indentation

    def is_char_in_string(self, text_index):
        """Whether the character at *text_index* is in a string (or not
        colorized yet); without a colorizer, assume it may be."""
        if getattr(self, 'color', None):
            return bool(self.text.tag_prevrange("TODO", text_index) or
                        "STRING" in self.text.tag_names(text_index))
        return True

    def _build_char_in_string_func(self, startindex):
        """Return a function telling whether the character at an offset
        from *startindex* is in a string (for pyparse)."""
        def inner(offset, _startindex=startindex,
                  _icis=self.is_char_in_string):
            return _icis(_startindex + "+%dc" % offset)
        return inner

    def get_selection_indices(self):
        """Return (first, last) of the selection, or (None, None)."""
        try:
            first = self.text.index("sel.first")
            last = self.text.index("sel.last")
            return first, last
        except TclError:
            return None, None

    def get_tk_tabwidth(self):
        current = self.text['tabs'] or TK_TABWIDTH_DEFAULT
        return int(current)

    def set_tk_tabwidth(self, newtabwidth):
        text = self.text
        if self.get_tk_tabwidth() != newtabwidth:
            # Tab stops in pixels of the average character.
            pixels = text.tk.call("font", "measure", text["font"],
                                  "-displayof", text.master,
                                  "n" * newtabwidth)
            text.configure(tabs=pixels)

    def set_indentation_params(self, is_py_src, guess=True):
        if is_py_src and guess:
            i = self.guess_indent()
            if i is not None and 2 <= i <= 8:
                self.indentwidth = i
            if self.indentwidth != self.tabwidth:
                self.usetabs = False
        self.set_tk_tabwidth(self.tabwidth)

    def smart_backspace_event(self, event):
        """Delete a character; in leading whitespace, dedent to the
        previous indent stop."""
        text = self.text
        first, last = self.get_selection_indices()
        if first and last:
            text.delete(first, last)
            text.mark_set("insert", first)
            return "break"
        chars = text.get("insert linestart", "insert")
        if chars == '':
            if text.compare("insert", ">", "1.0"):
                text.delete("insert-1c")  # join with the previous line
            else:
                text.bell()
            return "break"
        if chars[-1] not in " \t":
            text.delete("insert-1c")
            return "break"
        # Delete blanks back to the previous multiple of indentwidth.
        tabwidth = self.tabwidth
        have = len(chars.expandtabs(tabwidth))
        want = ((have - 1) // self.indentwidth) * self.indentwidth
        ndelete = 0
        while chars and chars[-1] in " \t" and have > want:
            chars = chars[:-1]
            ndelete += 1
            have = len(chars.expandtabs(tabwidth))
        text.undo_block_start()
        text.delete(f"insert-{ndelete}c", "insert")
        if have < want:
            text.insert("insert", ' ' * (want - have),
                        self.user_input_insert_tags)
        text.undo_block_stop()
        return "break"

    def smart_indent_event(self, event):
        """Tab: indent a multiline selection; in leading whitespace,
        indent one level; elsewhere, insert blanks to the next stop."""
        text = self.text
        first, last = self.get_selection_indices()
        text.undo_block_start()
        try:
            if first and last:
                if index2line(first) != index2line(last):
                    return self.fregion.indent_region_event(event)
                text.delete(first, last)
                text.mark_set("insert", first)
            prefix = text.get("insert linestart", "insert")
            raw, effective = get_line_indent(prefix, self.tabwidth)
            if raw == len(prefix):
                # Only blanks before the cursor: indent the line.
                self.reindent_to(effective + self.indentwidth)
            else:
                if self.usetabs:
                    pad = '\t'
                else:
                    width = len(prefix.expandtabs(self.tabwidth))
                    n = self.indentwidth
                    pad = ' ' * (n - width % n)
                text.insert("insert", pad, self.user_input_insert_tags)
            text.see("insert")
            return "break"
        finally:
            text.undo_block_stop()

    def newline_and_indent_event(self, event):
        """Return: start a new line indented as the code requires."""
        text = self.text
        first, last = self.get_selection_indices()
        text.undo_block_start()
        try:
            if first and last:
                text.delete(first, last)
                text.mark_set("insert", first)
            line = text.get("insert linestart", "insert")

            # In the shell, the prompt is not part of the line.
            if (self.prompt_last_line and
                    line.startswith(self.prompt_last_line)):
                line = line[len(self.prompt_last_line):]

            indent_len = len(line) - len(line.lstrip(" \t"))
            if indent_len == len(line):
                # Only blanks before the cursor: move them down to the
                # new line, leaving this one empty.
                text.insert("insert linestart", '\n',
                            self.user_input_insert_tags)
                return "break"
            indent = line[:indent_len]

            # Blanks around the cursor are dropped.
            trailing = len(line) - len(line.rstrip(" \t"))
            if trailing:
                text.delete(f"insert-{trailing}c", "insert")
            while text.get("insert") in " \t":
                text.delete("insert")

            text.insert("insert", '\n', self.user_input_insert_tags)

            # Parse the code before the new line.
            y = pyparse.Parser(self.indentwidth, self.tabwidth)
            if not self.prompt_last_line:
                lno = index2line(text.index('insert'))
                for context in self.num_context_lines:
                    startat = max(lno - context, 1)
                    startatindex = f"{startat}.0"
                    rawtext = text.get(startatindex, "insert")
                    y.set_code(rawtext)
                    bod = y.find_good_parse_start(
                        self._build_char_in_string_func(startatindex))
                    if bod is not None or startat == 1:
                        break
                y.set_lo(bod or 0)
            else:
                r = text.tag_prevrange("console", "insert")
                startatindex = r[1] if r else "1.0"
                rawtext = text.get(startatindex, "insert")
                y.set_code(rawtext)
                y.set_lo(0)

            c = y.get_continuation_type()
            if c != pyparse.C_NONE:
                if c == pyparse.C_STRING_FIRST_LINE:
                    pass  # the string's text is not indented
                elif c == pyparse.C_STRING_NEXT_LINES:
                    text.insert("insert", indent, self.user_input_insert_tags)
                elif c == pyparse.C_BRACKET:
                    self.reindent_to(y.compute_bracket_indent())
                elif c == pyparse.C_BACKSLASH:
                    if y.get_num_lines_in_stmt() > 1:
                        text.insert("insert", indent,
                                    self.user_input_insert_tags)
                    else:
                        self.reindent_to(y.compute_backslash_indent())
                return "break"

            # A new statement: indent like the last one, one level more
            # after a block opener, one less after return/pass/...
            indent = y.get_base_indent_string()
            text.insert("insert", indent, self.user_input_insert_tags)
            if y.is_block_opener():
                self.smart_indent_event(event)
            elif indent and y.is_block_closer():
                self.smart_backspace_event(event)
            return "break"
        finally:
            text.see("insert")
            text.undo_block_stop()

    def _make_blanks(self, n):
        """Blanks for an indent of *n* columns (tabs if usetabs)."""
        if self.usetabs:
            ntabs, nspaces = divmod(n, self.tabwidth)
            return '\t' * ntabs + ' ' * nspaces
        return ' ' * n

    def reindent_to(self, column):
        """Replace the blanks before the cursor by an indent of *column*."""
        text = self.text
        text.undo_block_start()
        if text.compare("insert linestart", "!=", "insert"):
            text.delete("insert linestart", "insert")
        if column:
            text.insert("insert", self._make_blanks(column),
                        self.user_input_insert_tags)
        text.undo_block_stop()

    def guess_indent(self):
        """Return the indent width used by the first indented block, or
        None if there is none."""
        opener, indented = IndentSearcher(self.text).run()
        if opener and indented:
            raw, indentsmall = get_line_indent(opener, self.tabwidth)
            raw, indentlarge = get_line_indent(indented, self.tabwidth)
        else:
            return None
        return indentlarge - indentsmall

    def toggle_line_numbers_event(self, event=None):
        if self.line_numbers is None:
            return
        if self.line_numbers.is_shown:
            self.line_numbers.hide_sidebar()
            menu_label = "Show"
        else:
            self.line_numbers.show_sidebar()
            menu_label = "Hide"
        self.update_menu_label(menu='options', index='*ine*umbers',
                               label=f'{menu_label} Line Numbers')


def index2line(index):
    return int(float(index))


def _swap_letter_case(sequence):
    """Swap the case of the letter keysyms of a key sequence
    ('<Control-Key-x><Alt-Key-S>' -> '<Control-Key-X><Alt-Key-s>')."""
    return re.sub(r'(?<=[-<])([A-Za-z])>',
                  lambda m: m.group(1).swapcase() + '>', sequence)


_BLOCK_KEYWORDS = {'class', 'def', 'for', 'if', 'try', 'while', 'with',
                   'async', 'match'}


class IndentSearcher:
    """Find the first block opener of a text and the first indented
    line after it (used to guess the indent width of a file)."""

    def __init__(self, text):
        self.text = text
        self.i = 0
        self.finished = False
        self.blkopenline = self.indentedline = None

    def readline(self):
        if self.finished:
            return ""
        self.i += 1
        mark = f"{self.i}.0"
        if self.text.compare(mark, ">=", "end"):
            return ""
        return self.text.get(mark, mark + " lineend+1c")

    def run(self):
        """Return (opener line, indented line); either may be None."""
        try:
            for tok in tokenize.generate_tokens(self.readline):
                if self.blkopenline is None:
                    if (tok.type == tokenize.NAME and
                            tok.string in _BLOCK_KEYWORDS):
                        self.blkopenline = tok.line
                elif tok.type == tokenize.INDENT:
                    self.indentedline = tok.line
                    break
        except (tokenize.TokenError, SyntaxError):
            pass
        return self.blkopenline, self.indentedline


# --- menu helpers

def prepstr(s):
    """Return (underline index, label) for a label with '_' marking the
    underlined letter."""
    i = s.find('_')
    if i >= 0:
        s = s[:i] + s[i + 1:]
    return i, s


keynames = {
    'bracketleft': '[',
    'bracketright': ']',
    'slash': '/',
}


def get_accelerator(keydefs, eventname):
    """Return the accelerator text shown in the menus for an event."""
    keylist = keydefs.get(eventname)
    # Some bindings are shown by Tk itself (or not at all) on macOS.
    if (not keylist) or (macosx.isCocoaTk() and eventname in {
            "<<open-module>>", "<<goto-line>>", "<<change-indentwidth>>"}):
        return ""
    s = keylist[0]
    s = re.sub(r"-[a-z]\b", lambda m: m.group().upper(), s)
    s = re.sub(r"\b\w+\b", lambda m: keynames.get(m.group(), m.group()), s)
    s = s.replace("Key-", "")
    s = s.replace("Cancel", "Ctrl-Break")
    s = s.replace("Control-", "Ctrl-")
    s = s.replace("-", "+")
    s = s.replace("><", " ")
    s = s.replace("<", "")
    s = s.replace(">", "")
    return s


def fixwordbreaks(root):
    """Words are identifiers (for double-click and word motion)."""
    tk = root.tk
    tk.call('tcl_wordBreakAfter', 'a b', 0)  # make sure word.tcl is loaded
    tk.call('set', 'tcl_wordchars', r'\w')
    tk.call('set', 'tcl_nonwordchars', r'\W')


def _editor_window(parent):  # htest #
    # Tests run without a file list.
    root = parent
    fixwordbreaks(root)
    if sys.argv[1:]:
        filename = sys.argv[1]
    else:
        filename = None
    macosx.setupApp(root, None)
    edit = EditorWindow(root=root, filename=filename)
    text = edit.text
    text['height'] = 10
    for i in range(20):
        text.insert('insert', '  ' * i + str(i) + '\n')


if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_editor', verbosity=2, exit=False)
