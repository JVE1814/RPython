"""Dialog for choosing a new key binding (Settings > Keys).

Basic mode builds the key sequence from modifier checkbuttons and a list
of keys; advanced mode accepts any Tk key sequence typed in.
"""
from tkinter import Toplevel, Listbox, StringVar, TclError
from tkinter.ttk import Frame, Button, Checkbutton, Entry, Label, Scrollbar
from tkinter import messagebox
from tkinter.simpledialog import _setup_dialog
import string
import sys


FUNCTION_KEYS = ('F1', 'F2', 'F3', 'F4', 'F5', 'F6',
                 'F7', 'F8', 'F9', 'F10', 'F11', 'F12')
ALPHANUM_KEYS = tuple(string.ascii_lowercase + string.digits)
PUNCTUATION_KEYS = tuple('~!@#%^&*()_-+={}[]|;:,.<>/?')
WHITESPACE_KEYS = ('Tab', 'Space', 'Return')
EDIT_KEYS = ('BackSpace', 'Delete', 'Insert')
MOVE_KEYS = ('Home', 'End', 'Page Up', 'Page Down', 'Left Arrow',
             'Right Arrow', 'Up Arrow', 'Down Arrow')
AVAILABLE_KEYS = (ALPHANUM_KEYS + PUNCTUATION_KEYS + FUNCTION_KEYS +
                  WHITESPACE_KEYS + EDIT_KEYS + MOVE_KEYS)

# Tk keysyms of the key names shown in the list.
_KEYSYMS = {
    'Space': 'space',
    '~': 'asciitilde', '!': 'exclam', '@': 'at', '#': 'numbersign',
    '%': 'percent', '^': 'asciicircum', '&': 'ampersand',
    '*': 'asterisk', '(': 'parenleft', ')': 'parenright',
    '_': 'underscore', '-': 'minus', '+': 'plus', '=': 'equal',
    '{': 'braceleft', '}': 'braceright',
    '[': 'bracketleft', ']': 'bracketright', '|': 'bar',
    ';': 'semicolon', ':': 'colon', ',': 'comma', '.': 'period',
    '<': 'less', '>': 'greater', '/': 'slash', '?': 'question',
    'Page Up': 'Prior', 'Page Down': 'Next',
    'Left Arrow': 'Left', 'Right Arrow': 'Right',
    'Up Arrow': 'Up', 'Down Arrow': 'Down', 'Tab': 'Tab',
}


def translate_key(key, modifiers):
    """Return the Tk key part ('Key-keysym') for a key of the list."""
    key = _KEYSYMS.get(key, key)
    if 'Shift' in modifiers and key in string.ascii_lowercase:
        key = key.upper()
    return f'Key-{key}'


