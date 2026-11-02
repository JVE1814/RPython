"""The IDLE help window (Help > IDLE Help).

help.html is rendered in a read-only Text widget by a small HTML parser:
headings, paragraphs, lists, definition lists, preformatted blocks and
inline code/emphasis.  A Table of Contents menu jumps to the headings.
"""
from html.parser import HTMLParser
from os.path import abspath, dirname, isfile, join
from platform import python_version

from tkinter import Toplevel, Text, Menu
from tkinter.ttk import Frame, Menubutton, Scrollbar, Style
from tkinter import font as tkfont

from idlelib.config import idleConf


class HelpParser(HTMLParser):
    """Insert the content of an HTML help file into a HelpText.

    Only the elements used by help.html are understood; the text of
    <head>, <script> and <style> is skipped.
    """

    def __init__(self, text):
        HTMLParser.__init__(self, convert_charrefs=True)
        self.text = text
        self.tags = ''           # tags of the text being inserted
        self.chartags = ''       # inline tags (code, em, strong)
        self.skip_tag = None     # head, script or style being skipped
        self.skip = 0            # its nesting depth
        self.hprefix = ''        # heading tag (h1, h2, h3) while in one
        self.pre = False         # inside <pre>
        self.list_depth = 0
        self.nested_dl = False
        self.toc = []            # (heading text, index) for the menu
        self.header = ''         # text of the current heading
        self.new_block = True    # the next text starts a block

    def indent(self, amount=1):
        self.list_depth += amount
        self.tags = '' if self.list_depth == 0 else f'l{self.list_depth}'

    def _block(self, blank_lines=1):
        """Start a block: end the current line, then *blank_lines*."""
        if self.text.compare('end-1c', '==', '1.0'):
            return  # nothing yet
        line_ended = self.text.get('end-2c') == '\n'
        self.text.insert('end', '\n' * (blank_lines + (not line_ended)))

    def handle_starttag(self, tag, attrs):
        if self.skip_tag:
            if tag == self.skip_tag:
                self.skip += 1
            return
        if tag in ('head', 'script', 'style'):
            self.skip_tag = tag
            self.skip = 1
            return
        if tag in ('h1', 'h2', 'h3'):
            self._block()
            self.hprefix = tag
            self.header = ''
            self.text.insert('end', ' ', tag)
        elif tag == 'p':
            self._block(0 if self.list_depth else 1)
        elif tag in ('ul', 'ol'):
            self._block(0)
            self.indent()
        elif tag == 'li':
            self._block(0)
            self.text.insert('end', '• ', self.tags)
        elif tag == 'dl':
            self._block(0)
        elif tag == 'dt':
            self._block(0)
            self.chartags = 'em'
        elif tag == 'dd':
            self._block(0)
            self.indent()
        elif tag == 'pre':
            self._block()
            self.pre = True
            self.tags = 'preblock'
        elif tag in ('code', 'kbd', 'tt'):
            self.chartags = 'pre'
        elif tag in ('em', 'i', 'strong', 'b'):
            self.chartags = 'em'
        elif tag == 'br':
            self.text.insert('end', '\n')

    def handle_endtag(self, tag):
        if self.skip_tag:
            if tag == self.skip_tag:
                self.skip -= 1
                if not self.skip:
                    self.skip_tag = None
            return
        if tag in ('h1', 'h2', 'h3'):
            self.text.insert('end', ' ', tag)
            index = self.text.index('end-1c linestart')
            level = int(tag[1])
            self.toc.append(('  ' * (level - 1) + self.header.strip(), index))
            self.hprefix = ''
            self.text.insert('end', '\n')
        elif tag in ('ul', 'ol'):
            self.indent(-1)
        elif tag == 'dd':
            self.indent(-1)
        elif tag == 'dt':
            self.chartags = ''
        elif tag == 'pre':
            self.pre = False
            self.tags = '' if not self.list_depth else f'l{self.list_depth}'
            self.text.insert('end', '\n')
        elif tag in ('code', 'kbd', 'tt', 'em', 'i', 'strong', 'b'):
            self.chartags = ''

    def handle_data(self, data):
        if self.skip_tag:
            return
        if self.pre:
            tags = (self.tags, 'pre')
        else:
            data = ' '.join(data.split('\n'))
            # Collapse runs of blanks like a browser (keeping a leading
            # or trailing one between inline elements).
            while '  ' in data:
                data = data.replace('  ', ' ')
            if self.text.get('end-2c') in ('\n', ' ') or self.hprefix:
                if self.text.get('end-2c') == '\n' or (
                        self.hprefix and not self.header):
                    data = data.lstrip()
            if not data:
                return
            tags = (self.hprefix or self.tags, self.chartags)
        if self.hprefix:
            self.header += data
        self.text.insert('end', data, tags)


