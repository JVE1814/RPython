"""The settings dialog (Options > Configure IDLE).

One notebook page per group of settings: fonts, highlighting (themes),
keys (key sets), windows, shell and editor, extensions (with the
additional help sources).  Every change of a widget's variable is
recorded in `changes` (a config.ConfigChanges) through `tracers`; Apply
and Ok write the changes and apply them to the open windows.
"""
import re

from tkinter import (Toplevel, Listbox, Canvas, StringVar, BooleanVar,
                     IntVar, TRUE, FALSE, TOP, BOTTOM, RIGHT, LEFT, SOLID,
                     GROOVE, NONE, BOTH, X, Y, W, E, EW, NS, NSEW, NW,
                     HORIZONTAL, VERTICAL, ANCHOR, ACTIVE, END, TclError,
                     EventType, OptionMenu, Text)
from tkinter.ttk import (Frame, LabelFrame, Button, Checkbutton, Entry,
                         Label, OptionMenu as TtkOptionMenu, Notebook,
                         Radiobutton, Scrollbar, Style, Spinbox)
from tkinter import colorchooser
import tkinter.font as tkfont
from tkinter import messagebox

from idlelib.config import idleConf, ConfigChanges
from idlelib.config_key import GetKeysWindow
from idlelib.dynoption import DynOptionMenu
from idlelib import macosx
from idlelib.query import SectionName, HelpSource
from idlelib.textview import view_text
from idlelib.autocomplete import AutoComplete
from idlelib.codecontext import CodeContext
from idlelib.parenmatch import ParenMatch
from idlelib.format import FormatParagraph
from idlelib.squeezer import Squeezer

changes = ConfigChanges()
# Classes whose class attributes cache settings (reloaded on Apply).
reloadables = (AutoComplete, CodeContext, ParenMatch, FormatParagraph,
               Squeezer)


class VarTrace:
    """Callbacks run when the dialog's Tk variables change.

    Variables are added (with their callbacks) while the widgets are
    created, before the values are loaded; attach() then starts the
    tracing.  A callback given as a tuple (config type, section, option)
    records the variable's value in `changes`.
    """

    def __init__(self):
        self.untraced = []
        self.traced = []

    def clear(self):
        self.untraced.clear()
        self.traced.clear()

    def add(self, var, callback):
        """Add *var* with *callback* (a function or a config tuple);
        return *var*."""
        if isinstance(callback, tuple):
            callback = self.make_callback(var, callback)
        self.untraced.append((var, callback))
        return var

    @staticmethod
    def make_callback(var, config):
        """Return a callback recording the value of *var* as *config*."""
        def default_callback(*params):
            value = var.get()
            if value == '':
                # A blank entry is no value: forget a recorded one.
                config_type, section, option = config
                changes[config_type].get(section, {}).pop(option, None)
                return
            changes.add_option(*config, value)
        return default_callback

    def attach(self):
        """Start tracing the added variables."""
        while self.untraced:
            var, callback = self.untraced.pop()
            var.trace_add('write', callback)
            self.traced.append((var, callback))

    def detach(self):
        """Stop tracing (the callbacks are kept for attach)."""
        while self.traced:
            var, callback = self.traced.pop()
            for mode, cbname in var.trace_info():
                var.trace_remove(mode, cbname)
            self.untraced.append((var, callback))


tracers = VarTrace()


class ConfigDialog(Toplevel):
    """The settings dialog."""

    def __init__(self, parent, title='', *, _htest=False, _utest=False):
        """*parent* is the window opening the dialog; its instance_dict
        holds the windows to update.  _utest leaves the dialog
        non-modal (for tests)."""
        Toplevel.__init__(self, parent)
        self.parent = parent
        if _htest:
            parent.instance_dict = {}
        if not _utest:
            self.withdraw()

        self.title(title or 'IDLE Preferences')
        x = parent.winfo_rootx() + 20
        y = parent.winfo_rooty() + (30 if not _htest else 150)
        self.geometry(f'+{x}+{y}')
        self.create_widgets()
        self.resizable(height=FALSE, width=FALSE)
        self.transient(parent)
        self.protocol("WM_DELETE_WINDOW", self.cancel)
        self.fontpage.fontlist.focus_set()
        tracers.attach()

        if not _utest:
            self.grab_set()
            self.wm_deiconify()
            self.wait_window()

    def create_widgets(self):
        self.frame = frame = Frame(self, padding=5)
        self.frame.grid(sticky="nwes")
        self.note = note = Notebook(frame)
        self.extpage = ExtPage(note)
        self.highpage = HighPage(note, self.extpage)
        self.fontpage = FontPage(note, self.highpage)
        self.keyspage = KeysPage(note, self.extpage)
        self.winpage = WinPage(note)
        self.shedpage = ShedPage(note)

        note.add(self.fontpage, text=' Fonts ')
        note.add(self.highpage, text='Highlights')
        note.add(self.keyspage, text=' Keys ')
        note.add(self.winpage, text=' Windows ')
        note.add(self.shedpage, text=' Shell/Ed ')
        note.add(self.extpage, text='Extensions')
        note.enable_traversal()
        note.pack(side=TOP, expand=TRUE, fill=BOTH)
        self.create_action_buttons().pack(side=BOTTOM)

    def create_action_buttons(self):
        """Return a frame with the Ok, Apply, Cancel and Help buttons."""
        padding_args = {'padding': (6, 3)} if macosx.isAquaTk() else {}
        outer = Frame(self.frame, padding=2)
        buttons_frame = Frame(outer, padding=2)
        self.buttons = {}
        for txt, cmd in (('Ok', self.ok),
                         ('Apply', self.apply),
                         ('Cancel', self.cancel),
                         ('Help', self.help)):
            self.buttons[txt] = Button(buttons_frame, text=txt, command=cmd,
                                       takefocus=FALSE, **padding_args)
            self.buttons[txt].pack(side=LEFT, padx=5)
        # Space between the pages and the buttons.
        Frame(outer, height=2, borderwidth=0).pack(side=TOP)
        buttons_frame.pack(side=BOTTOM)
        return outer

    def ok(self):
        """Apply the changes and close."""
        self.apply()
        self.destroy()

    def apply(self):
        """Save the changes and apply them to the open windows."""
        self.deactivate_current_config()
        changes.save_all()
        self.extpage.save_all_changed_extensions()
        self.activate_config_changes()

    def cancel(self):
        """Forget the changes and close."""
        changes.clear()
        self.destroy()

    def destroy(self):
        global font_sample_text
        font_sample_text = self.fontpage.font_sample.get('1.0', 'end')
        self.grab_release()
        super().destroy()

    def help(self):
        """Show the help for the current page."""
        page = self.note.tab(self.note.select(), option='text').strip()
        view_text(self, title='Help for IDLE preferences',
                  contents=help_common + help_pages.get(page, ''))

    def deactivate_current_config(self):
        """Remove the key bindings of the open windows (before the key
        set changes)."""
        for instance in self.parent.instance_dict.keys():
            instance.RemoveKeybindings()

    def activate_config_changes(self):
        """Apply the saved settings to the open windows."""
        for cls in reloadables:
            cls.reload()
        for instance in self.parent.instance_dict.keys():
            instance.ResetColorizer()
            instance.ResetFont()
            instance.set_notabs_indentwidth()
            instance.ApplyKeybindings()
            instance.reset_help_menu_entries()
            instance.update_cursor_blink()


font_sample_text = (
    '<ASCII/Latin1>\n'
    'AaBbCcDdEeFfGgHhIiJj\n1234567890#:+=(){}[]\n'
    '¢£¥§©«®¶½Ğ'
    'ÀÁÂÃÄÅÇÐØß\n'
    '\n<IPA,Greek,Cyrillic>\n'
    'ɐɕɘɞɟɤɫɮɰɷ'
    'ɻʁʃʆʎʞʢʫʭʯ\n'
    'ΑαΒβΓγΔδΕε'
    'ΖζΗηΘθΙιΚκ\n'
    'БбДдЖжПпФф'
    'ЧчЪъЭэѠѤѬӨ\n'
    '\n<Hebrew, Arabic>\n'
    'אבגדהוזחטי'
    'ךכלםמןנסעף\n'
    'ابجدهوزحطي'
    '٠١٢٣٤٥٦٧٨٩\n'
    '\n<Devanagari, Tamil>\n'
    '०१२३४५६७८९'
    'अआइईउऊएऐओऔ\n'
    '௦௧௨௩௪௫௬௭௮௯'
    'அஇஉஎ\n'
    '\n<East Asian>\n'
    '〇一二三四五六七八九\n'
    '汉字漢字人木火土金水\n'
    '가냐더려모뵤수유즈치\n'
    'あいうえおアイウエオ\n'
)


