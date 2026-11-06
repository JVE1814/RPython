"""The Help > About IDLE dialog."""
import os
import sys
import webbrowser
from platform import python_version, architecture

from tkinter import Toplevel, Frame, Label, Button, PhotoImage
from tkinter import SUNKEN, TOP, BOTTOM, LEFT, X, BOTH, W, EW, NSEW, E

from idlelib import textview

pyver = python_version()


def build_bits():
    """Return '64' or '32', the pointer size of this Python in bits."""
    if sys.platform == 'darwin':
        return '64' if sys.maxsize > 2**32 else '32'
    return architecture()[0][:2]


bits = build_bits()


class AboutDialog(Toplevel):
    """Version information, links and buttons showing license texts."""

    def __init__(self, parent, title=None, *, _htest=False, _utest=False):
        """With _utest the dialog is not shown modally (for tests)."""
        Toplevel.__init__(self, parent)
        self.configure(borderwidth=5)
        self.geometry("+%d+%d" % (
                        parent.winfo_rootx() + 30,
                        parent.winfo_rooty() + (30 if not _htest else 100)))
        self.bg = "#bbbbbb"
        self.fg = "#000000"
        self.create_widgets()
        self.resizable(height=False, width=False)
        self.title(title or f'About IDLE {pyver} ({bits} bit)')
        self.transient(parent)
        self.grab_set()
        self.protocol("WM_DELETE_WINDOW", self.ok)
        self.parent = parent
        self.button_ok.focus_set()
        self.bind('<Return>', self.ok)
        self.bind('<Escape>', self.ok)
        self._current_textview = None
        self._utest = _utest

        if not _utest:
            self.deiconify()
            self.wait_window()

    def create_widgets(self):
        bg, fg = self.bg, self.fg
        frame = Frame(self, borderwidth=2, relief=SUNKEN)
        frame_buttons = Frame(self)
        frame_buttons.pack(side=BOTTOM, fill=X)
        frame.pack(side=TOP, expand=True, fill=BOTH)
        self.button_ok = Button(frame_buttons, text='Close',
                                command=self.ok)
        self.button_ok.pack(padx=5, pady=5)

        frame_background = Frame(frame, bg=bg)
        frame_background.pack(expand=True, fill=BOTH)

        header = Label(frame_background, text='IDLE', fg=fg,
                       bg=bg, font=('courier', 24, 'bold'))
        header.grid(row=0, column=0, sticky=E, padx=10, pady=10)

        icons = os.path.join(os.path.abspath(os.path.dirname(__file__)),
                             'Icons')
        # Tk 8.6 reads PNG; older versions only GIF.
        ext = '.png' if self.info_patchlevel() >= (8, 6) else '.gif'
        self.icon_image = PhotoImage(master=self._root(),
                                     file=os.path.join(icons, 'idle_48' + ext))
        logo = Label(frame_background, image=self.icon_image, bg=bg)
        logo.grid(row=0, column=0, sticky=W, rowspan=2, padx=10, pady=10)

        byline = Label(frame_background, justify=LEFT, fg=fg, bg=bg,
                       text='Python\'s Integrated Development\n'
                            'and Learning Environment' + 5 * '\n')
        byline.grid(row=2, column=0, sticky=W, columnspan=3, padx=10, pady=5)

        email = Label(frame_background, text='email:  idle-dev@python.org',
                      justify=LEFT, fg=fg, bg=bg)
        email.grid(row=6, column=0, columnspan=2, sticky=W, padx=10, pady=0)

        docs_url = ('https://docs.python.org/%d.%d/library/idle.html' %
                    sys.version_info[:2])
        docs = Label(frame_background, text=docs_url, fg=fg, bg=bg,
                     justify=LEFT, cursor='hand2')
        docs.grid(row=7, column=0, columnspan=2, sticky=W, padx=10, pady=0)
        docs.bind('<Button-1>', lambda event: webbrowser.open(docs_url))

        Frame(frame_background, borderwidth=1, relief=SUNKEN,
              height=2, bg=bg).grid(row=8, column=0, sticky=EW,
                                    columnspan=3, padx=5, pady=5)

        tk_patchlevel = self.info_patchlevel()
        tk_version = '.'.join(map(str, tk_patchlevel[:3]))
        if tk_patchlevel.releaselevel != 'final':
            tk_version += tk_patchlevel.releaselevel[0] + str(tk_patchlevel.serial)
        pyver_label = Label(frame_background,
                            text='Python version:  ' + pyver, fg=fg, bg=bg)
        pyver_label.grid(row=9, column=0, sticky=W, padx=10, pady=0)
        tkver_label = Label(frame_background, text='Tk version:  ' + tk_version,
                            fg=fg, bg=bg)
        tkver_label.grid(row=9, column=1, sticky=W, padx=2, pady=0)

        py_buttons = Frame(frame_background, bg=bg)
        py_buttons.grid(row=10, column=0, columnspan=2, sticky=NSEW)
        self.py_license = Button(py_buttons, text='License', width=8,
                                 highlightbackground=bg,
                                 command=self.show_py_license)
        self.py_license.pack(side=LEFT, padx=10, pady=10)
        self.py_copyright = Button(py_buttons, text='Copyright', width=8,
                                   highlightbackground=bg,
                                   command=self.show_py_copyright)
        self.py_copyright.pack(side=LEFT, padx=10, pady=10)
        self.py_credits = Button(py_buttons, text='Credits', width=8,
                                 highlightbackground=bg,
                                 command=self.show_py_credits)
        self.py_credits.pack(side=LEFT, padx=10, pady=10)

        Frame(frame_background, borderwidth=1, relief=SUNKEN,
              height=2, bg=bg).grid(row=11, column=0, sticky=EW,
                                    columnspan=3, padx=5, pady=5)

        idle_label = Label(frame_background, text='IDLE version:   ' + pyver,
                           fg=fg, bg=bg)
        idle_label.grid(row=12, column=0, sticky=W, padx=10, pady=0)
        idle_buttons = Frame(frame_background, bg=bg)
        idle_buttons.grid(row=13, column=0, columnspan=3, sticky=NSEW)
        self.readme = Button(idle_buttons, text='README', width=8,
                             highlightbackground=bg,
                             command=self.show_readme)
        self.readme.pack(side=LEFT, padx=10, pady=10)
        self.idle_news = Button(idle_buttons, text='NEWS', width=8,
                                highlightbackground=bg,
                                command=self.show_idle_news)
        self.idle_news.pack(side=LEFT, padx=10, pady=10)
        self.idle_credits = Button(idle_buttons, text='Credits', width=8,
                                   highlightbackground=bg,
                                   command=self.show_idle_credits)
        self.idle_credits.pack(side=LEFT, padx=10, pady=10)

    # The license texts are site's _Printer objects (builtins license,
    # copyright and credits).
    def show_py_license(self):
        self.display_printer_text('About - License', license)

    def show_py_copyright(self):
        self.display_printer_text('About - Copyright', copyright)

    def show_py_credits(self):
        self.display_printer_text('About - Python Credits', credits)

    def show_idle_credits(self):
        self.display_file_text('About - Credits', 'CREDITS.txt', 'utf-8')

    def show_readme(self):
        self.display_file_text('About - Readme', 'README.txt', 'ascii')

    def show_idle_news(self):
        self.display_file_text('About - NEWS', 'News3.txt', 'utf-8')

    def display_printer_text(self, title, printer):
        """Show the full text of a site._Printer."""
        printer._Printer__setup()
        text = '\n'.join(printer._Printer__lines)
        self._current_textview = textview.view_text(
            self, title, text, _utest=self._utest)

    def display_file_text(self, title, filename, encoding=None):
        """Show a file of the idlelib directory."""
        path = os.path.join(os.path.abspath(os.path.dirname(__file__)),
                            filename)
        self._current_textview = textview.view_file(
            self, title, path, encoding, _utest=self._utest)

    def ok(self, event=None):
        """Close the dialog."""
        self.grab_release()
        self.destroy()


if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_help_about', verbosity=2, exit=False)