class HelpText(Text):
    """The read-only text of the help window."""

    def __init__(self, parent, filename):
        """Configure the fonts and tags, then insert the file's text."""
        uwide = idleConf.GetOption('main', 'EditorWindow', 'width', type='int')
        uhigh = idleConf.GetOption('main', 'EditorWindow', 'height', type='int')
        uhigh = 3 * uhigh // 4  # leave room for the taskbar etc.
        Text.__init__(self, parent, wrap='word', highlightthickness=0,
                      padx=5, borderwidth=0, width=uwide, height=uhigh)

        normalfont = self.findfont(['TkDefaultFont', 'arial', 'helvetica'])
        fixedfont = self.findfont(['TkFixedFont', 'monaco', 'courier'])
        self['font'] = (normalfont, 12)
        self.tag_configure('em', font=(normalfont, 12, 'italic'))
        self.tag_configure('h1', font=(normalfont, 20, 'bold'))
        self.tag_configure('h2', font=(normalfont, 18, 'bold'))
        self.tag_configure('h3', font=(normalfont, 15, 'bold'))
        self.tag_configure('pre', font=(fixedfont, 12), background='#f6f6ff')
        self.tag_configure('preblock', font=(fixedfont, 10), lmargin1=25,
                           borderwidth=1, relief='solid', background='#eeffcc')
        for depth in range(1, 5):
            margin = 25 * depth
            self.tag_configure(f'l{depth}', lmargin1=margin, lmargin2=margin)

        self.parser = HelpParser(self)
        with open(filename, encoding='utf-8') as f:
            contents = f.read()
        self.parser.feed(contents)
        self['state'] = 'disabled'

    def findfont(self, names):
        """Return the first font family of *names* available (in lower
        case for the named Tk fonts)."""
        for name in names:
            if name.lower() in (x.lower() for x in tkfont.names(root=self)):
                font = tkfont.Font(name=name, exists=True, root=self)
                return font.actual()['family']
            elif name.lower() in (x.lower() for x in
                                  tkfont.families(root=self)):
                return name
        return None


class HelpFrame(Frame):
    """The text with a scrollbar and a table of contents menu."""

    def __init__(self, parent, filename):
        Frame.__init__(self, parent)
        self.text = text = HelpText(self, filename)
        self.style = Style(parent)
        self['style'] = 'helpframe.TFrame'
        self.style.configure('helpframe.TFrame', background=text['background'])
        self.toc = toc = self.toc_menu(text)
        self.scroll = scroll = Scrollbar(self, command=text.yview)
        text['yscrollcommand'] = scroll.set

        self.rowconfigure(0, weight=1)
        self.columnconfigure(1, weight=1)  # the text
        toc.grid(row=0, column=0, sticky='nw')
        text.grid(row=0, column=1, sticky='nsew')
        scroll.grid(row=0, column=2, sticky='ns')

    def toc_menu(self, text):
        """Create the table of contents menubutton."""
        toc = Menubutton(self, text='TOC')
        drop = Menu(toc, tearoff=False)
        for label, index in text.parser.toc:
            drop.add_command(label=label,
                             command=lambda index=index: text.yview(index))
        toc['menu'] = drop
        return toc


class HelpWindow(Toplevel):
    """A window showing an HTML help file."""

    def __init__(self, parent, filename, title):
        Toplevel.__init__(self, parent)
        self.wm_title(title)
        self.protocol("WM_DELETE_WINDOW", self.destroy)
        self.frame = HelpFrame(self, filename)
        self.frame.grid(column=0, row=0, sticky='nsew')
        self.grid_columnconfigure(0, weight=1)
        self.grid_rowconfigure(0, weight=1)


def show_idlehelp(parent):
    """Show help.html; return the window (None if the file is missing)."""
    filename = join(abspath(dirname(__file__)), 'help.html')
    if not isfile(filename):
        return None
    return HelpWindow(parent, filename, f'IDLE Doc ({python_version()})')


if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_help', verbosity=2, exit=False)