class FontPage(Frame):
    """Font name, size and bold; a sample of the font."""

    def __init__(self, master, highpage):
        super().__init__(master)
        self.highlight_sample = highpage.highlight_sample
        self.create_page_font()
        self.load_font_cfg()

    def create_page_font(self):
        self.font_name = tracers.add(StringVar(self), self.var_changed_font)
        self.font_size = tracers.add(StringVar(self), self.var_changed_font)
        self.font_bold = tracers.add(BooleanVar(self), self.var_changed_font)

        frame_font = LabelFrame(self, borderwidth=2, relief=GROOVE,
                                text=' Shell/Editor Font ')
        frame_sample = LabelFrame(self, borderwidth=2, relief=GROOVE,
                                  text=' Font Sample (Editable) ')
        frame_font_name = Frame(frame_font)
        frame_font_param = Frame(frame_font)
        font_name_title = Label(frame_font_name, justify=LEFT,
                                text='Font Face :')
        self.fontlist = Listbox(frame_font_name, height=15,
                                takefocus=True, exportselection=FALSE)
        self.fontlist.bind('<ButtonRelease-1>', self.on_fontlist_select)
        self.fontlist.bind('<KeyRelease-Up>', self.on_fontlist_select)
        self.fontlist.bind('<KeyRelease-Down>', self.on_fontlist_select)
        scroll_font = Scrollbar(frame_font_name)
        scroll_font.config(command=self.fontlist.yview)
        self.fontlist.config(yscrollcommand=scroll_font.set)
        font_size_title = Label(frame_font_param, text='Size :')
        self.sizelist = DynOptionMenu(frame_font_param, self.font_size, None)
        self.bold_toggle = Checkbutton(
            frame_font_param, variable=self.font_bold,
            onvalue=1, offvalue=0, text='Bold')
        self.font_sample = Text(frame_sample, width=20, height=20)
        self.font_sample.insert(END, font_sample_text)

        self.columnconfigure(1, weight=1)
        self.rowconfigure(2, weight=1)
        frame_font.grid(row=0, column=0, padx=5, pady=5)
        frame_sample.grid(row=0, column=1, rowspan=3, padx=5, pady=5,
                          sticky='nsew')
        frame_font_name.pack(side=TOP, padx=5, pady=5, fill=X)
        frame_font_param.pack(side=TOP, padx=5, pady=5, fill=X)
        font_name_title.pack(side=TOP, anchor=W)
        self.fontlist.pack(side=LEFT, expand=TRUE, fill=X)
        scroll_font.pack(side=LEFT, fill=Y)
        font_size_title.pack(side=LEFT, anchor=W)
        self.sizelist.pack(side=LEFT, anchor=W)
        self.bold_toggle.pack(side=LEFT, anchor=W, padx=20)
        self.font_sample.pack(expand=TRUE, fill=BOTH)

    def load_font_cfg(self):
        """Show the configured font (and fill the font list)."""
        configured_font = idleConf.GetFont(self, 'main', 'EditorWindow')
        font_name = configured_font[0].lower()
        font_size = configured_font[1]
        font_bold = configured_font[2] == 'bold'

        fonts = sorted(set(tkfont.families(self)))
        self.fontlist.delete(0, END)
        for font in fonts:
            self.fontlist.insert(END, font)
        self.font_name.set(font_name)
        lc_fonts = [s.lower() for s in fonts]
        try:
            current_font_index = lc_fonts.index(font_name)
            self.fontlist.see(current_font_index)
            self.fontlist.select_set(current_font_index)
            self.fontlist.select_anchor(current_font_index)
            self.fontlist.activate(current_font_index)
        except ValueError:
            pass
        self.sizelist.SetMenu(('7', '8', '9', '10', '11', '12', '13', '14',
                               '16', '18', '20', '22', '25', '29', '34', '40'),
                              font_size)
        self.font_bold.set(font_bold)
        self.set_samples()

    def var_changed_font(self, *params):
        """Record the font (all three parts) and update the samples."""
        changes.add_option('main', 'EditorWindow', 'font',
                           self.font_name.get())
        changes.add_option('main', 'EditorWindow', 'font-size',
                           self.font_size.get())
        changes.add_option('main', 'EditorWindow', 'font-bold',
                           self.font_bold.get())
        self.set_samples()

    def on_fontlist_select(self, event):
        """A font was chosen with the mouse (anchor) or keys (active)."""
        index = ACTIVE if event.type == EventType.KeyRelease else ANCHOR
        font = self.fontlist.get(index)
        self.font_name.set(font.lower())

    def set_samples(self, event=None):
        """Show the font in the font and highlight samples."""
        font_name = self.font_name.get()
        font_weight = tkfont.BOLD if self.font_bold.get() else tkfont.NORMAL
        new_font = (font_name, self.font_size.get(), font_weight)
        self.font_sample['font'] = new_font
        self.highlight_sample['font'] = new_font


