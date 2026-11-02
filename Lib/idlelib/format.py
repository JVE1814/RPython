"""Text formatting commands of the editor's Format menu.

FormatParagraph  -- rewrap the paragraph (or comment block) at the cursor
FormatRegion     -- indent, dedent, comment, uncomment, tabify, untabify
Indents          -- toggle tabs, change the indent width
Rstrip           -- strip trailing whitespace
"""
import re
from tkinter.messagebox import askyesno
from tkinter.simpledialog import askinteger

from idlelib.config import idleConf


# --- paragraphs

_indent_re = re.compile(r"[ \t]*")
_comment_header_re = re.compile(r"[ \t]*#*")


def is_all_white(line):
    """True if *line* contains only whitespace."""
    return line.strip() == ''


def get_indent(line):
    """The leading spaces and tabs of *line*."""
    return _indent_re.match(line).group()


def get_comment_header(line):
    """The leading whitespace and '#' characters of *line*."""
    return _comment_header_re.match(line).group()


def find_paragraph(text, mark):
    """Find the paragraph at or after index *mark* of a Text widget.

    A paragraph is a run of non-blank lines with the same comment header
    (indentation plus '#'s).  Return (first, last, comment_header, chars)
    where first and last are line start indices and chars is the text in
    between.
    """
    def line_at(n):
        return text.get(f"{n}.0", f"{n}.end")

    lineno = int(mark.split('.')[0])
    line = line_at(lineno)
    # Skip blank lines below the mark.
    while is_all_white(line) and text.compare(f"{lineno}.0", "<", "end"):
        lineno += 1
        line = line_at(lineno)
    header = get_comment_header(line)

    def same_paragraph(line):
        return (get_comment_header(line) == header and
                not is_all_white(line[len(header):]))

    start = lineno
    while same_paragraph(line):
        lineno += 1
        line = line_at(lineno)
    last = f"{lineno}.0"

    lineno = start - 1
    while lineno > 0 and same_paragraph(line_at(lineno)):
        lineno -= 1
    first = f"{lineno + 1}.0"
    return first, last, header, text.get(first, last)


_word_re = re.compile(r"(\S+)(\s*)")


def reformat_paragraph(data, limit):
    """Rewrap the first paragraph of *data* to lines of at most *limit*
    columns (a single long word stays on its own line).

    The first line keeps its indent, the others get the indent of the
    second line.  Words followed by more than one blank stay followed by
    two (sentence ends).
    """
    lines = data.split("\n")
    start = 0
    while start < len(lines) and is_all_white(lines[start]):
        start += 1
    if start == len(lines):
        return data
    end = start
    while end < len(lines) and not is_all_white(lines[end]):
        end += 1
    first_indent = get_indent(lines[start])
    later_indent = (get_indent(lines[start + 1]) if start + 1 < end
                    else first_indent)

    wrapped = []
    current = first_indent
    has_word = False
    for line in lines[start:end]:
        for word, space in _word_re.findall(line):
            if has_word and len((current + word).expandtabs()) > limit:
                wrapped.append(current.rstrip())
                current = later_indent
            current += word + (' ' if space in ('', ' ') else '  ')
            has_word = True
    wrapped.append(current.rstrip())
    return "\n".join(lines[:start] + wrapped + lines[end:])


def reformat_comment(data, limit, comment_header):
    """Rewrap the comment lines (or indented lines) of *data*; every line
    starts with *comment_header*."""
    size = len(comment_header)
    body = "\n".join(line[size:] for line in data.split("\n"))
    # Leave room for some text even with deep indentation.
    width = max(limit - len(comment_header.expandtabs()), 20)
    newlines = reformat_paragraph(body, width).split("\n")
    trailer = ''
    if not newlines[-1]:
        trailer = '\n'
        newlines.pop()
    return "\n".join(comment_header + line for line in newlines) + trailer


class FormatParagraph:
    """Format > Format Paragraph: rewrap the selection or the paragraph
    (comment block) at the cursor to max-width columns."""

    def __init__(self, editwin):
        self.editwin = editwin

    @classmethod
    def reload(cls):
        cls.max_width = idleConf.GetOption(
            'extensions', 'FormatParagraph', 'max-width', type='int', default=72)

    def close(self):
        self.editwin = None

    def format_paragraph_event(self, event, limit=None):
        """Rewrap; *limit* overrides the configured width (for tests)."""
        if limit is None:
            limit = self.max_width
        text = self.editwin.text
        first, last = self.editwin.get_selection_indices()
        if first and last:
            data = text.get(first, last)
            comment_header = get_comment_header(data)
        else:
            first, last, comment_header, data = find_paragraph(
                text, text.index("insert"))
        if comment_header:
            newdata = reformat_comment(data, limit, comment_header)
        else:
            newdata = reformat_paragraph(data, limit)
        text.tag_remove("sel", "1.0", "end")
        if newdata != data:
            text.mark_set("insert", first)
            text.undo_block_start()
            text.delete(first, last)
            text.insert(first, newdata)
            text.undo_block_stop()
        else:
            text.mark_set("insert", last)
        text.see("insert")
        return "break"


FormatParagraph.reload()


# --- regions

def _line_indent(line, tabwidth):
    """Return (characters of the indent, its width in columns)."""
    indent = get_indent(line)
    return len(indent), len(indent.expandtabs(tabwidth))


