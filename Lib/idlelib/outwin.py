"""Output windows: editor windows used as files to write to.

Find in Files writes its hits to one; Debug > Go to File/Line (or the
context menu) opens the file at the line the cursor is on, if the line
looks like 'file "name", line N', 'name(N)' or 'name: N:'.
"""
import re

from tkinter import messagebox

from idlelib.editor import EditorWindow


# Patterns of lines naming a file and a line number; group 1 is the file
# name, group 2 the line number.
file_line_pats = [
    r'file "([^"]*)", line (\d+)',  # tracebacks
    r'([^\s]+)\((\d+)\)',           # name(N)
    r'^(\s*\S.*?):\s*(\d+):',       # name: N: (grep, compilers)
]

file_line_progs = None


def compile_progs():
    """Compile the patterns of file_line_pats (once)."""
    global file_line_progs
    file_line_progs = [re.compile(pat, re.IGNORECASE)
                       for pat in file_line_pats]


def file_line_helper(line):
    """Return (file name, line number) named in *line*, or None.

    The file must exist (it is opened to make sure).
    """
    if not file_line_progs:
        compile_progs()
    for prog in file_line_progs:
        match = prog.search(line)
        if match:
            filename, lineno = match.group(1, 2)
            try:
                f = open(filename)
                f.close()
                break
            except OSError:
                continue
    else:
        return None
    try:
        return filename, int(lineno)
    except TypeError:
        return None


class OutputWindow(EditorWindow):
    """An editor window that is written to like a file."""

    rmenu_specs = [
        ("Cut", "<<cut>>", "rmenu_check_cut"),
        ("Copy", "<<copy>>", "rmenu_check_copy"),
        ("Paste", "<<paste>>", "rmenu_check_paste"),
        (None, None, None),
        ("Go to file/line", "<<goto-file-line>>", None),
    ]

    allow_code_context = False

    def __init__(self, *args):
        EditorWindow.__init__(self, *args)
        self.text.bind("<<goto-file-line>>", self.goto_file_line)
        # The output is text: save it as such (opening files is as in
        # other windows).
        io = self.io
        io.defaultextension = '.txt'
        io.save_filetypes = (
            ("Text files", "*.txt", "TEXT"),
        ) + tuple(ft for ft in io.filetypes if ft[0] != "Text files")

    def ispythonsource(self, filename):
        """The output is never colorized as Python."""
        return False

    def short_title(self):
        return "Output"

    def maybesave(self):
        """Close without asking (the output can be produced again)."""
        return 'yes' if self.get_saved() else 'no'

    # The file interface.

    def write(self, s, tags=(), mark="insert"):
        """Insert *s* at *mark* (with *tags*); return its length."""
        assert isinstance(s, str)
        self.text.insert(mark, s, tags)
        self.text.see(mark)
        self.text.update()
        return len(s)

    def writelines(self, lines):
        for line in lines:
            self.write(line)

    def flush(self):
        pass

    def showerror(self, *args, **kwargs):
        messagebox.showerror(*args, **kwargs)

    def goto_file_line(self, event=None):
        """Open the file at the line named on the cursor's line (or the
        line above it)."""
        line = self.text.get("insert linestart", "insert lineend")
        result = file_line_helper(line)
        if not result:
            # Hits of Find in Files: the file name is on the line above.
            line = self.text.get("insert -1line linestart",
                                 "insert -1line lineend")
            result = file_line_helper(line)
            if not result:
                self.showerror(
                    "No special line",
                    "The line you point at doesn't look like "
                    "a valid file name followed by a line number.",
                    parent=self.text)
                return
        filename, lineno = result
        self.flist.gotofileline(filename, lineno)


class OnDemandOutputWindow:
    """Writes to an OutputWindow created at the first write."""

    tagdefs = {
        # Colors of the tags (the theme does not define them).
        "stdout":  {"foreground": "blue"},
        "stderr":  {"foreground": "#007700"},
    }

    def __init__(self, flist):
        self.flist = flist
        self.owin = None

    def write(self, s, tags, mark):
        if not self.owin:
            self.setup()
        self.owin.write(s, tags, mark)

    def setup(self):
        self.owin = owin = OutputWindow(self.flist)
        text = owin.text
        for tag, cnf in self.tagdefs.items():
            if cnf:
                text.tag_configure(tag, **cnf)
        text.tag_raise('sel')
        self.write = self.owin.write


if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_outwin', verbosity=2, exit=False)