class HighPage(Frame):
    """Color themes: a sample text, the element and color to change,
    built-in and custom themes."""

    def __init__(self, master, extpage):
        super().__init__(master)
        self.extpage = extpage
        self.cd = master.winfo_toplevel()
        self.style = Style(master)
        self.create_page_highlight()
        self.load_theme_cfg()

    # The sample text: (text, element) pairs.
    _sample = [
        ('#you can click here', 'comment'), ('\n', 'normal'),
        ('#to choose items', 'comment'), ('\n', 'normal'),
        ('code context section', 'context'), ('\n', 'normal'),
        ('|', 'linenumber'), ('def', 'keyword'), (' ', 'normal'),
        ('func', 'definition'), ('(param):\n', 'normal'),
        ('|', 'linenumber'), ('  """string"""', 'string'), ('\n', 'normal'),
        ('|', 'linenumber'), ('  var0 = ', 'normal'),
        ("'string'", 'string'), ('\n', 'normal'),
        ('|', 'linenumber'), ('  var1 = ', 'normal'),
        ("'selected'", 'hilite'), ('\n', 'normal'),
        ('|', 'linenumber'), ('  var2 = ', 'normal'),
        ("'found'", 'hit'), ('\n', 'normal'),
        ('|', 'linenumber'), ('  var3 = ', 'normal'),
        ('list', 'builtin'), ('(', 'normal'),
        ('None', 'keyword'), (')\n', 'normal'),
        ('|', 'linenumber'), ('  breakpoint("line")', 'break'),
        ('\n\n', 'normal'),
        ('>>>', 'console'), (' 3.14**2\n', 'normal'),
        ('9.8596', 'stdout'), ('\n', 'normal'),
        ('>>>', 'console'), (' pri ', 'normal'),
        ('n', 'error'), ('t(', 'normal'), (')\n', 'normal'),
        ('SyntaxError', 'stderr'), ('\n', 'normal'),
    ]

    def create_page_highlight(self):
        self.theme_elements = {
            'Normal Code or Text': 'normal',
            'Code Context': 'context',
            'Python Keywords': 'keyword',
            'Python Definitions': 'definition',
            'Python Builtins': 'builtin',
            'Python Comments': 'comment',
            'Python Strings': 'string',
            'Selected Text': 'hilite',
            'Found Text': 'hit',
            'Cursor': 'cursor',
            'Editor Breakpoint': 'break',
            'Shell Prompt': 'console',
            'Error Text': 'error',
            'Shell User Output': 'stdout',
            'Shell User Exception': 'stderr',
            'Line Number': 'linenumber',
        }
        self.builtin_name = tracers.add(
            StringVar(self), self.var_changed_builtin_name)
        self.custom_name = tracers.add(
            StringVar(self), self.var_changed_custom_name)
        self.fg_bg_toggle = BooleanVar(self)
        self.color = tracers.add(StringVar(self), self.var_changed_color)
        self.theme_source = tracers.add(
            BooleanVar(self), self.var_changed_theme_source)
        self.highlight_target = tracers.add(
            StringVar(self), self.var_changed_highlight_target)

        frame_custom = LabelFrame(self, borderwidth=2, relief=GROOVE,
                                  text=' Custom Highlighting ')
        frame_theme = LabelFrame(self, borderwidth=2, relief=GROOVE,
                                 text=' Highlighting Theme ')
        sample_frame = Frame(frame_custom, relief=SOLID, borderwidth=1)
        text = self.highlight_sample = Text(
            sample_frame, relief=SOLID, borderwidth=0,
            font=('courier', 12, ''), cursor='hand2', width=21, height=13,
            takefocus=FALSE, highlightthickness=0, wrap=NONE)
        text.bind('<Double-Button-1>', lambda e: 'break')
        text.bind('<B1-Motion>', lambda e: 'break')
        for chars, element in self._sample:
            text.insert(END, chars, element)
        for element_title, element in self.theme_elements.items():
            def tem(event, elem=element_title):
                # Clicking an element of the sample chooses it.
                self.highlight_target.set(elem)
            text.tag_bind(element, '<ButtonPress-1>', tem)
        text['state'] = 'disabled'
        self.style.configure('frame_color_set.TFrame', borderwidth=1,
                             relief='solid')
        self.frame_color_set = Frame(frame_custom, relief=SOLID,
                                     style='frame_color_set.TFrame')
        frame_fg_bg_toggle = Frame(frame_custom)
        self.button_set_color = Button(
            self.frame_color_set, text='Choose Color for :',
            command=self.get_color)
        self.targetlist = DynOptionMenu(
            self.frame_color_set, self.highlight_target, None,
            highlightthickness=0)
        self.fg_on = Radiobutton(
            frame_fg_bg_toggle, variable=self.fg_bg_toggle, value=1,
            text='Foreground', command=self.set_color_sample_binding)
        self.bg_on = Radiobutton(
            frame_fg_bg_toggle, variable=self.fg_bg_toggle, value=0,
            text='Background', command=self.set_color_sample_binding)
        self.fg_bg_toggle.set(1)
        self.button_save_custom = Button(
            frame_custom, text='Save as New Custom Theme',
            command=self.save_as_new_theme)

        theme_type_title = Label(frame_theme, text='Select : ')
        self.builtin_theme_on = Radiobutton(
            frame_theme, variable=self.theme_source, value=1,
            command=self.set_theme_type, text='a Built-in Theme')
        self.custom_theme_on = Radiobutton(
            frame_theme, variable=self.theme_source, value=0,
            command=self.set_theme_type, text='a Custom Theme')
        self.builtinlist = DynOptionMenu(
            frame_theme, self.builtin_name, None, command=None)
        self.customlist = DynOptionMenu(
            frame_theme, self.custom_name, None, command=None)
        self.button_delete_custom = Button(
            frame_theme, text='Delete Custom Theme',
            command=self.delete_custom)
        self.theme_message = Label(frame_theme, borderwidth=2)

        frame_custom.pack(side=LEFT, padx=5, pady=5, expand=TRUE, fill=BOTH)
        frame_theme.pack(side=TOP, padx=5, pady=5, fill=X)
        self.frame_color_set.pack(side=TOP, padx=5, pady=5, fill=X)
        frame_fg_bg_toggle.pack(side=TOP, padx=5, pady=0)
        sample_frame.pack(side=TOP, padx=5, pady=5, expand=TRUE, fill=BOTH)
        text.pack(expand=TRUE, fill=BOTH)
        self.button_set_color.pack(side=TOP, expand=TRUE, fill=X,
                                   padx=8, pady=4)
        self.targetlist.pack(side=TOP, expand=TRUE, fill=X, padx=8, pady=3)
        self.fg_on.pack(side=LEFT, anchor=E)
        self.bg_on.pack(side=RIGHT, anchor=W)
        self.button_save_custom.pack(side=BOTTOM, fill=X, padx=5, pady=5)
        theme_type_title.pack(side=TOP, anchor=W, padx=5, pady=5)
        self.builtin_theme_on.pack(side=TOP, anchor=W, padx=5)
        self.custom_theme_on.pack(side=TOP, anchor=W, padx=5, pady=2)
        self.builtinlist.pack(side=TOP, fill=X, padx=5, pady=5)
        self.customlist.pack(side=TOP, fill=X, anchor=W, padx=5, pady=5)
        self.button_delete_custom.pack(side=TOP, fill=X, padx=5, pady=5)
        self.theme_message.pack(side=TOP, fill=X, pady=5)

    def set_color_sample_binding(self, *args):
        self.set_color_sample()

    def load_theme_cfg(self):
        """Show the current theme (and fill the theme lists)."""
        self.theme_source.set(idleConf.GetOption(
            'main', 'Theme', 'default', type='bool', default=1))
        current_option = idleConf.CurrentTheme()
        builtins = sorted(idleConf.GetSectionList('default', 'highlight'))
        customs = sorted(idleConf.GetSectionList('user', 'highlight'))
        if self.theme_source.get():
            self.builtinlist.SetMenu(builtins, current_option)
            if not customs:
                self.custom_theme_on.state(('disabled',))
                self.custom_name.set('- no custom themes -')
            else:
                self.customlist.SetMenu(customs, customs[0])
        else:
            self.customlist.SetMenu(customs, current_option)
            self.builtinlist.SetMenu(builtins, builtins[0])
        self.set_theme_type()
        names = list(self.theme_elements)
        self.targetlist.SetMenu(names, names[0])
        self.paint_theme_sample()
        self.set_highlight_target()

    def var_changed_builtin_name(self, *params):
        """A built-in theme was chosen.  Themes newer than the classic
        ones are stored as 'name2' so that older IDLEs still find a
        theme they know in 'name'."""
        old_themes = ('IDLE Classic', 'IDLE New')
        value = self.builtin_name.get()
        if value not in old_themes:
            if idleConf.GetOption('main', 'Theme', 'name') not in old_themes:
                changes.add_option('main', 'Theme', 'name', old_themes[0])
            changes.add_option('main', 'Theme', 'name2', value)
            self.theme_message['text'] = 'New theme, see Help'
        else:
            changes.add_option('main', 'Theme', 'name', value)
            changes.add_option('main', 'Theme', 'name2', '')
            self.theme_message['text'] = ''
        self.paint_theme_sample()

    def var_changed_custom_name(self, *params):
        value = self.custom_name.get()
        if value != '- no custom themes -':
            changes.add_option('main', 'Theme', 'name', value)
            self.paint_theme_sample()

    def var_changed_theme_source(self, *params):
        value = self.theme_source.get()
        changes.add_option('main', 'Theme', 'default', value)
        if value:
            self.var_changed_builtin_name()
        else:
            self.var_changed_custom_name()

    def var_changed_color(self, *params):
        self.on_new_color_set()

    def var_changed_highlight_target(self, *params):
        self.set_highlight_target()

    def set_theme_type(self):
        """Enable the widgets of the chosen kind of theme."""
        if self.theme_source.get():
            self.builtinlist['state'] = 'normal'
            self.customlist['state'] = 'disabled'
            self.button_delete_custom.state(('disabled',))
        else:
            self.builtinlist['state'] = 'disabled'
            self.custom_theme_on.state(('!disabled',))
            self.customlist['state'] = 'normal'
            self.button_delete_custom.state(('!disabled',))

    def get_color(self):
        """Ask for a color for the target element.  A built-in theme is
        first copied to a new custom theme."""
        target = self.highlight_target.get()
        prev_color = self.style.lookup(self.frame_color_set['style'],
                                       'background')
        rgb, color_string = colorchooser.askcolor(
            parent=self, title='Pick new color for : ' + target,
            initialcolor=prev_color)
        if color_string and color_string != prev_color:
            if self.theme_source.get():
                message = ('Your changes will be saved as a new Custom '
                           'Theme. Enter a name for your new Custom Theme '
                           'below.')
                new_theme = self.get_new_theme_name(message)
                if not new_theme:
                    return
                self.create_new(new_theme)
                self.color.set(color_string)
            else:
                self.color.set(color_string)

    def on_new_color_set(self):
        """Show and record the new color of the target element."""
        new_color = self.color.get()
        self.style.configure('frame_color_set.TFrame', background=new_color)
        plane = 'foreground' if self.fg_bg_toggle.get() else 'background'
        sample_element = self.theme_elements[self.highlight_target.get()]
        self.highlight_sample.tag_config(sample_element, **{plane: new_color})
        theme = self.custom_name.get()
        theme_element = sample_element + '-' + plane
        changes.add_option('highlight', theme, theme_element, new_color)

    def get_new_theme_name(self, message):
        used_names = (idleConf.GetSectionList('user', 'highlight') +
                      idleConf.GetSectionList('default', 'highlight'))
        return SectionName(self, 'New Custom Theme', message,
                           used_names).result

    def save_as_new_theme(self):
        new_theme_name = self.get_new_theme_name('New Theme Name:')
        if new_theme_name:
            self.create_new(new_theme_name)

    def create_new(self, new_theme_name):
        """Save the current theme (with its pending changes) as a new
        custom theme and select it."""
        if self.theme_source.get():
            theme_type = 'default'
            theme_name = self.builtin_name.get()
        else:
            theme_type = 'user'
            theme_name = self.custom_name.get()
        new_theme = idleConf.GetThemeDict(theme_type, theme_name)
        if theme_name in changes['highlight']:
            new_theme.update(changes['highlight'][theme_name])
        self.save_new(new_theme_name, new_theme)
        custom_theme_list = sorted(idleConf.GetSectionList('user',
                                                           'highlight'))
        self.customlist.SetMenu(custom_theme_list, new_theme_name)
        self.theme_source.set(0)
        self.set_theme_type()

    def set_highlight_target(self):
        """The cursor has only a foreground color."""
        if self.highlight_target.get() == 'Cursor':
            self.fg_on.state(('disabled',))
            self.bg_on.state(('disabled',))
            self.fg_bg_toggle.set(1)
        else:
            self.fg_on.state(('!disabled',))
            self.bg_on.state(('!disabled',))
            self.fg_bg_toggle.set(1)
        self.set_color_sample()

    def set_color_sample(self):
        """Show the target element's current color."""
        tag = self.theme_elements[self.highlight_target.get()]
        plane = 'foreground' if self.fg_bg_toggle.get() else 'background'
        color = self.highlight_sample.tag_cget(tag, plane)
        self.style.configure('frame_color_set.TFrame', background=color)

    def paint_theme_sample(self):
        """Color the sample with the theme (and its pending changes)."""
        if self.theme_source.get():
            theme = self.builtin_name.get()
        else:
            theme = self.custom_name.get()
        for element_title in self.theme_elements:
            element = self.theme_elements[element_title]
            colors = idleConf.GetHighlight(theme, element)
            if element == 'cursor':
                colors['background'] = idleConf.GetHighlight(
                    theme, 'normal')['background']
            pending = changes['highlight'].get(theme, {})
            for plane in ('foreground', 'background'):
                if element + '-' + plane in pending:
                    colors[plane] = pending[element + '-' + plane]
            self.highlight_sample.tag_config(element, **colors)
        self.set_color_sample()

    def save_new(self, theme_name, theme):
        """Write *theme* (a dict) as a user theme."""
        user = idleConf.userCfg['highlight']
        if not user.has_section(theme_name):
            user.add_section(theme_name)
        for element, value in theme.items():
            user.SetOption(theme_name, element, value)

    askyesno = staticmethod(messagebox.askyesno)

    def delete_custom(self):
        """Delete the selected custom theme (after asking)."""
        theme_name = self.custom_name.get()
        if not self.askyesno(
                'Delete Theme',
                f'Delete the custom theme {theme_name!r}?', parent=self):
            return
        self.cd.deactivate_current_config()
        changes.delete_section('highlight', theme_name)
        item_list = sorted(idleConf.GetSectionList('user', 'highlight'))
        if not item_list:
            self.custom_theme_on.state(('disabled',))
            self.customlist.SetMenu(item_list, '- no custom themes -')
        else:
            self.customlist.SetMenu(item_list, item_list[0])
        self.theme_source.set(idleConf.defaultCfg['main'].Get('Theme',
                                                              'default'))
        self.builtin_name.set(idleConf.defaultCfg['main'].Get('Theme', 'name'))
        changes.save_all()
        self.extpage.save_all_changed_extensions()
        self.cd.activate_config_changes()
        self.set_theme_type()


