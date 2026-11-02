"""Small helpers shared by IDLE modules that import nothing else of IDLE.

* py_extensions: file extensions of Python source files.
* Mouse-wheel handling: x11_buttons(), bind_wheel(), wheel_event().
* Tk adjustments applied once to the root: fix_scaling(),
  fix_word_breaks(), fix_x11_paste(), fix_win_hidpi().
* Command-line arguments for "Run... Customized": split_cli_args() and
  join_cli_args().
"""
import sys

# Python source files (the Python launcher on Windows runs .pyw files
# without a console; .pyi files are type stubs).
py_extensions = ('.py', '.pyw', '.pyi')


# ---------------------------------------------------------------------------
# Mouse wheel

def x11_buttons(widget):
    """Return True if Tk reports the wheel as button 4/5 presses (X11
    before Tk 8.7) rather than <MouseWheel> events."""
    import tkinter
    return widget._windowingsystem == 'x11' and tkinter.TkVersion < 8.7


def bind_wheel(widget, handler):
    """Bind *handler* to the events this Tk generates for the wheel."""
    if x11_buttons(widget):
        widget.bind('<Button-4>', handler)
        widget.bind('<Button-5>', handler)
    else:
        widget.bind('<MouseWheel>', handler)


def wheel_event(event, widget=None):
    """Scroll *widget* (default: the event's widget) for a wheel event.

    Every notch of the wheel scrolls five lines, whatever the platform
    reports as the size of the movement.  Return 'break'.
    """
    from tkinter import EventType
    if event.type == EventType.MouseWheel:
        up = event.delta > 0
    else:  # ButtonPress on X11: button 4 is up, 5 is down
        up = event.num == 4
    target = event.widget if widget is None else widget
    target.yview('scroll', -5 if up else 5, 'units')
    return 'break'


# ---------------------------------------------------------------------------
# Tk adjustments

def fix_scaling(root):
    """Turn named-font sizes given in pixels into points on high-resolution
    displays, so that fonts do not look tiny."""
    import tkinter.font
    scaling = float(root.tk.call('tk', 'scaling'))
    if scaling > 1.4:
        for name in tkinter.font.names(root):
            font = tkinter.font.Font(root=root, name=name, exists=True)
            size = int(font['size'])
            if size < 0:
                font['size'] = round(-0.75 * size)


def fix_word_breaks(root):
    """Make Tk's word motions (double-click, Control-Left/Right) treat
    letters, digits and '_' as word characters, like Python."""
    # Calling a word.tcl function makes Tk load the file first, so that
    # its defaults do not override ours later.
    root.tk.call('tcl_wordBreakAfter', 'a b', 0)
    root.tk.call('set', 'tcl_wordchars', r'\w')
    root.tk.call('set', 'tcl_nonwordchars', r'\W')


def fix_x11_paste(root):
    """On X11, make paste replace the selection as on other systems."""
    if root._windowingsystem == 'x11':
        for cls in 'Text', 'Entry', 'Spinbox':
            root.bind_class(cls, '<<Paste>>',
                            'catch {%W delete sel.first sel.last}\n' +
                            root.bind_class(cls, '<<Paste>>'))


def fix_win_hidpi():
    """Ask Windows not to blur IDLE on high-resolution displays."""
    if sys.platform != 'win32':
        return
    try:
        import ctypes
        PROCESS_SYSTEM_DPI_AWARE = 1
        ctypes.OleDLL('shcore').SetProcessDpiAwareness(PROCESS_SYSTEM_DPI_AWARE)
    except (ImportError, AttributeError, OSError):
        pass


# ---------------------------------------------------------------------------
# Command-line arguments

def _split_windows(s):
    """Split a command line the way the Windows C runtime builds argv.

    Blanks separate arguments outside double quotes.  Backslashes are
    literal unless they precede a quote: 2n backslashes and a quote give n
    backslashes and toggle quoting; 2n+1 give n backslashes and a literal
    quote.  Inside quotes, "" is a literal quote.
    """
    args = []
    i, n = 0, len(s)
    while True:
        while i < n and s[i] in ' \t':
            i += 1
        if i >= n:
            break
        arg = []
        quoted = False
        while i < n:
            c = s[i]
            if c == '\\':
                j = i
                while j < n and s[j] == '\\':
                    j += 1
                count = j - i
                if j < n and s[j] == '"':
                    arg.append('\\' * (count // 2))
                    if count % 2:
                        arg.append('"')
                        j += 1
                    i = j
                else:
                    arg.append('\\' * count)
                    i = j
            elif c == '"':
                if quoted and i + 1 < n and s[i + 1] == '"':
                    arg.append('"')
                    i += 2
                else:
                    quoted = not quoted
                    i += 1
            elif c in ' \t' and not quoted:
                break
            else:
                arg.append(c)
                i += 1
        args.append(''.join(arg))
    return args


def _join_windows(args):
    """Quote *args* so that _split_windows() returns them."""
    parts = []
    for arg in args:
        if arg and not any(c in arg for c in ' \t"'):
            parts.append(arg)
            continue
        out = ['"']
        backslashes = 0
        for c in arg:
            if c == '\\':
                backslashes += 1
                continue
            if c == '"':
                out.append('\\' * (2 * backslashes + 1) + '"')
            else:
                out.append('\\' * backslashes + c)
            backslashes = 0
        out.append('\\' * (2 * backslashes) + '"')
        parts.append(''.join(out))
    return ' '.join(parts)


def split_cli_args(s):
    """Split a command line string into arguments (platform rules)."""
    if sys.platform == 'win32':
        return _split_windows(s)
    import shlex
    return shlex.split(s)


def join_cli_args(args):
    """Join arguments into a command line string (platform rules)."""
    if sys.platform == 'win32':
        return _join_windows(args)
    import shlex
    return shlex.join(args)


if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_util', verbosity=2)