class FormatRegion:
    """The region commands of the Format menu; they act on the selected
    lines, or on the line of the cursor."""

    def __init__(self, editwin):
        self.editwin = editwin

    def get_region(self):
        """Return (head, tail, chars, lines) of the lines to change.

        head and tail are line start indices; chars is the text between
        them and lines is chars split at newlines.
        """
        text = self.editwin.text
        first, last = self.editwin.get_selection_indices()
        if first and last:
            head = text.index(first + " linestart")
            tail = text.index(last + "-1c lineend +1c")
        else:
            head = text.index("insert linestart")
            tail = text.index("insert lineend +1c")
        chars = text.get(head, tail)
        return head, tail, chars, chars.split("\n")

    def set_region(self, head, tail, chars, lines):
        """Replace the region with *lines* and select it (just ring the
        bell if nothing changes)."""
        text = self.editwin.text
        newchars = "\n".join(lines)
        if newchars == chars:
            text.bell()
            return
        text.tag_remove("sel", "1.0", "end")
        text.mark_set("insert", head)
        text.undo_block_start()
        text.delete(head, tail)
        text.insert(head, newchars)
        text.undo_block_stop()
        text.tag_add("sel", head, "insert")

    def _change_indents(self, delta):
        editwin = self.editwin
        head, tail, chars, lines = self.get_region()
        for pos, line in enumerate(lines):
            if line:
                size, width = _line_indent(line, editwin.tabwidth)
                width = max(width + delta, 0)
                lines[pos] = editwin._make_blanks(width) + line[size:]
        self.set_region(head, tail, chars, lines)
        return "break"

    def indent_region_event(self, event=None):
        """Indent the non-empty lines one level."""
        return self._change_indents(self.editwin.indentwidth)

    def dedent_region_event(self, event=None):
        """Dedent the non-empty lines one level."""
        return self._change_indents(-self.editwin.indentwidth)

    def comment_region_event(self, event=None):
        """Put '##' in front of the lines."""
        head, tail, chars, lines = self.get_region()
        # The last element is what follows the final newline.
        for pos in range(len(lines) - 1):
            lines[pos] = '##' + lines[pos]
        self.set_region(head, tail, chars, lines)
        return "break"

    def uncomment_region_event(self, event=None):
        """Remove '##' (or a single '#') at the start of the lines."""
        head, tail, chars, lines = self.get_region()
        for pos, line in enumerate(lines):
            if line.startswith('##'):
                lines[pos] = line[2:]
            elif line.startswith('#'):
                lines[pos] = line[1:]
        self.set_region(head, tail, chars, lines)
        return "break"

    def tabify_region_event(self, event=None):
        """Turn the leading blanks into tabs (asking for the tab width)."""
        head, tail, chars, lines = self.get_region()
        tabwidth = self._asktabwidth()
        if tabwidth is None:
            return None
        for pos, line in enumerate(lines):
            if line:
                size, width = _line_indent(line, tabwidth)
                tabs, spaces = divmod(width, tabwidth)
                lines[pos] = '\t' * tabs + ' ' * spaces + line[size:]
        self.set_region(head, tail, chars, lines)
        return "break"

    def untabify_region_event(self, event=None):
        """Expand all tabs (asking for the tab width)."""
        head, tail, chars, lines = self.get_region()
        tabwidth = self._asktabwidth()
        if tabwidth is None:
            return None
        lines = [line.expandtabs(tabwidth) for line in lines]
        self.set_region(head, tail, chars, lines)
        return "break"

    def _asktabwidth(self):
        """Ask for a tab width; None if cancelled."""
        return askinteger(
            "Tab width",
            "Columns per tab? (2-16)",
            parent=self.editwin.text,
            initialvalue=self.editwin.indentwidth,
            minvalue=2,
            maxvalue=16)


class Indents:
    """Format > Toggle Tabs and New Indent Width."""

    def __init__(self, editwin):
        self.editwin = editwin

    def toggle_tabs_event(self, event):
        editwin = self.editwin
        usetabs = editwin.usetabs
        question = (f"Indent with {'spaces' if usetabs else 'tabs'} "
                    "from now on?\n"
                    "The indent width will be 8 columns (the width of "
                    "a tab).")
        if askyesno("Toggle tabs", question, parent=editwin.text):
            editwin.usetabs = not usetabs
            # A tab is always 8 columns wide; mixing would confuse.
            editwin.indentwidth = 8
        return "break"

    def change_indentwidth_event(self, event):
        editwin = self.editwin
        new = askinteger(
            "Indent width",
            "New indent width (2-16)\n(Indenting with tabs always uses 8.)",
            parent=editwin.text,
            initialvalue=editwin.indentwidth,
            minvalue=2,
            maxvalue=16)
        if new and new != editwin.indentwidth and not editwin.usetabs:
            editwin.indentwidth = new
        return "break"


class Rstrip:
    """Format > Strip Trailing Whitespace."""

    def __init__(self, editwin):
        self.editwin = editwin

    def do_rstrip(self, event=None):
        text = self.editwin.text
        undo = self.editwin.undo
        undo.undo_block_start()

        last_line = int(float(text.index('end')))
        for lineno in range(1, last_line):
            line = text.get(f'{lineno}.0', f'{lineno}.end')
            stripped = len(line.rstrip())
            if stripped < len(line):
                text.delete(f'{lineno}.{stripped}', f'{lineno}.end')

        # In files (not in the shell), also drop blank lines at the end,
        # keeping a single final newline.
        if not hasattr(self.editwin, 'interp'):
            while (text.get('end-2c') == '\n' and
                   (text.index('end-2c') == '1.0' or
                    text.get('end-3c') == '\n')):
                text.delete('end-2c')
        undo.undo_block_stop()


if __name__ == "__main__":
    from unittest import main
    main('idlelib.idle_test.test_format', verbosity=2, exit=False)