class KeysPage(Frame):
    """Key sets: the bindings of the current set, built-in and custom
    sets."""

    def __init__(self, master, extpage):
        super().__init__(master)
        self.extpage = extpage
        self.cd = master.winfo_toplevel()
        self.create_page_keys()
        self.load_key_cfg()

    def create_page_keys(self):
        self.builtin_name = tracers.add(
            StringVar(self), self.var_changed_builtin_name)
        self.custom_name = tracers.add(
            StringVar(self), self.var_changed_custom_name)
        self.keyset_source = tracers.add(
            BooleanVar(self), self.var_changed_keyset_source)
        self.keybinding = tracers.add(
            StringVar(self), self.var_changed_keybinding)

        frame_custom = LabelFrame(self, borderwidth=2, relief=GROOVE,
                                  text=' Custom Key Bindings ')
        frame_key_sets = LabelFrame(self, borderwidth=2, relief=GROOVE,
                                    text=' Key Set ')
        frame_target = Frame(frame_custom)
        target_title = Label(frame_target, text='Action - Key(s)')
        scroll_target_y = Scrollbar(frame_target)
        scroll_target_x = Scrollbar(frame_target, orient=HORIZONTAL)
        self.bindingslist = Listbox(frame_target, takefocus=FALSE,
                                    exportselection=FALSE)
        for sequence in ('<ButtonRelease-1>', '<KeyRelease-Up>',
                         '<KeyRelease-Down>'):
            self.bindingslist.bind(sequence, self.on_bindingslist_select)
        scroll_target_y['command'] = self.bindingslist.yview
        scroll_target_x['command'] = self.bindingslist.xview
        self.bindingslist['yscrollcommand'] = scroll_target_y.set
        self.bindingslist['xscrollcommand'] = scroll_target_x.set
        self.button_new_keys = Button(
            frame_custom, text='Get New Keys for Selection',
            command=self.get_new_keys, state='disabled')

        frames = [Frame(frame_key_sets, padding=2, borderwidth=0)
                  for _ in range(2)]
        self.builtin_keyset_on = Radiobutton(
            frames[0], variable=self.keyset_source, value=1,
            command=self.set_keys_type, text='Use a Built-in Key Set')
        self.custom_keyset_on = Radiobutton(
            frames[0], variable=self.keyset_source, value=0,
            command=self.set_keys_type, text='Use a Custom Key Set')
        self.builtinlist = DynOptionMenu(
            frames[0], self.builtin_name, None, command=None)
        self.customlist = DynOptionMenu(
            frames[0], self.custom_name, None, command=None)
        self.button_delete_custom_keys = Button(
            frames[1], text='Delete Custom Key Set',
            command=self.delete_custom_keys)
        self.button_save_custom_keys = Button(
            frames[1], text='Save as New Custom Key Set',
            command=self.save_as_new_key_set)
        self.keys_message = Label(frames[0], borderwidth=2)

        frame_custom.pack(side=BOTTOM, padx=5, pady=5, expand=TRUE, fill=BOTH)
        frame_key_sets.pack(side=BOTTOM, padx=5, pady=5, fill=BOTH)
        self.button_new_keys.pack(side=BOTTOM, fill=X, padx=5, pady=5)
        frame_target.pack(side=LEFT, padx=5, pady=5, expand=TRUE, fill=BOTH)
        frame_target.columnconfigure(0, weight=1)
        frame_target.rowconfigure(1, weight=1)
        target_title.grid(row=0, column=0, columnspan=2, sticky=W)
        self.bindingslist.grid(row=1, column=0, sticky=NSEW)
        scroll_target_y.grid(row=1, column=1, sticky=NS)
        scroll_target_x.grid(row=2, column=0, sticky=EW)
        self.builtin_keyset_on.grid(row=0, column=0, sticky=W + NS)
        self.custom_keyset_on.grid(row=1, column=0, sticky=W + NS)
        self.builtinlist.grid(row=0, column=1, sticky=NSEW)
        self.customlist.grid(row=1, column=1, sticky=NSEW)
        self.keys_message.grid(row=0, column=2, sticky=NSEW, padx=5, pady=5)
        self.button_delete_custom_keys.pack(side=LEFT, fill=X, expand=True,
                                            padx=2)
        self.button_save_custom_keys.pack(side=LEFT, fill=X, expand=True,
                                          padx=2)
        frames[0].pack(side=TOP, fill=BOTH, expand=True)
        frames[1].pack(side=TOP, fill=X, expand=True, pady=2)

    def load_key_cfg(self):
        """Show the current key set (and fill the lists)."""
        self.keyset_source.set(idleConf.GetOption(
            'main', 'Keys', 'default', type='bool', default=1))
        current_option = idleConf.CurrentKeys()
        builtins = sorted(idleConf.GetSectionList('default', 'keys'))
        customs = sorted(idleConf.GetSectionList('user', 'keys'))
        if self.keyset_source.get():
            self.builtinlist.SetMenu(builtins, current_option)
            if not customs:
                self.custom_keyset_on.state(('disabled',))
                self.custom_name.set('- no custom keys -')
            else:
                self.customlist.SetMenu(customs, customs[0])
        else:
            self.customlist.SetMenu(customs, current_option)
            self.builtinlist.SetMenu(builtins, idleConf.default_keys())
        self.set_keys_type()
        self.load_keys_list(idleConf.CurrentKeys())

    def var_changed_builtin_name(self, *params):
        """A built-in key set was chosen; newer sets are stored as
        'name2' (see HighPage.var_changed_builtin_name)."""
        old_keys = ('IDLE Classic Windows', 'IDLE Classic Unix',
                    'IDLE Classic Mac', 'IDLE Classic OSX')
        value = self.builtin_name.get()
        if value not in old_keys:
            if idleConf.GetOption('main', 'Keys', 'name') not in old_keys:
                changes.add_option('main', 'Keys', 'name', old_keys[0])
            changes.add_option('main', 'Keys', 'name2', value)
            self.keys_message['text'] = 'New key set, see Help'
        else:
            changes.add_option('main', 'Keys', 'name', value)
            changes.add_option('main', 'Keys', 'name2', '')
            self.keys_message['text'] = ''
        self.load_keys_list(value)

    def var_changed_custom_name(self, *params):
        value = self.custom_name.get()
        if value != '- no custom keys -':
            changes.add_option('main', 'Keys', 'name', value)
            self.load_keys_list(value)

    def var_changed_keyset_source(self, *params):
        value = self.keyset_source.get()
        changes.add_option('main', 'Keys', 'default', value)
        if value:
            self.var_changed_builtin_name()
        else:
            self.var_changed_custom_name()

    def var_changed_keybinding(self, *params):
        """Record the new keys of the selected action (core bindings in
        the key set, extension bindings in the extension's section)."""
        value = self.keybinding.get()
        key_set = self.custom_name.get()
        event = self.bindingslist.get(ANCHOR).split()[0]
        if idleConf.IsCoreBinding(event):
            changes.add_option('keys', key_set, event, value)
        else:
            extn_name = idleConf.GetExtnNameForEvent(event)
            changes.add_option('extensions', extn_name + '_cfgBindings',
                               event, value)

    def set_keys_type(self):
        """Enable the widgets of the chosen kind of key set."""
        if self.keyset_source.get():
            self.builtinlist['state'] = 'normal'
            self.customlist['state'] = 'disabled'
            self.button_delete_custom_keys.state(('disabled',))
        else:
            self.builtinlist['state'] = 'disabled'
            self.custom_keyset_on.state(('!disabled',))
            self.customlist['state'] = 'normal'
            self.button_delete_custom_keys.state(('!disabled',))

    def get_new_keys(self):
        """Ask for new keys for the selected action.  A built-in key set
        is first copied to a new custom set."""
        list_index = self.bindingslist.index(ANCHOR)
        binding = self.bindingslist.get(list_index)
        bind_name = binding.split()[0]
        if self.keyset_source.get():
            current_key_set_name = self.builtin_name.get()
        else:
            current_key_set_name = self.custom_name.get()
        current_bindings = idleConf.GetCurrentKeySet()
        if current_key_set_name in changes['keys']:
            for event, keys in changes['keys'][current_key_set_name].items():
                current_bindings[event] = keys.split()
        current_key_sequences = list(current_bindings.values())
        new_keys = GetKeysWindow(self, 'Get New Keys', bind_name,
                                 current_key_sequences).result
        if new_keys:
            if self.keyset_source.get():
                message = ('Your changes will be saved as a new Custom Key '
                           'Set. Enter a name for your new Custom Key Set '
                           'below.')
                new_keyset = self.get_new_keys_name(message)
                if not new_keyset:
                    self.bindingslist.select_set(list_index)
                    self.bindingslist.select_anchor(list_index)
                    return
                self.create_new_key_set(new_keyset)
            self.bindingslist.delete(list_index)
            self.bindingslist.insert(list_index, bind_name + ' - ' + new_keys)
            self.bindingslist.select_set(list_index)
            self.bindingslist.select_anchor(list_index)
            self.keybinding.set(new_keys)
        else:
            self.bindingslist.select_set(list_index)
            self.bindingslist.select_anchor(list_index)

    def get_new_keys_name(self, message):
        used_names = (idleConf.GetSectionList('user', 'keys') +
                      idleConf.GetSectionList('default', 'keys'))
        return SectionName(self, 'New Custom Key Set', message,
                           used_names).result

    def save_as_new_key_set(self):
        new_keys_name = self.get_new_keys_name('New Key Set Name:')
        if new_keys_name:
            self.create_new_key_set(new_keys_name)

    def on_bindingslist_select(self, event):
        """An action was selected (with the arrow keys, the active one:
        Tk moves only the active item)."""
        if event is not None and getattr(event, 'type', None) == \
                EventType.KeyRelease:
            index = self.bindingslist.index(ACTIVE)
            self.bindingslist.selection_clear(0, END)
            self.bindingslist.selection_set(index)
            self.bindingslist.selection_anchor(index)
        self.button_new_keys.state(('!disabled',))

    def create_new_key_set(self, new_key_set_name):
        """Save the current key set (with its pending changes) as a new
        custom set and select it."""
        if self.keyset_source.get():
            prev_key_set_name = self.builtin_name.get()
        else:
            prev_key_set_name = self.custom_name.get()
        prev_keys = idleConf.GetCoreKeys(prev_key_set_name)
        new_keys = {event[2:-2]: ' '.join(keys)
                    for event, keys in prev_keys.items()}
        if prev_key_set_name in changes['keys']:
            new_keys.update(changes['keys'][prev_key_set_name])
        self.save_new_key_set(new_key_set_name, new_keys)
        custom_key_list = sorted(idleConf.GetSectionList('user', 'keys'))
        self.customlist.SetMenu(custom_key_list, new_key_set_name)
        self.keyset_source.set(0)
        self.set_keys_type()

    def load_keys_list(self, keyset_name):
        """List the bindings of *keyset_name* (with pending changes)."""
        reselect = False
        if self.bindingslist.curselection():
            reselect = True
            list_index = self.bindingslist.index(ANCHOR)
        keyset = idleConf.GetKeySet(keyset_name)
        pending = changes['keys'].get(keyset_name, {})
        self.bindingslist.delete(0, END)
        for bind_name in sorted(keyset):
            key = ' '.join(keyset[bind_name])
            name = bind_name[2:-2]
            key = pending.get(name, key)
            self.bindingslist.insert(END, name + ' - ' + key)
        if not reselect:
            list_index = 0  # select the first action
        if self.bindingslist.size():
            self.bindingslist.see(list_index)
            self.bindingslist.select_set(list_index)
            self.bindingslist.select_anchor(list_index)
            self.bindingslist.activate(list_index)
            self.button_new_keys.state(('!disabled',))

    @staticmethod
    def save_new_key_set(keyset_name, keyset):
        """Write *keyset* (event name -> keys) as a user key set."""
        user = idleConf.userCfg['keys']
        if not user.has_section(keyset_name):
            user.add_section(keyset_name)
        for event, value in keyset.items():
            user.SetOption(keyset_name, event, value)

    askyesno = staticmethod(messagebox.askyesno)

    def delete_custom_keys(self):
        """Delete the selected custom key set (after asking)."""
        keyset_name = self.custom_name.get()
        if not self.askyesno(
                'Delete Key Set',
                f'Delete the custom key set {keyset_name!r}?', parent=self):
            return
        self.cd.deactivate_current_config()
        changes.delete_section('keys', keyset_name)
        item_list = sorted(idleConf.GetSectionList('user', 'keys'))
        if not item_list:
            self.custom_keyset_on.state(('disabled',))
            self.customlist.SetMenu(item_list, '- no custom keys -')
        else:
            self.customlist.SetMenu(item_list, item_list[0])
        self.keyset_source.set(idleConf.defaultCfg['main'].Get('Keys',
                                                               'default'))
        self.builtin_name.set(idleConf.defaultCfg['main'].Get('Keys', 'name')
                              or idleConf.default_keys())
        changes.save_all()
        self.extpage.save_all_changed_extensions()
        self.cd.activate_config_changes()
        self.set_keys_type()