class GetKeysFrame(Frame):
    """The key sequence editor; `result` is the accepted sequence."""

    keyerror_title = 'Key Sequence Error'

    def __init__(self, parent, action, current_key_sequences):
        """*action* is the virtual event being bound;
        *current_key_sequences* are the sequences already in use (a list
        of lists)."""
        super().__init__(parent)
        self['borderwidth'] = 2
        self['relief'] = 'sunken'
        self.parent = parent
        self.action = action
        self.current_key_sequences = current_key_sequences
        self.result = ''
        self.key_string = StringVar(self)
        self.key_string.set('')
        self.set_modifiers_for_platform()
        self.modifier_vars = []
        for modifier in self.modifiers:
            variable = StringVar(self)
            variable.set('')
            self.modifier_vars.append(variable)
        self.advanced = False
        self.create_widgets()

    def showerror(self, *args, **kwargs):
        # Separate for testing.
        messagebox.showerror(*args, **kwargs)

    def create_widgets(self):
        # The basic and advanced frames share a grid cell; the one on top
        # of the stacking order is visible.
        self.frame_keyseq_basic = Frame(self, name='keyseq_basic')
        self.frame_keyseq_basic.grid(row=0, column=0, sticky='nsew',
                                     padx=5, pady=5)
        basic_title = Label(self.frame_keyseq_basic,
                            text=f"New keys for '{self.action}' :")
        basic_title.pack(anchor='w')
        basic_keys = Label(self.frame_keyseq_basic, justify='left',
                           textvariable=self.key_string, relief='groove',
                           borderwidth=2)
        basic_keys.pack(ipadx=5, ipady=5, fill='x')

        self.frame_controls_basic = Frame(self)
        self.frame_controls_basic.grid(row=1, column=0, sticky='nsew', padx=5)
        self.modifier_checkbuttons = {}
        column = 0
        for modifier, variable in zip(self.modifiers, self.modifier_vars):
            label = self.modifier_label.get(modifier, modifier)
            check = Checkbutton(self.frame_controls_basic,
                                command=self.build_key_string, text=label,
                                variable=variable, onvalue=modifier, offvalue='')
            check.grid(row=0, column=column, padx=2, sticky='w')
            self.modifier_checkbuttons[modifier] = check
            column += 1
        help_basic = Label(self.frame_controls_basic, justify='left',
                           text="Select the desired modifier keys\n"
                                "above, and the final key from the\n"
                                "list on the right.\n\n"
                                "Use upper case Symbols when using\n"
                                "the Shift modifier.  (Letters will be\n"
                                "converted automatically.)")
        help_basic.grid(row=1, column=0, columnspan=4, padx=2, sticky='w')
        self.list_keys_final = Listbox(self.frame_controls_basic, width=15,
                                       height=10, selectmode='single')
        self.list_keys_final.insert('end', *AVAILABLE_KEYS)
        self.list_keys_final.bind('<ButtonRelease-1>', self.final_key_selected)
        self.list_keys_final.grid(row=0, column=4, rowspan=4, sticky='ns')
        scroll_keys_final = Scrollbar(self.frame_controls_basic,
                                      orient='vertical',
                                      command=self.list_keys_final.yview)
        self.list_keys_final.config(yscrollcommand=scroll_keys_final.set)
        scroll_keys_final.grid(row=0, column=5, rowspan=4, sticky='ns')
        self.button_clear = Button(self.frame_controls_basic,
                                   text='Clear Keys',
                                   command=self.clear_key_seq)
        self.button_clear.grid(row=2, column=0, columnspan=4)

        self.frame_keyseq_advanced = Frame(self, name='keyseq_advanced')
        self.frame_keyseq_advanced.grid(row=0, column=0, sticky='nsew',
                                        padx=5, pady=5)
        advanced_title = Label(self.frame_keyseq_advanced, justify='left',
                               text=f"Enter new binding(s) for '{self.action}' :\n"
                                    "(These bindings will not be checked for "
                                    "validity!)")
        advanced_title.pack(anchor='w')
        self.advanced_keys = Entry(self.frame_keyseq_advanced,
                                   textvariable=self.key_string)
        self.advanced_keys.pack(fill='x')

        self.frame_help_advanced = Frame(self)
        self.frame_help_advanced.grid(row=1, column=0, sticky='nsew', padx=5)
        help_advanced = Label(self.frame_help_advanced, justify='left',
            text="Key bindings are specified using Tkinter keysyms as\n"
                 "in these samples: <Control-f>, <Shift-F2>, <F12>,\n"
                 "<Control-space>, <Meta-less>, <Control-Alt-Shift-X>.\n"
                 "Upper case is used when the Shift modifier is present!\n\n"
                 "'Emacs style' multi-keystroke bindings are specified as\n"
                 "follows: <Control-x><Control-y>, where the first key\n"
                 "is the 'do-nothing' keybinding.\n\n"
                 "Multiple separate bindings for one action should be\n"
                 "separated by a space, eg., <Alt-v> <Meta-v>.")
        help_advanced.grid(row=0, column=0, sticky='nsew')

        self.button_level = Button(self, command=self.toggle_level,
                                   text='Advanced Key Binding Entry >>')
        self.button_level.grid(row=2, column=0, stick='ew', padx=5, pady=5)
        self.toggle_level()
        self.toggle_level()  # start in basic mode, with basic on top

    def set_modifiers_for_platform(self):
        """Set the modifiers offered and their labels."""
        if sys.platform == "darwin":
            self.modifiers = ['Shift', 'Control', 'Option', 'Command']
        else:
            self.modifiers = ['Control', 'Alt', 'Shift']
        self.modifier_label = {'Control': 'Ctrl'}

    def toggle_level(self):
        """Switch between basic and advanced entry."""
        self.clear_key_seq()
        if not self.advanced:
            self.button_level.config(text='<< Basic Key Binding Entry')
            self.frame_keyseq_advanced.lift()
            self.frame_help_advanced.lift()
            self.advanced_keys.focus_set()
            self.advanced = True
        else:
            self.button_level.config(text='Advanced Key Binding Entry >>')
            self.frame_keyseq_basic.lift()
            self.frame_controls_basic.lift()
            self.advanced = False

    def final_key_selected(self, event=None):
        """A key was chosen in the list."""
        self.build_key_string()

    def build_key_string(self):
        """Show the sequence of the selected modifiers and key."""
        keylist = modifiers = self.get_modifiers()
        final_key = self.list_keys_final.get('anchor')
        if final_key:
            keylist.append(translate_key(final_key, modifiers))
        self.key_string.set(f"<{'-'.join(keylist)}>")

    def get_modifiers(self):
        """Return the selected modifiers, in display order."""
        return [v.get() for v in self.modifier_vars if v.get()]

    def clear_key_seq(self):
        """Deselect everything."""
        self.list_keys_final.select_clear(0, 'end')
        self.list_keys_final.yview('moveto', '0.0')
        for variable in self.modifier_vars:
            variable.set('')
        self.key_string.set('')

    def ok(self):
        """Accept the sequence if it is valid (result stays '' if not)."""
        self.result = ''
        keys = self.key_string.get().strip()
        if not keys:
            self.showerror(title=self.keyerror_title, parent=self,
                           message="No key specified.")
            return
        if (self.advanced or self.keys_ok(keys)) and self.bind_ok(keys):
            self.result = keys

    def keys_ok(self, keys):
        """Check a basic-mode sequence; report problems."""
        final_key = self.list_keys_final.get('anchor')
        modifiers = self.get_modifiers()
        title = self.keyerror_title
        in_use = [key for keylist in self.current_key_sequences
                  for key in keylist]
        if not keys.endswith('>'):
            message = 'Missing the final Key'
        elif not modifiers and final_key not in FUNCTION_KEYS + MOVE_KEYS:
            message = 'No modifier key(s) specified.'
        elif (modifiers == ['Shift'] and
              final_key not in FUNCTION_KEYS + MOVE_KEYS + ('Tab', 'Space')):
            message = ('The shift modifier by itself may not be used with '
                       'this key symbol.')
        elif keys in in_use:
            message = 'This key combination is already in use.'
        else:
            return True
        self.showerror(title=title, parent=self, message=message)
        return False

    def bind_ok(self, keys):
        """Return True if Tk accepts *keys* as a binding."""
        try:
            binding = self.bind(keys, lambda: None)
        except TclError as err:
            self.showerror(
                title=self.keyerror_title, parent=self,
                message=(f'The entered key sequence is not accepted.\n\n'
                         f'Error: {err}'))
            return False
        self.unbind(keys, binding)
        return True