class WinPage(Frame):
    """Startup window, window size, indentation, completions, paren
    matching and paragraph width."""

    def __init__(self, master):
        super().__init__(master)
        self.init_validators()
        self.create_page_windows()
        self.load_windows_cfg()

    def init_validators(self):
        digits_or_empty_re = re.compile(r'[0-9]*')

        def is_digits_or_empty(s):
            return digits_or_empty_re.fullmatch(s) is not None
        self.digits_only = (self.register(is_digits_or_empty), '%P')

    def create_page_windows(self):
        self.startup_edit = tracers.add(
            IntVar(self), ('main', 'General', 'editor-on-startup'))
        self.win_width = tracers.add(
            StringVar(self), ('main', 'EditorWindow', 'width'))
        self.win_height = tracers.add(
            StringVar(self), ('main', 'EditorWindow', 'height'))
        self.indent_spaces = tracers.add(
            StringVar(self), ('main', 'Indent', 'num-spaces'))
        self.cursor_blink = tracers.add(
            BooleanVar(self), ('main', 'EditorWindow', 'cursor-blink'))
        self.autocomplete_wait = tracers.add(
            StringVar(self), ('extensions', 'AutoComplete', 'popupwait'))
        self.paren_style = tracers.add(
            StringVar(self), ('extensions', 'ParenMatch', 'style'))
        self.flash_delay = tracers.add(
            StringVar(self), ('extensions', 'ParenMatch', 'flash-delay'))
        self.paren_bell = tracers.add(
            BooleanVar(self), ('extensions', 'ParenMatch', 'bell'))
        self.format_width = tracers.add(
            StringVar(self), ('extensions', 'FormatParagraph', 'max-width'))

        frame_window = LabelFrame(self, borderwidth=2, relief=GROOVE,
                                  text=' Window Preferences')

        def row(text):
            frame = Frame(frame_window, borderwidth=0)
            frame.pack(side=TOP, padx=5, pady=0, fill=X)
            Label(frame, text=text).pack(side=LEFT, anchor=W, padx=5, pady=5)
            return frame

        def entry(frame, var, width=3):
            widget = Entry(frame, textvariable=var, width=width,
                           validatecommand=self.digits_only, validate='key')
            widget.pack(side=RIGHT, anchor=E, padx=10, pady=5)
            return widget

        frame = row('At Startup')
        self.startup_shell_on = Radiobutton(
            frame, variable=self.startup_edit, value=0,
            text='Open Shell Window')
        self.startup_editor_on = Radiobutton(
            frame, variable=self.startup_edit, value=1,
            text='Open Edit Window')
        self.startup_shell_on.pack(side=RIGHT, anchor=W, padx=5, pady=5)
        self.startup_editor_on.pack(side=RIGHT, anchor=W, padx=5, pady=5)

        frame = row('Initial Window Size  (in characters)')
        self.win_height_int = entry(frame, self.win_height)
        Label(frame, text='Height').pack(side=RIGHT, anchor=E, pady=5)
        self.win_width_int = entry(frame, self.win_width)
        Label(frame, text='Width').pack(side=RIGHT, anchor=E, pady=5)

        frame = row('Indent spaces (4 is standard)')
        self.indent_chooser = Spinbox(
            frame, textvariable=self.indent_spaces, from_=1, to=10,
            width=2, validatecommand=self.digits_only, validate='key')
        self.indent_chooser.pack(side=RIGHT, anchor=E, padx=10, pady=5)

        frame = row('')
        self.cursor_blink_bool = Checkbutton(
            frame, variable=self.cursor_blink, text="Cursor Blink")
        self.cursor_blink_bool.pack(side=LEFT, padx=5, pady=5)

        frame = row('Completions Popup Wait (milliseconds)')
        self.auto_wait_int = entry(frame, self.autocomplete_wait, 6)

        frame = row('Paren Match Style')
        self.paren_style_type = OptionMenu(
            frame, self.paren_style, 'opener', 'parens', 'expression')
        self.paren_flash_time = entry(frame, self.flash_delay, 6)
        Label(frame, text='Time Match Displayed (milliseconds)\n'
                          '(0 is until next input)').pack(side=RIGHT)
        self.bell_on = Checkbutton(frame, text="Bell on Mismatch",
                                   variable=self.paren_bell)
        self.paren_style_type.pack(side=LEFT, padx=5)
        self.bell_on.pack(side=RIGHT, anchor=E, padx=15, pady=5)

        frame = row('Format Paragraph Max Width')
        self.format_width_int = entry(frame, self.format_width, 4)

        frame_window.pack(side=TOP, padx=5, pady=5, expand=TRUE, fill=BOTH)

    def load_windows_cfg(self):
        self.startup_edit.set(idleConf.GetOption(
            'main', 'General', 'editor-on-startup', type='bool'))
        self.win_width.set(idleConf.GetOption(
            'main', 'EditorWindow', 'width', type='int'))
        self.win_height.set(idleConf.GetOption(
            'main', 'EditorWindow', 'height', type='int'))
        self.indent_spaces.set(idleConf.GetOption(
            'main', 'Indent', 'num-spaces', type='int'))
        self.cursor_blink.set(idleConf.GetOption(
            'main', 'EditorWindow', 'cursor-blink', type='bool'))
        self.autocomplete_wait.set(idleConf.GetOption(
            'extensions', 'AutoComplete', 'popupwait', type='int'))
        self.paren_style.set(idleConf.GetOption(
            'extensions', 'ParenMatch', 'style'))
        self.flash_delay.set(idleConf.GetOption(
            'extensions', 'ParenMatch', 'flash-delay', type='int'))
        self.paren_bell.set(idleConf.GetOption(
            'extensions', 'ParenMatch', 'bell'))
        self.format_width.set(idleConf.GetOption(
            'extensions', 'FormatParagraph', 'max-width', type='int'))


class ShedPage(Frame):
    """Shell and editor settings: autosave, line numbers, code context,
    squeezing of output."""

    def __init__(self, master):
        super().__init__(master)
        self.init_validators()
        self.create_page_shed()
        self.load_shelled_cfg()

    def init_validators(self):
        digits_or_empty_re = re.compile(r'[0-9]*')

        def is_digits_or_empty(s):
            return digits_or_empty_re.fullmatch(s) is not None
        self.digits_only = (self.register(is_digits_or_empty), '%P')

    def create_page_shed(self):
        self.autosave = tracers.add(
            IntVar(self), ('main', 'General', 'autosave'))
        self.line_numbers_default = tracers.add(
            BooleanVar(self), ('main', 'EditorWindow', 'line-numbers-default'))
        self.context_lines = tracers.add(
            StringVar(self), ('extensions', 'CodeContext', 'maxlines'))
        self.auto_squeeze_min_lines = tracers.add(
            StringVar(self), ('main', 'PyShell', 'auto-squeeze-min-lines'))

        frame_shell = LabelFrame(self, borderwidth=2, relief=GROOVE,
                                 text=' Shell Preferences')
        frame_editor = LabelFrame(self, borderwidth=2, relief=GROOVE,
                                  text=' Editor Preferences')

        frame = Frame(frame_shell, borderwidth=0)
        Label(frame, text='Auto-Squeeze Min. Lines:').pack(side=LEFT,
                                                           padx=5, pady=5)
        self.auto_squeeze_min_lines_int = Entry(
            frame, width=4, textvariable=self.auto_squeeze_min_lines,
            validatecommand=self.digits_only, validate='key')
        self.auto_squeeze_min_lines_int.pack(side=RIGHT, padx=10, pady=5)
        frame.pack(side=TOP, padx=5, pady=0, fill=X)

        frame = Frame(frame_editor, borderwidth=0)
        Label(frame, text='At Start of Run (F5)  ').pack(side=LEFT, padx=5)
        self.save_ask_on = Radiobutton(frame, variable=self.autosave,
                                       value=0, text="Prompt to Save")
        self.save_auto_on = Radiobutton(frame, variable=self.autosave,
                                        value=1, text='No Prompt')
        self.save_auto_on.pack(side=RIGHT, anchor=W, padx=5, pady=5)
        self.save_ask_on.pack(side=RIGHT, anchor=W, padx=5, pady=5)
        frame.pack(side=TOP, padx=5, pady=0, fill=X)

        frame = Frame(frame_editor, borderwidth=0)
        Label(frame, text='Show line numbers in new windows').pack(
            side=LEFT, padx=5)
        self.line_numbers_default_bool = Checkbutton(
            frame, variable=self.line_numbers_default, width=1)
        self.line_numbers_default_bool.pack(side=RIGHT, padx=5)
        frame.pack(side=TOP, padx=5, pady=0, fill=X)

        frame = Frame(frame_editor, borderwidth=0)
        Label(frame, text='Max Lines:').pack(side=LEFT, padx=5)
        self.context_int = Entry(
            frame, textvariable=self.context_lines, width=3,
            validatecommand=self.digits_only, validate='key')
        self.context_int.pack(side=TOP, padx=5, pady=5)
        frame.pack(side=TOP, padx=5, pady=0, fill=X)

        frame_shell.pack(side=TOP, padx=5, pady=5, fill=BOTH)
        frame_editor.pack(side=TOP, padx=5, pady=5, fill=BOTH)

    def load_shelled_cfg(self):
        self.auto_squeeze_min_lines.set(idleConf.GetOption(
            'main', 'PyShell', 'auto-squeeze-min-lines', type='int'))
        self.autosave.set(idleConf.GetOption(
            'main', 'General', 'autosave', default=0, type='bool'))
        self.line_numbers_default.set(idleConf.GetOption(
            'main', 'EditorWindow', 'line-numbers-default', type='bool'))
        self.context_lines.set(idleConf.GetOption(
            'extensions', 'CodeContext', 'maxlines', type='int'))