class GetKeysWindow(Toplevel):
    """Modal window with a GetKeysFrame and OK/Cancel."""

    def __init__(self, parent, title, action, current_key_sequences,
                 *, _htest=False, _utest=False):
        super().__init__(parent)
        self.withdraw()  # shown when complete
        self['borderwidth'] = 5
        self.resizable(height=False, width=False)
        self.transient(parent)
        _setup_dialog(self)
        self.grab_set()
        self.protocol("WM_DELETE_WINDOW", self.cancel)
        self.parent = parent
        self.action = action
        self.current_key_sequences = current_key_sequences

        self.frame = frame = GetKeysFrame(self, action, current_key_sequences)
        self.protocol("WM_DELETE_WINDOW", self.cancel)
        frame_buttons = Frame(self)
        self.button_ok = Button(frame_buttons, text='OK',
                                width=8, command=self.ok)
        self.button_cancel = Button(frame_buttons, text='Cancel',
                                    width=8, command=self.cancel)
        self.button_ok.grid(row=0, column=0, padx=5, pady=5)
        self.button_cancel.grid(row=0, column=1, padx=5, pady=5)
        frame.pack(side='top', expand=True, fill='both')
        frame_buttons.pack(side='bottom', fill='x')
        self.update_idletasks()
        self.geometry(
            "+%d+%d" % (
                parent.winfo_rootx() +
                (parent.winfo_width()/2 - self.winfo_reqwidth()/2),
                parent.winfo_rooty() +
                ((parent.winfo_height()/2 - self.winfo_reqheight()/2)
                 if not _htest else 150)))
        self.title(title)
        if not _utest:
            self.deiconify()
            self.wait_window()

    @property
    def result(self):
        return self.frame.result

    @result.setter
    def result(self, value):
        self.frame.result = value

    def ok(self, event=None):
        self.frame.ok()
        self.grab_release()
        self.destroy()

    def cancel(self, event=None):
        self.result = ''
        self.grab_release()
        self.destroy()


if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_config_key', verbosity=2, exit=False)