class ExtPage(Frame):
    """The options of the extensions, and the additional help sources."""

    def __init__(self, master):
        super().__init__(master)
        self.ext_defaultCfg = idleConf.defaultCfg['extensions']
        self.ext_userCfg = idleConf.userCfg['extensions']
        self.is_int = self.register(is_int)
        self.load_extensions()
        self.create_page_extensions()
        self.frame_help = HelpFrame(self, borderwidth=2, relief=GROOVE,
                                    text=' Help Menu Extensions ')
        self.frame_help.grid(row=1, column=0, columnspan=2,
                             sticky=NSEW, padx=5, pady=5)

    def create_page_extensions(self):
        self.extension_names = StringVar(self)
        self.rowconfigure(0, weight=1)
        self.columnconfigure(2, weight=1)
        self.extension_list = Listbox(self, listvariable=self.extension_names,
                                      selectmode='browse')
        self.extension_list.bind('<<ListboxSelect>>', self.extension_selected)
        scroll = Scrollbar(self, command=self.extension_list.yview)
        self.extension_list.yscrollcommand = scroll.set
        self.details_frame = LabelFrame(self, width=250, height=250)
        self.extension_list.grid(column=0, row=0, sticky='nws')
        scroll.grid(column=1, row=0, sticky='ns')
        self.details_frame.grid(column=2, row=0, sticky='nsew', padx=[10, 0])
        self.config_frame = {}
        self.current_extension = None

        ext_names = ''
        for ext_name in sorted(self.extensions):
            self.create_extension_frame(ext_name)
            ext_names = ext_names + f'{{{ext_name}}} '
        self.extension_names.set(ext_names)
        self.extension_list.selection_set(0)
        self.extension_selected(None)

    def load_extensions(self):
        """Read the options of all extensions (default and user files).

        self.extensions maps each extension to a list of option dicts
        (name, type, default, value, var), the 'enable' options first.
        """
        self.extensions = {}
        for ext_name in sorted(idleConf.GetExtensions(active_only=False)):
            names = set(self.ext_defaultCfg.GetOptionList(ext_name))
            if self.ext_userCfg.has_section(ext_name):
                names.update(self.ext_userCfg.GetOptionList(ext_name))
            enables = sorted(n for n in names if n.startswith('enable'))
            others = sorted(n for n in names if not n.startswith('enable'))
            options = []
            for opt_name in enables + others:
                def_str = self.ext_defaultCfg.Get(ext_name, opt_name,
                                                  raw=True)
                if def_str is None:
                    def_str = ''
                if def_str in ('True', 'False'):
                    def_obj, opt_type = def_str == 'True', 'bool'
                else:
                    try:
                        def_obj, opt_type = int(def_str), 'int'
                    except ValueError:
                        def_obj, opt_type = def_str, None
                try:
                    value = self.ext_userCfg.Get(
                        ext_name, opt_name, type=opt_type, raw=True,
                        default=def_obj)
                except ValueError:  # bad user value
                    value = def_obj
                var = StringVar(self)
                var.set(str(value))
                options.append({'name': opt_name, 'type': opt_type,
                                'default': def_str, 'value': value,
                                'var': var})
            self.extensions[ext_name] = options

    def extension_selected(self, event):
        """Show the options of the selected extension."""
        newsel = self.extension_list.curselection()
        if newsel:
            newsel = self.extension_list.get(newsel)
        if newsel is None or newsel != self.current_extension:
            if self.current_extension:
                self.details_frame.config(text='')
                self.config_frame[self.current_extension].grid_forget()
                self.current_extension = None
        if newsel:
            self.details_frame.config(text=newsel)
            self.config_frame[newsel].grid(column=0, row=0, sticky='nsew')
            self.current_extension = newsel

    def create_extension_frame(self, ext_name):
        """Create the option widgets of an extension."""
        f = VerticalScrolledFrame(self.details_frame, height=250, width=250)
        self.config_frame[ext_name] = f
        entry_area = f.interior
        for row, opt in enumerate(self.extensions[ext_name]):
            label = Label(entry_area, text=opt['name'])
            label.grid(row=row, column=0, sticky=NW)
            var = opt['var']
            if opt['type'] == 'bool':
                Checkbutton(entry_area, variable=var,
                            onvalue='True', offvalue='False', width=8
                            ).grid(row=row, column=1, sticky=W, padx=7)
            elif opt['type'] == 'int':
                Entry(entry_area, textvariable=var, validate='key',
                      validatecommand=(self.is_int, '%P'), width=10
                      ).grid(row=row, column=1, sticky=NSEW, padx=7)
            else:
                Entry(entry_area, textvariable=var, width=15
                      ).grid(row=row, column=1, sticky=NSEW, padx=7)

    def set_extension_value(self, section, opt):
        """Store an option in the user file (or remove it if it equals
        the default).  A blank value means the default.  Return True if
        the file content changed."""
        name = opt['name']
        default = opt['default']
        value = opt['var'].get().strip() or default
        opt['var'].set(value)
        if value == default:
            return self.ext_userCfg.RemoveOption(section, name)
        return self.ext_userCfg.SetOption(section, name, value)

    def save_all_changed_extensions(self):
        """Store all extension options; save the file if anything
        changed."""
        has_changes = False
        for ext_name, options in self.extensions.items():
            for opt in options:
                if self.set_extension_value(ext_name, opt):
                    has_changes = True
        if has_changes:
            self.ext_userCfg.Save()


class HelpFrame(LabelFrame):
    """The list of additional help sources (Help menu items)."""

    def __init__(self, master, **cfg):
        super().__init__(master, **cfg)
        self.create_frame_help()
        self.load_helplist()

    def create_frame_help(self):
        frame_helplist = Frame(self)
        self.helplist = Listbox(
            frame_helplist, height=5, takefocus=True, exportselection=FALSE)
        scroll_helplist = Scrollbar(frame_helplist)
        scroll_helplist['command'] = self.helplist.yview
        self.helplist['yscrollcommand'] = scroll_helplist.set
        self.helplist.bind('<ButtonRelease-1>', self.help_source_selected)
        self.helplist.bind('<KeyRelease-Up>', self.help_source_selected)
        self.helplist.bind('<KeyRelease-Down>', self.help_source_selected)

        frame_buttons = Frame(self)
        self.button_helplist_edit = Button(
            frame_buttons, text='Edit', state='disabled', width=8,
            command=self.helplist_item_edit)
        self.button_helplist_add = Button(
            frame_buttons, text='Add', width=8,
            command=self.helplist_item_add)
        self.button_helplist_remove = Button(
            frame_buttons, text='Remove', state='disabled', width=8,
            command=self.helplist_item_remove)

        frame_helplist.pack(side=LEFT, padx=5, pady=5, expand=TRUE, fill=BOTH)
        self.helplist.pack(side=LEFT, anchor=E, expand=TRUE, fill=BOTH)
        scroll_helplist.pack(side=RIGHT, anchor=W, fill=Y)
        frame_buttons.pack(side=RIGHT, padx=5, pady=5, fill=Y)
        self.button_helplist_edit.pack(side=TOP, anchor=W, pady=5)
        self.button_helplist_add.pack(side=TOP, anchor=W)
        self.button_helplist_remove.pack(side=TOP, anchor=W, pady=5)

    def help_source_selected(self, event):
        """An item was selected (with the keys, the active one)."""
        if event is not None and getattr(event, 'type', None) == \
                EventType.KeyRelease:
            self.helplist.selection_anchor(self.helplist.index(ACTIVE))
        self.set_add_delete_state()

    def set_add_delete_state(self):
        """Edit and Remove need a selected item."""
        if self.helplist.size() < 1 or not self.helplist.curselection():
            self.button_helplist_edit.state(('disabled',))
            self.button_helplist_remove.state(('disabled',))
        else:
            self.button_helplist_edit.state(('!disabled',))
            self.button_helplist_remove.state(('!disabled',))

    def helplist_item_add(self):
        """Ask for a new help source and add it."""
        help_source = HelpSource(self, 'New Help Source').result
        if help_source:
            self.user_helplist.append(help_source)
            self.helplist.insert(END, help_source[0])
            self.update_help_changes()

    def helplist_item_edit(self):
        """Edit the selected help source."""
        item_index = self.helplist.index(ANCHOR)
        help_source = self.user_helplist[item_index]
        new_help_source = HelpSource(
            self, 'Edit Help Source',
            menuitem=help_source[0],
            filepath=help_source[1],
        ).result
        if new_help_source and new_help_source != help_source:
            self.user_helplist[item_index] = new_help_source
            self.helplist.delete(item_index)
            self.helplist.insert(item_index, new_help_source[0])
            self.update_help_changes()
            self.set_add_delete_state()

    def helplist_item_remove(self):
        """Remove the selected help source."""
        item_index = self.helplist.index(ANCHOR)
        del self.user_helplist[item_index]
        self.helplist.delete(item_index)
        self.update_help_changes()
        self.set_add_delete_state()

    def update_help_changes(self):
        """Record the whole list (it replaces the saved one)."""
        changes['main']['HelpFiles'] = {}
        for num, source in enumerate(self.user_helplist, 1):
            changes.add_option('main', 'HelpFiles', str(num),
                               ';'.join(source[:2]))

    def load_helplist(self):
        self.user_helplist = idleConf.GetAllExtraHelpSourcesList()
        self.helplist.delete(0, 'end')
        for help_item in self.user_helplist:
            self.helplist.insert(END, help_item[0])
        self.set_add_delete_state()


def is_int(s):
    """Validator of integer entries: digits or nothing."""
    return s == '' or s.isdigit()


class VerticalScrolledFrame(Frame):
    """A frame with a vertical scrollbar; put widgets in .interior."""

    def __init__(self, parent, *args, **kw):
        Frame.__init__(self, parent, *args, **kw)
        vscrollbar = Scrollbar(self, orient=VERTICAL)
        vscrollbar.pack(fill=Y, side=RIGHT, expand=FALSE)
        canvas = Canvas(self, borderwidth=0, highlightthickness=0,
                        yscrollcommand=vscrollbar.set, width=240)
        canvas.pack(side=LEFT, fill=BOTH, expand=TRUE)
        vscrollbar.config(command=canvas.yview)
        canvas.xview_moveto(0)
        canvas.yview_moveto(0)
        self.interior = interior = Frame(canvas)
        interior_id = canvas.create_window(0, 0, window=interior, anchor=NW)

        def configure_interior(event):
            # The scroll region follows the size of the content.
            size = (interior.winfo_reqwidth(), interior.winfo_reqheight())
            canvas.config(scrollregion="0 0 %s %s" % size)
        interior.bind('<Configure>', configure_interior)

        def configure_canvas(event):
            if interior.winfo_reqwidth() != canvas.winfo_width():
                canvas.itemconfigure(interior_id, width=canvas.winfo_width())
        canvas.bind('<Configure>', configure_canvas)


help_common = '''\
When you click either the Apply or Ok buttons, settings in this
dialog that are different from IDLE's default are saved in
a .idlerc directory in your home directory.  Except as noted,
these changes apply to all versions of IDLE installed on this
machine.  Cancel forgets the changes made since the last Apply.
'''
help_pages = {
    'Fonts': '''
Font sample: the sample text can be edited to check how characters
of interest look in the chosen font.
''',
    'Highlights': '''
Highlighting:
Click a part of the sample to choose the element to color, then
Choose Color.  A built-in theme cannot be changed: the first color
change asks for the name of a new custom theme based on it.

The IDLE Dark theme is newer than the IDLE Classic and IDLE New
themes.  Older IDLE versions do not know it; they use the classic
theme stored with it instead.
''',
    'Keys': '''
Keys:
Select an action and click 'Get New Keys for Selection'.  As with
themes, a change of a built-in key set is saved as a new custom key
set.  Some key sets are newer than the classic ones; older IDLE
versions use the classic key set saved with them instead.  To
keep a set of keys for use with an older version, save it as a
custom key set under a different name.
''',
    'Windows': '''
Windows:
The initial size and the indentation apply to new windows.  The
completions popup wait is the delay before the list appears after
a '.' or a quote.
''',
    'Shell/Ed': '''
Shell Preferences: output with at least the given number of lines
is squeezed into a button.
Editor Preferences: whether Run asks to save the file first, line
numbers in new windows, the number of code context lines.
''',
    'Extensions': '''
Extensions:
The options of the extensions installed (enable an extension to use
it), and the additional items of the Help menu.
''',
}


if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_configdialog', verbosity=2, exit=False)
