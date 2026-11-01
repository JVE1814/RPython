"""Python interface to the Tk GUI toolkit.

Tk is a portable widget set that is driven by Tcl commands.  This package
represents every Tk widget as an instance of a Python class.  Widgets are
created as children of a master widget (by default the root window, an
instance of Tk), configured with keyword options, arranged on the screen by
one of the geometry managers (pack, grid, place) and react to events through
callbacks bound with bind() or given as `command` options.

Example:

    import tkinter as tk
    root = tk.Tk()
    tk.Button(root, text='Quit', command=root.destroy).pack()
    root.mainloop()

The low-level interpreter is available as the `tk` attribute of every widget;
`widget.tk.call(...)` sends one command to Tcl.
"""

import collections
import enum
import re
import sys
import types

import _tkinter
from tkinter.constants import *

TclError = _tkinter.TclError
wantobjects = 1
_debug = False

TkVersion = float(_tkinter.TK_VERSION)
TclVersion = float(_tkinter.TCL_VERSION)

READABLE = _tkinter.READABLE
WRITABLE = _tkinter.WRITABLE
EXCEPTION = _tkinter.EXCEPTION


# ---------------------------------------------------------------------------
# Tcl list quoting

# Strings containing braces or backslashes are protected with backslashes
# (braces could be unbalanced); other strings with blanks or a leading quote
# are enclosed in braces.  Lists become a braced sub-list.
_brace_or_backslash = re.compile(r'[\\{}]')
_blank = re.compile(r'\s')


def _backslash_quote(s):
    out = []
    for ch in s:
        if ch in '\\{}':
            out.append('\\' + ch)
        elif ch == '\n':
            out.append('\\n')
        elif ch.isspace():
            out.append('\\' + ch)
        else:
            out.append(ch)
    s = ''.join(out)
    if s[0] == '"':
        s = '\\' + s
    return s


def _stringify(value):
    """Return *value* as a single Tcl list element."""
    if isinstance(value, (list, tuple)):
        if len(value) != 1:
            return '{%s}' % _join(value)
        s = _stringify(value[0])
        if _brace_or_backslash.search(s):
            s = '{%s}' % s
        return s
    if isinstance(value, bytes):
        value = str(value, 'latin1')
    else:
        value = str(value)
    if not value:
        return '{}'
    if _brace_or_backslash.search(value):
        return _backslash_quote(value)
    if value[0] == '"' or _blank.search(value):
        return '{%s}' % value
    return value


def _join(value):
    """Return a Tcl list built from the Python sequence *value*."""
    return ' '.join(map(_stringify, value))


def _flatten(seq):
    """Flatten nested tuples and lists into one tuple (None items dropped)."""
    out = []
    for item in seq:
        if isinstance(item, (tuple, list)):
            out.extend(_flatten(item))
        elif item is not None:
            out.append(item)
    return tuple(out)

try:
    _flatten = _tkinter._flatten
except AttributeError:
    pass


def _cnfmerge(cnfs):
    """Merge a sequence of option dictionaries into one dictionary."""
    if isinstance(cnfs, dict):
        return cnfs
    if isinstance(cnfs, (type(None), str)):
        return cnfs
    merged = {}
    for c in _flatten(cnfs):
        try:
            merged.update(c)
        except (AttributeError, TypeError) as exc:
            print("_cnfmerge: fallback due to:", exc)
            for k, v in c.items():
                merged[k] = v
    return merged

try:
    _cnfmerge = _tkinter._cnfmerge
except AttributeError:
    pass


def _splitdict(tk, v, cut_minus=True, conv=None):
    """Turn a Tcl list of alternating keys and values into a dict.

    With *cut_minus* a leading '-' is removed from each key; *conv* is applied
    to every value.
    """
    items = tk.splitlist(v)
    if len(items) % 2:
        raise RuntimeError('Tcl list representing a dict is expected '
                           'to contain an even number of elements')
    result = {}
    it = iter(items)
    for key in it:
        value = next(it)
        key = str(key)
        if cut_minus and key[:1] == '-':
            key = key[1:]
        if conv:
            value = conv(value)
        result[key] = value
    return result


class _VersionInfoType(collections.namedtuple('_VersionInfoType',
        ('major', 'minor', 'micro', 'releaselevel', 'serial'))):
    def __str__(self):
        if self.releaselevel == 'final':
            return '%d.%d.%d' % (self.major, self.minor, self.micro)
        return '%d.%d%s%d' % (self.major, self.minor,
                              self.releaselevel[0], self.serial)


def _parse_version(version):
    m = re.fullmatch(r'(\d+)\.(\d+)([ab.])(\d+)', version)
    if m is None:
        raise ValueError('invalid Tcl/Tk version %r' % (version,))
    major, minor, sep, num = m.groups()
    major = int(major)
    minor = int(minor)
    num = int(num)
    if sep == '.':
        return _VersionInfoType(major, minor, num, 'final', 0)
    level = 'alpha' if sep == 'a' else 'beta'
    return _VersionInfoType(major, minor, 0, level, num)


# ---------------------------------------------------------------------------
# Events

@enum._simple_enum(enum.StrEnum)
class EventType:
    KeyPress = '2'
    Key = KeyPress
    KeyRelease = '3'
    ButtonPress = '4'
    Button = ButtonPress
    ButtonRelease = '5'
    Motion = '6'
    Enter = '7'
    Leave = '8'
    FocusIn = '9'
    FocusOut = '10'
    Keymap = '11'
    Expose = '12'
    GraphicsExpose = '13'
    NoExpose = '14'
    Visibility = '15'
    Create = '16'
    Destroy = '17'
    Unmap = '18'
    Map = '19'
    MapRequest = '20'
    Reparent = '21'
    Configure = '22'
    ConfigureRequest = '23'
    Gravity = '24'
    ResizeRequest = '25'
    Circulate = '26'
    CirculateRequest = '27'
    Property = '28'
    SelectionClear = '29'
    SelectionRequest = '30'
    Selection = '31'
    Colormap = '32'
    ClientMessage = '33'
    Mapping = '34'
    VirtualEvent = '35'
    Activate = '36'
    Deactivate = '37'
    MouseWheel = '38'


_STATE_NAMES = ('Shift', 'Lock', 'Control',
                'Mod1', 'Mod2', 'Mod3', 'Mod4', 'Mod5',
                'Button1', 'Button2', 'Button3', 'Button4', 'Button5')


class Event:
    """Container for the fields of an event delivered to a bound callback.

    Fields that Tk does not report for a given event type hold '??'.
      serial - serial number of the event
      num - mouse button pressed (ButtonPress, ButtonRelease)
      focus - whether the window has the focus (Enter, Leave)
      height, width - new size of the widget (Configure, Expose)
      keycode - keycode of the pressed key (KeyPress, KeyRelease)
      state - modifier state as a number (most events)
      time - timestamp of the event
      x, y - pointer position relative to the widget
      x_root, y_root - pointer position relative to the screen
      char - the character typed (KeyPress, KeyRelease)
      send_event - whether the event was generated by a client (bool)
      keysym - key symbol name (KeyPress, KeyRelease)
      keysym_num - key symbol as a number (KeyPress, KeyRelease)
      type - the EventType of the event
      widget - the widget the event happened in
      delta - wheel movement (MouseWheel)
      user_data - data of a virtual event
      detail - detail field (Enter, Leave, FocusIn, FocusOut, ...)
    """

    def __repr__(self):
        fields = {k: v for k, v in self.__dict__.items() if v != '??'}
        parts = []
        if fields.get('send_event'):
            parts.append('send_event=True')
        state = fields.get('state')
        if isinstance(state, int):
            if state:
                names = []
                rest = state
                for i, n in enumerate(_STATE_NAMES):
                    if state & (1 << i):
                        names.append(n)
                        rest &= ~(1 << i)
                if rest or not names:
                    names.append(hex(rest))
                parts.append('state=' + '|'.join(names))
        elif state is not None:
            parts.append('state=%s' % (state,))
        if 'keysym' in fields:
            parts.append('keysym=%s' % fields['keysym'])
        if 'keycode' in fields:
            parts.append('keycode=%s' % fields['keycode'])
        if fields.get('char'):
            parts.append('char=%r' % fields['char'])
        if 'num' in fields:
            parts.append('num=%s' % fields['num'])
        if fields.get('delta'):
            parts.append('delta=%s' % fields['delta'])
        if 'focus' in fields:
            parts.append('focus=%s' % fields['focus'])
        for k in ('x', 'y', 'width', 'height'):
            if k in fields:
                parts.append('%s=%s' % (k, fields[k]))
        typ = getattr(self, 'type', '??')
        typ = getattr(typ, 'name', typ)
        return '<%s event%s>' % (typ, ''.join(' ' + p for p in parts))


# ---------------------------------------------------------------------------
# Default root

_support_default_root = True
_default_root = None


def NoDefaultRoot():
    """Stop tkinter from creating a root window implicitly.

    Afterwards every widget needs an explicit master; calling this also
    forgets the current default root.
    """
    global _support_default_root, _default_root
    _support_default_root = False
    _default_root = None
    del _default_root


def _get_default_root(what=None):
    if not _support_default_root:
        raise RuntimeError("No master specified and tkinter is "
                           "configured to not support default root")
    if globals().get('_default_root') is None:
        if what:
            raise RuntimeError("Too early to %s: no default root window" % what)
        root = Tk()
        assert _default_root is root
    return _default_root


def _get_temp_root():
    global _support_default_root
    if not _support_default_root:
        raise RuntimeError("No master specified and tkinter is "
                           "configured to not support default root")
    root = globals().get('_default_root')
    if root is None:
        assert _support_default_root
        _support_default_root = False
        try:
            root = Tk()
        finally:
            _support_default_root = True
        assert globals().get('_default_root') is None
        root.withdraw()
        root._temporary = True
    return root


def _destroy_temp_root(master):
    if getattr(master, '_temporary', False):
        try:
            master.destroy()
        except TclError:
            pass


def _tkerror(err):
    """Internal function: errors reported by Tcl's bgerror end up here."""
    pass


def _exit(code=0):
    """Internal function: implementation of the Tcl 'exit' command."""
    try:
        code = int(code)
    except ValueError:
        pass
    raise SystemExit(code)


# ---------------------------------------------------------------------------
# Variables

_varnum = 0


class Variable:
    """Base class for values held in a Tcl variable.

    Widgets that take a `variable` or `textvariable` option keep the
    variable's value and the widget state in sync.
    """
    _default = ""
    _root = None
    _tk = None
    _tclCommands = None

    def __init__(self, master=None, value=None, name=None):
        """Create a variable; *value* is its initial value and *name* the
        name of the Tcl variable (by default a new name 'PY_VARnum')."""
        if name is not None and not isinstance(name, str):
            raise TypeError("name must be a string")
        global _varnum
        if master is None:
            master = _get_default_root('create variable')
        self._root = master._root()
        self._tk = master.tk
        if name:
            self._name = name
        else:
            self._name = 'PY_VAR' + repr(_varnum)
            _varnum += 1
        if value is not None:
            self.initialize(value)
        elif not self._tk.getboolean(self._tk.call("info", "exists", self._name)):
            self.initialize(self._default)

    def __del__(self):
        """Unset the Tcl variable and delete callbacks registered for it."""
        if self._tk is None:
            return
        if self._tk.getboolean(self._tk.call("info", "exists", self._name)):
            self._tk.globalunsetvar(self._name)
        if self._tclCommands is not None:
            for name in self._tclCommands:
                self._tk.deletecommand(name)
            self._tclCommands = None

    def __str__(self):
        """Return the name of the Tcl variable."""
        return self._name

    def set(self, value):
        """Set the variable to *value*."""
        return self._tk.globalsetvar(self._name, value)

    initialize = set

    def get(self):
        """Return the value of the variable."""
        return self._tk.globalgetvar(self._name)

    def _register(self, callback):
        f = CallWrapper(callback, None, self._root).__call__
        cbname = repr(id(f))
        try:
            callback = callback.__func__
        except AttributeError:
            pass
        try:
            cbname = cbname + callback.__name__
        except AttributeError:
            pass
        self._tk.createcommand(cbname, f)
        if self._tclCommands is None:
            self._tclCommands = []
        self._tclCommands.append(cbname)
        return cbname

    def trace_add(self, mode, callback):
        """Call *callback* when the variable is accessed.

        *mode* is one of "array", "read", "write", "unset" or a sequence of
        them.  The callback gets the variable name, an index (or '') and the
        operation.  Returns the name of the callback (for trace_remove).
        """
        cbname = self._register(callback)
        self._tk.call('trace', 'add', 'variable', self._name, mode, (cbname,))
        return cbname

    def trace_remove(self, mode, cbname):
        """Remove the trace *cbname* for *mode* (as given to trace_add)."""
        self._tk.call('trace', 'remove', 'variable', self._name, mode, cbname)
        for m, ca in self.trace_info():
            if self._tk.splitlist(ca)[0] == cbname:
                break
        else:
            self._tk.deletecommand(cbname)
            try:
                self._tclCommands.remove(cbname)
            except ValueError:
                pass

    def trace_info(self):
        """Return a list of (modes, callback name) pairs for all traces."""
        splitlist = self._tk.splitlist
        return [(splitlist(m), b) for m, b in map(splitlist,
                splitlist(self._tk.call('trace', 'info', 'variable', self._name)))]

    def trace_variable(self, mode, callback):
        """Deprecated: use trace_add().  *mode* is "r", "w" or "u"."""
        import warnings
        warnings.warn(
                "trace_variable() is deprecated and not supported with Tcl 9; "
                "use trace_add() instead.",
                DeprecationWarning, stacklevel=2)
        cbname = self._register(callback)
        self._tk.call("trace", "variable", self._name, mode, cbname)
        return cbname

    trace = trace_variable

    def trace_vdelete(self, mode, cbname):
        """Deprecated: use trace_remove()."""
        import warnings
        warnings.warn(
                "trace_vdelete() is deprecated and not supported with Tcl 9; "
                "use trace_remove() instead.",
                DeprecationWarning, stacklevel=2)
        self._tk.call("trace", "vdelete", self._name, mode, cbname)
        cbname = self._tk.splitlist(cbname)[0]
        for m, ca in self.trace_info():
            if self._tk.splitlist(ca)[0] == cbname:
                break
        else:
            self._tk.deletecommand(cbname)
            try:
                self._tclCommands.remove(cbname)
            except ValueError:
                pass

    def trace_vinfo(self):
        """Deprecated: use trace_info()."""
        import warnings
        warnings.warn(
                "trace_vinfo() is deprecated and not supported with Tcl 9; "
                "use trace_info() instead.",
                DeprecationWarning, stacklevel=2)
        return [self._tk.splitlist(x) for x in self._tk.splitlist(
            self._tk.call("trace", "vinfo", self._name))]

    def __eq__(self, other):
        if not isinstance(other, Variable):
            return NotImplemented
        return (self.__class__ is other.__class__
                and self._name == other._name
                and self._root is other._root)

    def __hash__(self):
        return hash(self._name)


class StringVar(Variable):
    """A Tcl variable holding a string."""
    _default = ""

    def __init__(self, master=None, value=None, name=None):
        Variable.__init__(self, master, value, name)

    def get(self):
        """Return the value of the variable as a string."""
        value = self._tk.globalgetvar(self._name)
        if isinstance(value, str):
            return value
        return str(value)


class IntVar(Variable):
    """A Tcl variable holding an integer."""
    _default = 0

    def __init__(self, master=None, value=None, name=None):
        Variable.__init__(self, master, value, name)

    def get(self):
        """Return the value of the variable as an int."""
        value = self._tk.globalgetvar(self._name)
        try:
            return self._tk.getint(value)
        except (TypeError, TclError):
            return int(self._tk.getdouble(value))


class DoubleVar(Variable):
    """A Tcl variable holding a float."""
    _default = 0.0

    def __init__(self, master=None, value=None, name=None):
        Variable.__init__(self, master, value, name)

    def get(self):
        """Return the value of the variable as a float."""
        return self._tk.getdouble(self._tk.globalgetvar(self._name))


class BooleanVar(Variable):
    """A Tcl variable holding a boolean."""
    _default = False

    def __init__(self, master=None, value=None, name=None):
        Variable.__init__(self, master, value, name)

    def set(self, value):
        """Set the variable to *value* (any Tcl boolean)."""
        return self._tk.globalsetvar(self._name, self._tk.getboolean(value))

    initialize = set

    def get(self):
        """Return the value of the variable as a bool."""
        try:
            return self._tk.getboolean(self._tk.globalgetvar(self._name))
        except TclError:
            raise ValueError("invalid literal for getboolean()")


def mainloop(n=0):
    """Run the event loop of the default root window."""
    _get_default_root('run the main loop').tk.mainloop(n)


getint = int
getdouble = float


def getboolean(s):
    """Convert a Tcl boolean value to True or False."""
    try:
        return _get_default_root('use getboolean()').tk.getboolean(s)
    except TclError:
        raise ValueError("invalid literal for getboolean()")


# ---------------------------------------------------------------------------
# Misc: methods common to all widgets and the root window

class Misc:
    """Methods available on every widget and on Tk instances."""

    _last_child_ids = None
    _tclCommands = None

    def destroy(self):
        """Delete the Tcl commands created for this widget's callbacks."""
        if self._tclCommands is not None:
            for name in self._tclCommands:
                self.tk.deletecommand(name)
            self._tclCommands = None

    def deletecommand(self, name):
        """Delete the Tcl command *name* (created by _register)."""
        self.tk.deletecommand(name)
        try:
            self._tclCommands.remove(name)
        except (ValueError, AttributeError):
            pass

    def tk_strictMotif(self, boolean=None):
        """Get or set Tk's strict Motif look-and-feel flag."""
        return self.tk.getboolean(self.tk.call('set', 'tk_strictMotif', boolean))

    def tk_bisque(self):
        """Switch to the old bisque color scheme."""
        self.tk.call('tk_bisque')

    def tk_setPalette(self, *args, **kw):
        """Set a new color scheme for all widgets.

        A single color argument is the new background; otherwise give pairs
        of option names and values (background, foreground, activeBackground,
        ...).
        """
        self.tk.call(('tk_setPalette',) + _flatten(args) + _flatten(list(kw.items())))

    def wait_variable(self, name='PY_VAR'):
        """Process events until the variable *name* is written."""
        self.tk.call('tkwait', 'variable', name)

    waitvar = wait_variable

    def wait_window(self, window=None):
        """Process events until *window* (default self) is destroyed."""
        if window is None:
            window = self
        self.tk.call('tkwait', 'window', window._w)

    def wait_visibility(self, window=None):
        """Process events until *window* (default self) becomes visible."""
        if window is None:
            window = self
        self.tk.call('tkwait', 'visibility', window._w)

    def setvar(self, name='PY_VAR', value='1'):
        """Set the Tcl variable *name* to *value*."""
        self.tk.setvar(name, value)

    def getvar(self, name='PY_VAR'):
        """Return the value of the Tcl variable *name*."""
        return self.tk.getvar(name)

    def getint(self, s):
        try:
            return self.tk.getint(s)
        except TclError as exc:
            raise ValueError(str(exc))

    def getdouble(self, s):
        try:
            return self.tk.getdouble(s)
        except TclError as exc:
            raise ValueError(str(exc))

    def getboolean(self, s):
        """Convert a Tcl boolean value to True or False."""
        try:
            return self.tk.getboolean(s)
        except TclError:
            raise ValueError("invalid literal for getboolean()")

    # -- focus

    def focus_set(self):
        """Give the keyboard focus to this widget (when the application
        has the focus)."""
        self.tk.call('focus', self._w)

    focus = focus_set

    def focus_force(self):
        """Give the keyboard focus to this widget even if the application
        does not have the focus."""
        self.tk.call('focus', '-force', self._w)

    def focus_get(self):
        """Return the widget that has the focus in this application, or
        None."""
        name = self.tk.call('focus')
        if name == 'none' or not name:
            return None
        return self._nametowidget(name)

    def focus_displayof(self):
        """Return the widget that has the focus on this widget's display."""
        name = self.tk.call('focus', '-displayof', self._w)
        if name == 'none' or not name:
            return None
        return self._nametowidget(name)

    def focus_lastfor(self):
        """Return the widget that would get the focus if this widget's
        top-level window got it."""
        name = self.tk.call('focus', '-lastfor', self._w)
        if name == 'none' or not name:
            return None
        return self._nametowidget(name)

    def tk_focusFollowsMouse(self):
        """Make the focus follow the mouse pointer."""
        self.tk.call('tk_focusFollowsMouse')

    def tk_focusNext(self):
        """Return the next widget in the focus order (or None)."""
        name = self.tk.call('tk_focusNext', self._w)
        if not name:
            return None
        return self._nametowidget(name)

    def tk_focusPrev(self):
        """Return the previous widget in the focus order (or None)."""
        name = self.tk.call('tk_focusPrev', self._w)
        if not name:
            return None
        return self._nametowidget(name)

    # -- timers

    def after(self, ms, func=None, *args):
        """Call *func(*args)* once after *ms* milliseconds.

        Without *func*, just wait (processing no events).  Returns an
        identifier for after_cancel().
        """
        if func is None:
            self.tk.call('after', ms)
            return None
        def callit():
            try:
                func(*args)
            finally:
                try:
                    self.deletecommand(name)
                except TclError:
                    pass
        try:
            callit.__name__ = func.__name__
        except AttributeError:
            callit.__name__ = type(func).__name__
        name = self._register(callit)
        return self.tk.call('after', ms, name)

    def after_idle(self, func, *args):
        """Call *func(*args)* once when Tk has nothing else to do."""
        return self.after('idle', func, *args)

    def after_cancel(self, id):
        """Cancel a callback scheduled with after() or after_idle()."""
        if not id:
            raise ValueError('id must be a valid identifier returned from '
                             'after or after_idle')
        try:
            data = self.tk.call('after', 'info', id)
            script = self.tk.splitlist(data)[0]
            self.deletecommand(script)
        except TclError:
            pass
        self.tk.call('after', 'cancel', id)

    def after_info(self, id=None):
        """Return the identifiers of all pending after events, or (for
        *id*) a tuple (script, type) describing one of them."""
        return self.tk.splitlist(self.tk.call('after', 'info', id))

    def bell(self, displayof=0):
        """Ring the display's bell."""
        self.tk.call(('bell',) + self._displayof(displayof))

    # -- busy windows

    def tk_busy_cget(self, option):
        """Return the value of a busy-window configuration option."""
        return self.tk.call('tk', 'busy', 'cget', self._w, '-' + option)

    busy_cget = tk_busy_cget

    def tk_busy_configure(self, cnf=None, **kw):
        """Query or modify the busy-window options (currently -cursor)."""
        if kw:
            cnf = _cnfmerge((cnf, kw))
        elif cnf:
            cnf = _cnfmerge(cnf)
        if cnf is None:
            return self._getconfigure('tk', 'busy', 'configure', self._w)
        if isinstance(cnf, str):
            return self._getconfigure1('tk', 'busy', 'configure', self._w, '-' + cnf)
        self.tk.call('tk', 'busy', 'configure', self._w, *self._options(cnf))

    busy_config = busy_configure = tk_busy_config = tk_busy_configure

    def tk_busy_current(self, pattern=None):
        """Return the busy widgets whose path names match *pattern*."""
        return [self._nametowidget(x) for x in
                self.tk.splitlist(self.tk.call('tk', 'busy', 'current', pattern))]

    busy_current = tk_busy_current

    def tk_busy_forget(self):
        """Make the widget usable again after tk_busy_hold()."""
        self.tk.call('tk', 'busy', 'forget', self._w)

    busy_forget = tk_busy_forget

    def tk_busy_hold(self, **kw):
        """Make the widget and its descendants appear busy (ignore input)."""
        self.tk.call('tk', 'busy', 'hold', self._w, *self._options(kw))

    busy = busy_hold = tk_busy = tk_busy_hold

    def tk_busy_status(self):
        """Return True if the widget is busy."""
        return self.tk.getboolean(self.tk.call('tk', 'busy', 'status', self._w))

    busy_status = tk_busy_status

    # -- clipboard

    def clipboard_get(self, **kw):
        """Return the clipboard contents (options: displayof, type)."""
        if 'type' not in kw and self._windowingsystem == 'x11':
            try:
                kw['type'] = 'UTF8_STRING'
                return self.tk.call(('clipboard', 'get') + self._options(kw))
            except TclError:
                del kw['type']
        return self.tk.call(('clipboard', 'get') + self._options(kw))

    def clipboard_clear(self, **kw):
        """Empty the clipboard (option: displayof)."""
        if 'displayof' not in kw:
            kw['displayof'] = self._w
        self.tk.call(('clipboard', 'clear') + self._options(kw))

    def clipboard_append(self, string, **kw):
        """Append *string* to the clipboard (options: displayof, format,
        type)."""
        if 'displayof' not in kw:
            kw['displayof'] = self._w
        self.tk.call(('clipboard', 'append') + self._options(kw) + ('--', string))

    # -- grabs

    def grab_current(self):
        """Return the widget holding the grab in this application, or None."""
        name = self.tk.call('grab', 'current', self._w)
        if not name:
            return None
        return self._nametowidget(name)

    def grab_release(self):
        """Release this widget's grab."""
        self.tk.call('grab', 'release', self._w)

    def grab_set(self):
        """Direct all pointer events of the application to this widget."""
        self.tk.call('grab', 'set', self._w)

    def grab_set_global(self):
        """Direct all pointer events on the display to this widget."""
        self.tk.call('grab', 'set', '-global', self._w)

    def grab_status(self):
        """Return None, "local" or "global" for this widget's grab."""
        status = self.tk.call('grab', 'status', self._w)
        if status == 'none':
            status = None
        return status

    # -- option database

    def option_add(self, pattern, value, priority=None):
        """Add an entry to the option database."""
        self.tk.call('option', 'add', pattern, value, priority)

    def option_clear(self):
        """Clear the option database."""
        self.tk.call('option', 'clear')

    def option_get(self, name, className):
        """Return the option-database value for *name*/*className*."""
        return self.tk.call('option', 'get', self._w, name, className)

    def option_readfile(self, fileName, priority=None):
        """Read option-database entries from a file."""
        self.tk.call('option', 'readfile', fileName, priority)

    # -- selection

    def selection_clear(self, **kw):
        """Clear the selection (options: displayof, selection)."""
        if 'displayof' not in kw:
            kw['displayof'] = self._w
        self.tk.call(('selection', 'clear') + self._options(kw))

    def selection_get(self, **kw):
        """Return the contents of the selection (options: displayof,
        selection, type)."""
        if 'displayof' not in kw:
            kw['displayof'] = self._w
        if 'type' not in kw and self._windowingsystem == 'x11':
            try:
                kw['type'] = 'UTF8_STRING'
                return self.tk.call(('selection', 'get') + self._options(kw))
            except TclError:
                del kw['type']
        return self.tk.call(('selection', 'get') + self._options(kw))

    def selection_handle(self, command, **kw):
        """Make *command(offset, length)* supply the selection contents."""
        name = self._register(command)
        self.tk.call(('selection', 'handle') + self._options(kw) + (self._w, name))

    def selection_own(self, **kw):
        """Make this widget the owner of the selection."""
        self.tk.call(('selection', 'own') + self._options(kw) + (self._w,))

    def selection_own_get(self, **kw):
        """Return the widget owning the selection, or None."""
        if 'displayof' not in kw:
            kw['displayof'] = self._w
        name = self.tk.call(('selection', 'own') + self._options(kw))
        if not name:
            return None
        return self._nametowidget(name)

    def send(self, interp, cmd, *args):
        """Send a Tcl command to another Tk application."""
        return self.tk.call(('send', interp, cmd) + args)

    # -- stacking order

    def lower(self, belowThis=None):
        """Lower this widget in the stacking order."""
        self.tk.call('lower', self._w, belowThis)

    def tkraise(self, aboveThis=None):
        """Raise this widget in the stacking order."""
        self.tk.call('raise', self._w, aboveThis)

    lift = tkraise

    def info_patchlevel(self):
        """Return the Tcl patch level as a named tuple."""
        return _parse_version(self.tk.call('info', 'patchlevel'))

    # -- winfo

    def winfo_atom(self, name, displayof=0):
        """Return the integer identifier of the atom *name*."""
        args = ('winfo', 'atom') + self._displayof(displayof) + (name,)
        return self.tk.getint(self.tk.call(args))

    def winfo_atomname(self, id, displayof=0):
        """Return the name of the atom with identifier *id*."""
        args = ('winfo', 'atomname') + self._displayof(displayof) + (id,)
        return self.tk.call(args)

    def winfo_cells(self):
        """Return the number of cells in the widget's colormap."""
        return self.tk.getint(self.tk.call('winfo', 'cells', self._w))

    def winfo_children(self):
        """Return the child widgets of this widget (in stacking order)."""
        result = []
        for child in self.tk.splitlist(self.tk.call('winfo', 'children', self._w)):
            try:
                result.append(self._nametowidget(child))
            except KeyError:
                pass
        return result

    def winfo_class(self):
        """Return the Tk class name of the widget."""
        return self.tk.call('winfo', 'class', self._w)

    def winfo_colormapfull(self):
        """Return True if the widget's colormap is full."""
        return self.tk.getboolean(self.tk.call('winfo', 'colormapfull', self._w))

    def winfo_containing(self, rootX, rootY, displayof=0):
        """Return the widget at the screen position (rootX, rootY)."""
        args = ('winfo', 'containing') + self._displayof(displayof) + (rootX, rootY)
        name = self.tk.call(args)
        if not name:
            return None
        return self._nametowidget(name)

    def winfo_depth(self):
        """Return the number of bits per pixel."""
        return self.tk.getint(self.tk.call('winfo', 'depth', self._w))

    def winfo_exists(self):
        """Return true if the widget exists."""
        return self.tk.getint(self.tk.call('winfo', 'exists', self._w))

    def winfo_fpixels(self, number):
        """Return the number of pixels for a screen distance, as a float."""
        return self.tk.getdouble(self.tk.call('winfo', 'fpixels', self._w, number))

    def winfo_geometry(self):
        """Return the geometry of the widget as "widthxheight+x+y"."""
        return self.tk.call('winfo', 'geometry', self._w)

    def winfo_height(self):
        """Return the height of the widget."""
        return self.tk.getint(self.tk.call('winfo', 'height', self._w))

    def winfo_id(self):
        """Return the window identifier of the widget."""
        return int(self.tk.call('winfo', 'id', self._w), 0)

    def winfo_interps(self, displayof=0):
        """Return the names of the Tk applications on the display."""
        args = ('winfo', 'interps') + self._displayof(displayof)
        return self.tk.splitlist(self.tk.call(args))

    def winfo_ismapped(self):
        """Return true if the widget is mapped."""
        return self.tk.getint(self.tk.call('winfo', 'ismapped', self._w))

    def winfo_manager(self):
        """Return the geometry manager of the widget."""
        return self.tk.call('winfo', 'manager', self._w)

    def winfo_name(self):
        """Return the last component of the widget's path name."""
        return self.tk.call('winfo', 'name', self._w)

    def winfo_parent(self):
        """Return the path name of the widget's parent."""
        return self.tk.call('winfo', 'parent', self._w)

    def winfo_pathname(self, id, displayof=0):
        """Return the path name of the widget with window identifier *id*."""
        args = ('winfo', 'pathname') + self._displayof(displayof) + (id,)
        return self.tk.call(args)

    def winfo_pixels(self, number):
        """Return the rounded number of pixels for a screen distance."""
        return self.tk.getint(self.tk.call('winfo', 'pixels', self._w, number))

    def winfo_pointerx(self):
        """Return the pointer's x coordinate on the root window."""
        return self.tk.getint(self.tk.call('winfo', 'pointerx', self._w))

    def winfo_pointerxy(self):
        """Return the pointer's (x, y) on the root window."""
        return self._getints(self.tk.call('winfo', 'pointerxy', self._w))

    def winfo_pointery(self):
        """Return the pointer's y coordinate on the root window."""
        return self.tk.getint(self.tk.call('winfo', 'pointery', self._w))

    def winfo_reqheight(self):
        """Return the requested height of the widget."""
        return self.tk.getint(self.tk.call('winfo', 'reqheight', self._w))

    def winfo_reqwidth(self):
        """Return the requested width of the widget."""
        return self.tk.getint(self.tk.call('winfo', 'reqwidth', self._w))

    def winfo_rgb(self, color):
        """Return the (red, green, blue) components of *color* (0..65535)."""
        return self._getints(self.tk.call('winfo', 'rgb', self._w, color))

    def winfo_rootx(self):
        """Return the x coordinate of the widget's upper left corner on the
        root window."""
        return self.tk.getint(self.tk.call('winfo', 'rootx', self._w))

    def winfo_rooty(self):
        """Return the y coordinate of the widget's upper left corner on the
        root window."""
        return self.tk.getint(self.tk.call('winfo', 'rooty', self._w))

    def winfo_screen(self):
        """Return the screen name of the widget."""
        return self.tk.call('winfo', 'screen', self._w)

    def winfo_screencells(self):
        """Return the number of cells in the default colormap of the screen."""
        return self.tk.getint(self.tk.call('winfo', 'screencells', self._w))

    def winfo_screendepth(self):
        """Return the number of bits per pixel of the root window."""
        return self.tk.getint(self.tk.call('winfo', 'screendepth', self._w))

    def winfo_screenheight(self):
        """Return the screen height in pixels."""
        return self.tk.getint(self.tk.call('winfo', 'screenheight', self._w))

    def winfo_screenmmheight(self):
        """Return the screen height in millimeters."""
        return self.tk.getint(self.tk.call('winfo', 'screenmmheight', self._w))

    def winfo_screenmmwidth(self):
        """Return the screen width in millimeters."""
        return self.tk.getint(self.tk.call('winfo', 'screenmmwidth', self._w))

    def winfo_screenvisual(self):
        """Return the default visual class of the screen."""
        return self.tk.call('winfo', 'screenvisual', self._w)

    def winfo_screenwidth(self):
        """Return the screen width in pixels."""
        return self.tk.getint(self.tk.call('winfo', 'screenwidth', self._w))

    def winfo_server(self):
        """Return information about the display server."""
        return self.tk.call('winfo', 'server', self._w)

    def winfo_toplevel(self):
        """Return the top-level widget containing this widget."""
        return self._nametowidget(self.tk.call('winfo', 'toplevel', self._w))

    def winfo_viewable(self):
        """Return true if the widget and all its ancestors are mapped."""
        return self.tk.getint(self.tk.call('winfo', 'viewable', self._w))

    def winfo_visual(self):
        """Return the visual class of the widget."""
        return self.tk.call('winfo', 'visual', self._w)

    def winfo_visualid(self):
        """Return the X identifier of the widget's visual."""
        return self.tk.call('winfo', 'visualid', self._w)

    def winfo_visualsavailable(self, includeids=False):
        """Return the visuals available on the screen as (class, depth[,
        id]) tuples."""
        data = self.tk.call('winfo', 'visualsavailable', self._w,
                            'includeids' if includeids else None)
        data = [self.tk.splitlist(x) for x in self.tk.splitlist(data)]
        def conv(x):
            return (x[0], self.tk.getint(x[1])) + tuple(
                int(v, 0) if isinstance(v, str) else v for v in x[2:])
        return [conv(x) for x in data]

    def winfo_vrootheight(self):
        """Return the height of the virtual root window."""
        return self.tk.getint(self.tk.call('winfo', 'vrootheight', self._w))

    def winfo_vrootwidth(self):
        """Return the width of the virtual root window."""
        return self.tk.getint(self.tk.call('winfo', 'vrootwidth', self._w))

    def winfo_vrootx(self):
        """Return the x offset of the virtual root window."""
        return self.tk.getint(self.tk.call('winfo', 'vrootx', self._w))

    def winfo_vrooty(self):
        """Return the y offset of the virtual root window."""
        return self.tk.getint(self.tk.call('winfo', 'vrooty', self._w))

    def winfo_width(self):
        """Return the width of the widget."""
        return self.tk.getint(self.tk.call('winfo', 'width', self._w))

    def winfo_x(self):
        """Return the x coordinate of the widget in its parent."""
        return self.tk.getint(self.tk.call('winfo', 'x', self._w))

    def winfo_y(self):
        """Return the y coordinate of the widget in its parent."""
        return self.tk.getint(self.tk.call('winfo', 'y', self._w))

    # -- event processing

    def update(self):
        """Process all pending events and idle callbacks."""
        self.tk.call('update')

    def update_idletasks(self):
        """Process idle callbacks (redraws, geometry calculations) only."""
        self.tk.call('update', 'idletasks')

    def bindtags(self, tagList=None):
        """Get or set the binding tags of the widget."""
        if tagList is None:
            return self.tk.splitlist(self.tk.call('bindtags', self._w))
        self.tk.call('bindtags', self._w, tagList)

    def _bind(self, what, sequence, func, add, needcleanup=1):
        if isinstance(func, str):
            self.tk.call(what + (sequence, func))
        elif func:
            funcid = self._register(func, self._substitute, needcleanup)
            cmd = '%sif {"[%s %s]" == "break"} break\n' % (
                '+' if add else '', funcid, self._subst_format_str)
            self.tk.call(what + (sequence, cmd))
            return funcid
        elif sequence:
            return self.tk.call(what + (sequence,))
        else:
            return self.tk.splitlist(self.tk.call(what))

    def bind(self, sequence=None, func=None, add=None):
        """Bind *func* to the event *sequence* of this widget.

        *sequence* is an event pattern such as '<Button-1>', '<Key-a>',
        '<Control-x>' or a virtual event '<<Paste>>'.  *func* receives an
        Event instance; returning "break" stops further handlers.  With
        *add* the binding is added to the existing ones.  Returns the
        identifier for unbind().  Without *func*, return the bound script;
        without *sequence*, the list of bound sequences.
        """
        return self._bind(('bind', self._w), sequence, func, add)

    def _unbind(self, what, funcid=None):
        if funcid is None:
            self.tk.call(*what, '')
        else:
            lines = self.tk.call(what).split('\n')
            prefix = 'if {"[%s ' % funcid
            keep = [l for l in lines if not l.startswith(prefix)]
            if not keep[0:1] or not ''.join(keep).strip():
                keep = []
            self.tk.call(*what, '\n'.join(keep))
            self.deletecommand(funcid)

    def unbind(self, sequence, funcid=None):
        """Remove the bindings for *sequence* (only *funcid* if given)."""
        self._unbind(('bind', self._w, sequence), funcid)

    def bind_all(self, sequence=None, func=None, add=None):
        """Bind to an event at the application level ('all' tag)."""
        return self._root()._bind(('bind', 'all'), sequence, func, add, True)

    def unbind_all(self, sequence):
        """Remove the application-level bindings for *sequence*."""
        self._root()._unbind(('bind', 'all', sequence))

    def bind_class(self, className, sequence=None, func=None, add=None):
        """Bind to an event of all widgets with binding tag *className*."""
        return self._root()._bind(('bind', className), sequence, func, add, True)

    def unbind_class(self, className, sequence):
        """Remove the bindings for *sequence* on the tag *className*."""
        self._root()._unbind(('bind', className, sequence))

    def mainloop(self, n=0):
        """Run the event loop until all windows are destroyed (or quit())."""
        self.tk.mainloop(n)

    def quit(self):
        """Leave the event loop."""
        self.tk.quit()

    # -- conversions

    def _getints(self, string):
        if string:
            return tuple(map(self.tk.getint, self.tk.splitlist(string)))

    def _getdoubles(self, string):
        if string:
            return tuple(map(self.tk.getdouble, self.tk.splitlist(string)))

    def _getboolean(self, string):
        if string:
            return self.tk.getboolean(string)

    def _displayof(self, displayof):
        if displayof:
            return ('-displayof', displayof)
        if displayof is None:
            return ('-displayof', self._w)
        return ()

    @property
    def _windowingsystem(self):
        """The windowing system: 'x11', 'win32' or 'aqua'."""
        root = self._root()
        try:
            return root._windowingsystem_cached
        except AttributeError:
            ws = root._windowingsystem_cached = self.tk.call('tk', 'windowingsystem')
            return ws

    def _options(self, cnf, kw=None):
        if kw:
            cnf = _cnfmerge((cnf, kw))
        else:
            cnf = _cnfmerge(cnf)
        res = ()
        for k, v in cnf.items():
            if v is None:
                continue
            if k[-1] == '_':
                k = k[:-1]
            if callable(v):
                v = self._register(v)
            elif isinstance(v, (tuple, list)):
                parts = []
                for item in v:
                    if isinstance(item, int):
                        parts.append(str(item))
                    elif isinstance(item, str):
                        parts.append(_stringify(item))
                    else:
                        break
                else:
                    v = ' '.join(parts)
            res = res + ('-' + k, v)
        return res

    def nametowidget(self, name):
        """Return the widget object for the Tcl path name *name*."""
        name = str(name).split('.')
        w = self
        if not name[0]:
            w = w._root()
            name = name[1:]
        for n in name:
            if not n:
                break
            w = w.children[n]
        return w

    _nametowidget = nametowidget

    def _register(self, func, subst=None, needcleanup=1):
        """Create a Tcl command that calls *func*; return its name."""
        f = CallWrapper(func, subst, self).__call__
        name = repr(id(f))
        try:
            func = func.__func__
        except AttributeError:
            pass
        try:
            name = name + func.__name__
        except AttributeError:
            pass
        self.tk.createcommand(name, f)
        if needcleanup:
            if self._tclCommands is None:
                self._tclCommands = []
            self._tclCommands.append(name)
        return name

    register = _register

    def _root(self):
        """Return the root window (the Tk instance) of this widget."""
        w = self
        while w.master is not None:
            w = w.master
        return w

    _subst_format = ('%#', '%b', '%f', '%h', '%k',
                     '%s', '%t', '%w', '%x', '%y',
                     '%A', '%E', '%K', '%N', '%W', '%T', '%X', '%Y', '%D', '%d')
    _subst_format_str = " ".join(_subst_format)

    def _substitute(self, *args):
        """Build an Event from the %-substitutions of a binding script."""
        if len(args) != len(self._subst_format):
            return args
        getboolean = self.tk.getboolean
        getint = self.tk.getint

        def getint_event(s):
            try:
                return getint(s)
            except (ValueError, TclError):
                return s

        def getboolean_event(s):
            try:
                return getboolean(s)
            except (ValueError, TclError):
                return s

        (nsign, b, f, h, k, s, t, w, x, y,
         A, E, K, N, W, T, X, Y, D, d) = args
        e = Event()
        e.serial = getint_event(nsign)
        e.num = getint_event(b)
        e.height = getint_event(h)
        e.keycode = getint_event(k)
        e.state = getint_event(s)
        e.time = getint_event(t)
        e.width = getint_event(w)
        e.x = getint_event(x)
        e.y = getint_event(y)
        e.char = A
        try:
            e.send_event = getboolean(E)
        except (ValueError, TclError):
            e.send_event = False
        e.keysym = K
        e.keysym_num = getint_event(N)
        try:
            e.type = EventType(T)
        except ValueError:
            try:
                e.type = EventType(str(T))
            except ValueError:
                e.type = T
        if e.type in (EventType.Enter, EventType.Leave):
            e.focus = getboolean_event(f)
        try:
            e.widget = self._nametowidget(W)
        except KeyError:
            e.widget = W
        e.x_root = getint_event(X)
        e.y_root = getint_event(Y)
        try:
            e.delta = getint(D)
        except (ValueError, TclError):
            e.delta = 0
        if e.type == EventType.VirtualEvent:
            e.user_data = d
            e.detail = '??'
        else:
            e.user_data = '??'
            e.detail = d
        return (e,)

    def _report_exception(self):
        """Report the exception of a failing callback."""
        exc, val, tb = sys.exc_info()
        root = self._root()
        root.report_callback_exception(exc, val, tb)

    # -- configuration

    def _getconfigure(self, *args):
        """Return all configuration options as a dict of tuples."""
        cnf = {}
        for x in self.tk.splitlist(self.tk.call(_flatten(args))):
            x = self.tk.splitlist(x)
            cnf[x[0][1:]] = (x[0][1:],) + x[1:]
        return cnf

    def _getconfigure1(self, *args):
        x = self.tk.splitlist(self.tk.call(_flatten(args)))
        return (x[0][1:],) + x[1:]

    def _configure(self, cmd, cnf, kw):
        if kw:
            cnf = _cnfmerge((cnf, kw))
        elif cnf:
            cnf = _cnfmerge(cnf)
        if cnf is None:
            return self._getconfigure(_flatten((self._w, cmd)))
        if isinstance(cnf, str):
            return self._getconfigure1(_flatten((self._w, cmd, '-' + cnf)))
        self.tk.call(_flatten((self._w, cmd)) + self._options(cnf))

    def configure(self, cnf=None, **kw):
        """Configure resources of a widget.

        Options are given as keyword arguments or as a dict.  Without
        arguments, return all options with their values; with a single
        option name, return that option's description tuple.
        """
        return self._configure('configure', cnf, kw)

    config = configure

    def cget(self, key):
        """Return the value of the option *key*."""
        return self.tk.call(self._w, 'cget', '-' + key)

    __getitem__ = cget

    def __setitem__(self, key, value):
        self.configure({key: value})

    def keys(self):
        """Return the names of all configuration options."""
        splitlist = self.tk.splitlist
        return [splitlist(x)[0][1:] for x in
                splitlist(self.tk.call(self._w, 'configure'))]

    def __str__(self):
        """Return the Tcl path name of the widget."""
        return self._w

    def __repr__(self):
        return '<%s.%s object %s>' % (
            self.__class__.__module__, self.__class__.__qualname__, self._w)

    # Widgets are neither containers nor iterables
    __iter__ = None
    __contains__ = None

    # -- geometry managers: shared queries

    def pack_propagate(self, flag=['_noarg_']):
        """Get or set whether the pack manager resizes this widget to fit
        its packed content."""
        if flag == Misc._noarg_:
            return self._getboolean(self.tk.call('pack', 'propagate', self._w))
        self.tk.call('pack', 'propagate', self._w, flag)

    _noarg_ = ['_noarg_']
    propagate = pack_propagate

    def pack_slaves(self):
        """Return the widgets packed in this widget."""
        return [self._nametowidget(x) for x in
                self.tk.splitlist(self.tk.call('pack', 'slaves', self._w))]

    pack_content = slaves = content = pack_slaves

    def place_slaves(self):
        """Return the widgets placed in this widget."""
        return [self._nametowidget(x) for x in
                self.tk.splitlist(self.tk.call('place', 'slaves', self._w))]

    place_content = place_slaves

    def grid_anchor(self, anchor=None):
        """Get or set how the grid is placed when it is smaller than the
        widget."""
        self.tk.call('grid', 'anchor', self._w, anchor)

    anchor = grid_anchor

    def grid_bbox(self, column=None, row=None, col2=None, row2=None):
        """Return the bounding box (x, y, width, height) of the grid or of
        some of its cells."""
        args = ('grid', 'bbox', self._w)
        if column is not None and row is not None:
            args = args + (column, row)
        if col2 is not None and row2 is not None:
            args = args + (col2, row2)
        return self._getints(self.tk.call(*args)) or None

    bbox = grid_bbox

    def _gridconvvalue(self, value):
        if isinstance(value, (str, _tkinter.Tcl_Obj)):
            try:
                svalue = str(value)
                if not svalue:
                    return None
                if '.' in svalue:
                    return self.tk.getdouble(svalue)
                return self.tk.getint(svalue)
            except (ValueError, TclError):
                pass
        return value

    def _grid_configure(self, command, index, cnf, kw):
        if isinstance(cnf, str) and not kw:
            if cnf[-1:] == '_':
                cnf = cnf[:-1]
            if cnf[:1] != '-':
                cnf = '-' + cnf
            options = (cnf,)
        else:
            options = self._options(cnf, kw)
        if not options:
            return _splitdict(self.tk,
                              self.tk.call('grid', command, self._w, index),
                              conv=self._gridconvvalue)
        res = self.tk.call(('grid', command, self._w, index) + options)
        if len(options) == 1:
            return self._gridconvvalue(res)

    def grid_columnconfigure(self, index, cnf={}, **kw):
        """Configure column *index* of the grid (minsize, weight, uniform,
        pad)."""
        return self._grid_configure('columnconfigure', index, cnf, kw)

    columnconfigure = grid_columnconfigure

    def grid_location(self, x, y):
        """Return the (column, row) at pixel position (x, y)."""
        return self._getints(self.tk.call('grid', 'location', self._w, x, y)) or None

    def grid_propagate(self, flag=['_noarg_']):
        """Get or set whether the grid manager resizes this widget to fit
        its gridded content."""
        if flag == Misc._noarg_:
            return self._getboolean(self.tk.call('grid', 'propagate', self._w))
        self.tk.call('grid', 'propagate', self._w, flag)

    def grid_rowconfigure(self, index, cnf={}, **kw):
        """Configure row *index* of the grid (minsize, weight, uniform,
        pad)."""
        return self._grid_configure('rowconfigure', index, cnf, kw)

    rowconfigure = grid_rowconfigure

    def grid_size(self):
        """Return the number of (columns, rows) of the grid."""
        return self._getints(self.tk.call('grid', 'size', self._w)) or None

    size = grid_size

    def grid_slaves(self, row=None, column=None):
        """Return the widgets gridded in this widget (optionally only in
        one row and/or column)."""
        args = ()
        if row is not None:
            args = args + ('-row', row)
        if column is not None:
            args = args + ('-column', column)
        return [self._nametowidget(x) for x in
                self.tk.splitlist(self.tk.call(('grid', 'slaves', self._w) + args))]

    grid_content = grid_slaves

    # -- virtual events

    def event_add(self, virtual, *sequences):
        """Bind the virtual event *virtual* to each of *sequences*."""
        self.tk.call(('event', 'add', virtual) + sequences)

    def event_delete(self, virtual, *sequences):
        """Unbind the virtual event *virtual* from *sequences*."""
        self.tk.call(('event', 'delete', virtual) + sequences)

    def event_generate(self, sequence, **kw):
        """Generate the event *sequence* (options: when, x, y, data, ...)."""
        args = ('event', 'generate', self._w, sequence)
        for k, v in kw.items():
            args = args + ('-%s' % k, str(v))
        self.tk.call(args)

    def event_info(self, virtual=None):
        """Return the virtual events (or the sequences bound to *virtual*)."""
        return self.tk.splitlist(self.tk.call('event', 'info', virtual))

    # -- images

    def image_names(self):
        """Return the names of all existing images."""
        return self.tk.splitlist(self.tk.call('image', 'names'))

    def image_types(self):
        """Return the available image types."""
        return self.tk.splitlist(self.tk.call('image', 'types'))


class CallWrapper:
    """Calls a Python function from Tcl, converting its arguments with
    *subst* and reporting exceptions through the widget."""

    def __init__(self, func, subst, widget):
        self.func = func
        self.subst = subst
        self.widget = widget

    def __call__(self, *args):
        try:
            if self.subst:
                args = self.subst(*args)
            return self.func(*args)
        except SystemExit:
            raise
        except:
            self.widget._report_exception()


class XView:
    """Horizontal scrolling for widgets that support it."""

    def xview(self, *args):
        """Query or change the horizontal view (moveto/scroll ...)."""
        res = self.tk.call(self._w, 'xview', *args)
        if not args:
            return self._getdoubles(res)

    def xview_moveto(self, fraction):
        """Show the content starting at *fraction* of its width."""
        self.tk.call(self._w, 'xview', 'moveto', fraction)

    def xview_scroll(self, number, what):
        """Scroll by *number* units or pages (*what*)."""
        self.tk.call(self._w, 'xview', 'scroll', number, what)


class YView:
    """Vertical scrolling for widgets that support it."""

    def yview(self, *args):
        """Query or change the vertical view (moveto/scroll ...)."""
        res = self.tk.call(self._w, 'yview', *args)
        if not args:
            return self._getdoubles(res)

    def yview_moveto(self, fraction):
        """Show the content starting at *fraction* of its height."""
        self.tk.call(self._w, 'yview', 'moveto', fraction)

    def yview_scroll(self, number, what):
        """Scroll by *number* units or pages (*what*)."""
        self.tk.call(self._w, 'yview', 'scroll', number, what)


# ---------------------------------------------------------------------------
# Window manager interface

class Wm:
    """Window-manager methods of top-level windows."""

    def wm_aspect(self, minNumer=None, minDenom=None, maxNumer=None, maxDenom=None):
        """Get or set the allowed aspect ratio range of the window."""
        return self._getints(self.tk.call('wm', 'aspect', self._w,
                                          minNumer, minDenom, maxNumer, maxDenom))

    aspect = wm_aspect

    def wm_attributes(self, *args, return_python_dict=False, **kwargs):
        """Get or set platform-specific window attributes.

        Without arguments return all attributes (as a dict when
        *return_python_dict* is true); with one name return its value;
        otherwise set the attributes given as keywords or name/value pairs.
        """
        if not kwargs:
            if not args:
                res = self.tk.call('wm', 'attributes', self._w)
                if return_python_dict:
                    return _splitdict(self.tk, res)
                return self.tk.splitlist(res)
            if len(args) == 1 and args[0] is not None:
                option = args[0]
                if option[0] == '-':
                    option = option[1:]
                return self.tk.call('wm', 'attributes', self._w, '-' + option)
        options = []
        for k, v in kwargs.items():
            options.append('-' + k)
            options.append(v)
        return self.tk.call('wm', 'attributes', self._w, *args, *options)

    attributes = wm_attributes

    def wm_client(self, name=None):
        """Get or set the WM_CLIENT_MACHINE property."""
        return self.tk.call('wm', 'client', self._w, name)

    client = wm_client

    def wm_colormapwindows(self, *wlist):
        """Get or set the WM_COLORMAP_WINDOWS property."""
        if len(wlist) > 1:
            wlist = (wlist,)
        args = ('wm', 'colormapwindows', self._w) + wlist
        if wlist:
            self.tk.call(args)
        else:
            return [self._nametowidget(x) for x in self.tk.splitlist(self.tk.call(args))]

    colormapwindows = wm_colormapwindows

    def wm_command(self, value=None):
        """Get or set the WM_COMMAND property."""
        return self.tk.call('wm', 'command', self._w, value)

    command = wm_command

    def wm_deiconify(self):
        """Display the window (undo iconify/withdraw)."""
        return self.tk.call('wm', 'deiconify', self._w)

    deiconify = wm_deiconify

    def wm_focusmodel(self, model=None):
        """Get or set the focus model ("active" or "passive")."""
        return self.tk.call('wm', 'focusmodel', self._w, model)

    focusmodel = wm_focusmodel

    def wm_forget(self, window):
        """Unmanage *window* as a top-level window."""
        self.tk.call('wm', 'forget', window)

    forget = wm_forget

    def wm_frame(self):
        """Return the identifier of the outermost decorative frame."""
        return self.tk.call('wm', 'frame', self._w)

    frame = wm_frame

    def wm_geometry(self, newGeometry=None):
        """Get or set the geometry "widthxheight+x+y"."""
        return self.tk.call('wm', 'geometry', self._w, newGeometry)

    geometry = wm_geometry

    def wm_grid(self, baseWidth=None, baseHeight=None, widthInc=None, heightInc=None):
        """Get or set gridded geometry management."""
        return self._getints(self.tk.call('wm', 'grid', self._w,
                                          baseWidth, baseHeight, widthInc, heightInc))

    grid = wm_grid

    def wm_group(self, pathName=None):
        """Get or set the group leader of the window."""
        return self.tk.call('wm', 'group', self._w, pathName)

    group = wm_group

    def wm_iconbitmap(self, bitmap=None, default=None):
        """Get or set the icon bitmap (with *default*: for descendants too,
        Windows only)."""
        if default is not None:
            return self.tk.call('wm', 'iconbitmap', self._w, '-default', default)
        return self.tk.call('wm', 'iconbitmap', self._w, bitmap)

    iconbitmap = wm_iconbitmap

    def wm_iconify(self):
        """Turn the window into an icon."""
        return self.tk.call('wm', 'iconify', self._w)

    iconify = wm_iconify

    def wm_iconmask(self, bitmap=None):
        """Get or set the icon mask bitmap."""
        return self.tk.call('wm', 'iconmask', self._w, bitmap)

    iconmask = wm_iconmask

    def wm_iconname(self, newName=None):
        """Get or set the icon name."""
        return self.tk.call('wm', 'iconname', self._w, newName)

    iconname = wm_iconname

    def wm_iconphoto(self, default=False, *args):
        """Set the window icon from photo images (*default*: for future
        top-levels too)."""
        if default:
            self.tk.call('wm', 'iconphoto', self._w, '-default', *args)
        else:
            self.tk.call('wm', 'iconphoto', self._w, *args)

    iconphoto = wm_iconphoto

    def wm_iconposition(self, x=None, y=None):
        """Get or set the preferred icon position."""
        return self._getints(self.tk.call('wm', 'iconposition', self._w, x, y))

    iconposition = wm_iconposition

    def wm_iconwindow(self, pathName=None):
        """Get or set the window used as icon."""
        return self.tk.call('wm', 'iconwindow', self._w, pathName)

    iconwindow = wm_iconwindow

    def wm_manage(self, widget):
        """Make *widget* a stand-alone top-level window."""
        self.tk.call('wm', 'manage', widget)

    manage = wm_manage

    def wm_maxsize(self, width=None, height=None):
        """Get or set the maximal window size."""
        return self._getints(self.tk.call('wm', 'maxsize', self._w, width, height))

    maxsize = wm_maxsize

    def wm_minsize(self, width=None, height=None):
        """Get or set the minimal window size."""
        return self._getints(self.tk.call('wm', 'minsize', self._w, width, height))

    minsize = wm_minsize

    def wm_overrideredirect(self, boolean=None):
        """Get or set the override-redirect flag (no decorations)."""
        return self._getboolean(self.tk.call('wm', 'overrideredirect', self._w, boolean))

    overrideredirect = wm_overrideredirect

    def wm_positionfrom(self, who=None):
        """Get or set who chose the window position ("user"/"program")."""
        return self.tk.call('wm', 'positionfrom', self._w, who)

    positionfrom = wm_positionfrom

    def wm_protocol(self, name=None, func=None):
        """Get or set the handler of a window-manager protocol such as
        WM_DELETE_WINDOW."""
        if callable(func):
            command = self._register(func)
        else:
            command = func
        return self.tk.call('wm', 'protocol', self._w, name, command)

    protocol = wm_protocol

    def wm_resizable(self, width=None, height=None):
        """Get or set whether the window can be resized in each direction."""
        return self.tk.call('wm', 'resizable', self._w, width, height)

    resizable = wm_resizable

    def wm_sizefrom(self, who=None):
        """Get or set who chose the window size ("user"/"program")."""
        return self.tk.call('wm', 'sizefrom', self._w, who)

    sizefrom = wm_sizefrom

    def wm_state(self, newstate=None):
        """Get or set the state: normal, icon, iconic, withdrawn, zoomed."""
        return self.tk.call('wm', 'state', self._w, newstate)

    state = wm_state

    def wm_title(self, string=None):
        """Get or set the window title."""
        return self.tk.call('wm', 'title', self._w, string)

    title = wm_title

    def wm_transient(self, master=None):
        """Get or set the master of a transient (dialog) window."""
        return self.tk.call('wm', 'transient', self._w, master)

    transient = wm_transient

    def wm_withdraw(self):
        """Remove the window from the screen (deiconify shows it again)."""
        return self.tk.call('wm', 'withdraw', self._w)

    withdraw = wm_withdraw


# ---------------------------------------------------------------------------
# The root window

class Tk(Misc, Wm):
    """Top-level widget of Tk: owns a Tcl interpreter and is the main
    window of an application."""

    _w = '.'

    def __init__(self, screenName=None, baseName=None, className='Tk',
                 useTk=True, sync=False, use=None):
        """Create the main window with a new Tcl interpreter.

        *className* names the application class (used by the option
        database and window manager); *screenName* selects the display.
        With *useTk* false only Tcl is initialised (see Tcl()).
        """
        self.master = None
        self.children = {}
        self._tkloaded = False
        self.tk = None
        if baseName is None:
            import os
            baseName = os.path.basename(sys.argv[0])
            base, ext = os.path.splitext(baseName)
            if ext not in ('.py', '.pyc'):
                base = base + ext
            baseName = base
        interactive = False
        self.tk = _tkinter.create(screenName, baseName, className,
                                  interactive, wantobjects, useTk, sync, use)
        if _debug:
            self.tk.settrace(_print_command)
        if useTk:
            self._loadtk()
        if not sys.flags.ignore_environment:
            self.readprofile(baseName, className)

    def loadtk(self):
        """Load Tk into an interpreter created with useTk=False."""
        if not self._tkloaded:
            self.tk.loadtk()
            self._loadtk()

    def _loadtk(self):
        self._tkloaded = True
        global _default_root
        tk_version = self.tk.getvar('tk_version')
        if tk_version != _tkinter.TK_VERSION:
            raise RuntimeError("tk.h version (%s) doesn't match libtk.a version (%s)"
                               % (_tkinter.TK_VERSION, tk_version))
        tcl_version = str(self.tk.getvar('tcl_version'))
        if tcl_version != _tkinter.TCL_VERSION:
            raise RuntimeError("tcl.h version (%s) doesn't match libtcl.a version (%s)"
                               % (_tkinter.TCL_VERSION, tcl_version))
        if self._tclCommands is None:
            self._tclCommands = []
        self.tk.createcommand('tkerror', _tkerror)
        self.tk.createcommand('exit', _exit)
        self._tclCommands.append('tkerror')
        self._tclCommands.append('exit')
        if _support_default_root and globals().get('_default_root') is None:
            _default_root = self
        self.protocol("WM_DELETE_WINDOW", self.destroy)

    def destroy(self):
        """Destroy this and all descendant widgets; ends the application
        when this is the main window."""
        for c in list(self.children.values()):
            c.destroy()
        self.tk.call('destroy', self._w)
        Misc.destroy(self)
        global _default_root
        if _support_default_root and globals().get('_default_root') is self:
            _default_root = None

    def readprofile(self, baseName, className):
        """Run the profile scripts ~/.CLASSNAME.tcl, ~/.BASENAME.tcl (in
        Tcl) and ~/.CLASSNAME.py, ~/.BASENAME.py (in Python, with `self`
        bound to this window)."""
        import os
        home = os.environ.get('HOME') or os.curdir
        class_tcl = os.path.join(home, '.%s.tcl' % className)
        class_py = os.path.join(home, '.%s.py' % className)
        base_tcl = os.path.join(home, '.%s.tcl' % baseName)
        base_py = os.path.join(home, '.%s.py' % baseName)
        namespace = {'self': self}
        exec('from tkinter import *', namespace)

        def run_py(path):
            with open(path, 'rb') as f:
                source = f.read()
            exec(compile(source, path, 'exec'), namespace)

        if os.path.isfile(class_tcl):
            self.tk.call('source', class_tcl)
        if os.path.isfile(class_py):
            run_py(class_py)
        if os.path.isfile(base_tcl):
            self.tk.call('source', base_tcl)
        if os.path.isfile(base_py):
            run_py(base_py)

    def report_callback_exception(self, exc, val, tb):
        """Print an exception raised in a callback to sys.stderr.

        Applications may override this (e.g. to show a dialog)."""
        import traceback
        print("Exception in Tkinter callback", file=sys.stderr)
        sys.last_exc = val
        sys.last_type = exc
        sys.last_value = val
        sys.last_traceback = tb
        traceback.print_exception(exc, val, tb)

    def __getattr__(self, attr):
        "Delegate attribute access to the interpreter object"
        return getattr(self.tk, attr)


def _print_command(cmd, *, file=sys.stderr):
    # Debugging aid: show each command sent to Tcl.
    if isinstance(cmd, tuple):
        print(_join(cmd), file=file)
    else:
        print(cmd, file=file)


def Tcl(screenName=None, baseName=None, className='Tk', useTk=False):
    """Return a Tk instance with only the Tcl interpreter initialised."""
    return Tk(screenName, baseName, className, useTk)


# ---------------------------------------------------------------------------
# Geometry managers

class Pack:
    """The packer geometry manager."""

    def pack_configure(self, cnf={}, **kw):
        """Pack the widget in its parent.

        Options: after, before, anchor, expand, fill, in_, ipadx, ipady,
        padx, pady, side.
        """
        self.tk.call(('pack', 'configure', self._w) + self._options(cnf, kw))

    pack = configure = config = pack_configure

    def pack_forget(self):
        """Unmap the widget and remove it from the packing order."""
        self.tk.call('pack', 'forget', self._w)

    forget = pack_forget

    def pack_info(self):
        """Return the packing options of the widget as a dict."""
        d = _splitdict(self.tk, self.tk.call('pack', 'info', self._w))
        if 'in' in d:
            d['in'] = self.nametowidget(d['in'])
        return d

    info = pack_info
    propagate = pack_propagate = Misc.pack_propagate
    slaves = pack_slaves = Misc.pack_slaves
    content = pack_content = Misc.pack_content


class Place:
    """The placer geometry manager."""

    def place_configure(self, cnf={}, **kw):
        """Place the widget at an absolute or relative position.

        Options: in_, x, y, relx, rely, anchor, width, height, relwidth,
        relheight, bordermode.
        """
        self.tk.call(('place', 'configure', self._w) + self._options(cnf, kw))

    place = configure = config = place_configure

    def place_forget(self):
        """Unmap the widget."""
        self.tk.call('place', 'forget', self._w)

    forget = place_forget

    def place_info(self):
        """Return the placing options of the widget as a dict."""
        d = _splitdict(self.tk, self.tk.call('place', 'info', self._w))
        if 'in' in d:
            d['in'] = self.nametowidget(d['in'])
        return d

    info = place_info
    slaves = place_slaves = Misc.place_slaves
    content = place_content = Misc.place_content


class Grid:
    """The grid geometry manager."""

    def grid_configure(self, cnf={}, **kw):
        """Position the widget in a grid cell of its parent.

        Options: column, columnspan, in_, ipadx, ipady, padx, pady, row,
        rowspan, sticky.
        """
        self.tk.call(('grid', 'configure', self._w) + self._options(cnf, kw))

    grid = configure = config = grid_configure
    bbox = grid_bbox = Misc.grid_bbox
    columnconfigure = grid_columnconfigure = Misc.grid_columnconfigure

    def grid_forget(self):
        """Unmap the widget (its grid options are forgotten)."""
        self.tk.call('grid', 'forget', self._w)

    forget = grid_forget

    def grid_remove(self):
        """Unmap the widget but remember its grid options."""
        self.tk.call('grid', 'remove', self._w)

    def grid_info(self):
        """Return the grid options of the widget as a dict."""
        d = _splitdict(self.tk, self.tk.call('grid', 'info', self._w))
        if 'in' in d:
            d['in'] = self.nametowidget(d['in'])
        return d

    info = grid_info
    location = grid_location = Misc.grid_location
    propagate = grid_propagate = Misc.grid_propagate
    rowconfigure = grid_rowconfigure = Misc.grid_rowconfigure
    size = grid_size = Misc.grid_size
    slaves = grid_slaves = Misc.grid_slaves
    content = grid_content = Misc.grid_content


# ---------------------------------------------------------------------------
# Widgets

class BaseWidget(Misc):
    """Common base of all widgets (and Toplevel)."""

    def _setup(self, master, cnf):
        if master is None:
            master = _get_default_root()
        self.master = master
        self.tk = master.tk
        name = None
        if 'name' in cnf:
            name = cnf['name']
            del cnf['name']
        if not name:
            name = self.__class__.__name__.lower()
            if name[-1].isdigit():
                name += '!'
            if master._last_child_ids is None:
                master._last_child_ids = {}
            count = master._last_child_ids.get(name, 0) + 1
            master._last_child_ids[name] = count
            if count == 1:
                name = '!%s' % (name,)
            else:
                name = '!%s%d' % (name, count)
        self._name = name
        if master._w == '.':
            self._w = '.' + name
        else:
            self._w = master._w + '.' + name
        self.children = {}
        if self._name in master.children:
            master.children[self._name].destroy()
        master.children[self._name] = self

    def __init__(self, master, widgetName, cnf={}, kw={}, extra=()):
        """Create a widget of Tk class *widgetName* as child of *master*."""
        if kw:
            cnf = _cnfmerge((cnf, kw))
        self.widgetName = widgetName
        self._setup(master, cnf)
        if self._tclCommands is None:
            self._tclCommands = []
        classes = [(k, v) for k, v in cnf.items() if isinstance(k, type)]
        for k, v in classes:
            del cnf[k]
        self.tk.call((widgetName, self._w) + extra + self._options(cnf))
        for k, v in classes:
            k.configure(self, v)

    def destroy(self):
        """Destroy this widget and its descendants."""
        for c in list(self.children.values()):
            c.destroy()
        self.tk.call('destroy', self._w)
        if self._name in self.master.children:
            del self.master.children[self._name]
        Misc.destroy(self)

    def _do(self, name, args=()):
        return self.tk.call((self._w, name) + args)


class Widget(BaseWidget, Pack, Place, Grid):
    """A widget that can be managed by pack, place and grid."""
    pass


class Toplevel(BaseWidget, Wm):
    """A top-level window."""

    def __init__(self, master=None, cnf={}, **kw):
        """Create a top-level window.

        Options: background, bd, bg, borderwidth, class, colormap,
        container, cursor, height, highlightbackground, highlightcolor,
        highlightthickness, menu, relief, screen, takefocus, use, visual,
        width.
        """
        if kw:
            cnf = _cnfmerge((cnf, kw))
        extra = ()
        for wmkey in ['screen', 'class_', 'class', 'visual', 'colormap']:
            if wmkey in cnf:
                val = cnf[wmkey]
                if wmkey[-1] == '_':
                    opt = '-' + wmkey[:-1]
                else:
                    opt = '-' + wmkey
                extra = extra + (opt, val)
                del cnf[wmkey]
        BaseWidget.__init__(self, master, 'toplevel', cnf, {}, extra)
        root = self._root()
        self.iconname(root.iconname())
        self.title(root.title())
        self.protocol("WM_DELETE_WINDOW", self.destroy)


class Button(Widget):
    """Button widget."""

    def __init__(self, master=None, cnf={}, **kw):
        """Create a button.

        Standard options: activebackground, activeforeground, anchor,
        background, bitmap, borderwidth, cursor, disabledforeground, font,
        foreground, highlightbackground, highlightcolor, highlightthickness,
        image, justify, padx, pady, relief, repeatdelay, repeatinterval,
        takefocus, text, textvariable, underline, wraplength.
        Widget-specific options: command, compound, default, height,
        overrelief, state, width.
        """
        Widget.__init__(self, master, 'button', cnf, kw)

    def flash(self):
        """Flash the button (alternate active and normal colors)."""
        self.tk.call(self._w, 'flash')

    def invoke(self):
        """Invoke the command of the button and return its result."""
        return self.tk.call(self._w, 'invoke')


class Canvas(Widget, XView, YView):
    """Canvas widget for structured graphics."""

    def __init__(self, master=None, cnf={}, **kw):
        """Create a canvas.

        Options: background, bd, bg, borderwidth, closeenough, confine,
        cursor, height, highlightbackground, highlightcolor,
        highlightthickness, insertbackground, insertborderwidth,
        insertofftime, insertontime, insertwidth, offset, relief,
        scrollregion, selectbackground, selectborderwidth, selectforeground,
        state, takefocus, width, xscrollcommand, xscrollincrement,
        yscrollcommand, yscrollincrement.
        """
        Widget.__init__(self, master, 'canvas', cnf, kw)

    def addtag(self, *args):
        """Internal: add a tag to items (see the addtag_* methods)."""
        self.tk.call((self._w, 'addtag') + args)

    def addtag_above(self, newtag, tagOrId):
        """Add *newtag* to the item just above *tagOrId*."""
        self.addtag(newtag, 'above', tagOrId)

    def addtag_all(self, newtag):
        """Add *newtag* to all items."""
        self.addtag(newtag, 'all')

    def addtag_below(self, newtag, tagOrId):
        """Add *newtag* to the item just below *tagOrId*."""
        self.addtag(newtag, 'below', tagOrId)

    def addtag_closest(self, newtag, x, y, halo=None, start=None):
        """Add *newtag* to the item closest to (x, y)."""
        self.addtag(newtag, 'closest', x, y, halo, start)

    def addtag_enclosed(self, newtag, x1, y1, x2, y2):
        """Add *newtag* to the items completely inside the rectangle."""
        self.addtag(newtag, 'enclosed', x1, y1, x2, y2)

    def addtag_overlapping(self, newtag, x1, y1, x2, y2):
        """Add *newtag* to the items overlapping the rectangle."""
        self.addtag(newtag, 'overlapping', x1, y1, x2, y2)

    def addtag_withtag(self, newtag, tagOrId):
        """Add *newtag* to all items matching *tagOrId*."""
        self.addtag(newtag, 'withtag', tagOrId)

    def bbox(self, *args):
        """Return the bounding box (x1, y1, x2, y2) of the given items."""
        return self._getints(self.tk.call((self._w, 'bbox') + args)) or None

    def tag_unbind(self, tagOrId, sequence, funcid=None):
        """Remove a binding of items with *tagOrId*."""
        self._unbind((self._w, 'bind', tagOrId, sequence), funcid)

    def tag_bind(self, tagOrId, sequence=None, func=None, add=None):
        """Bind *func* to an event on the items with *tagOrId*."""
        return self._bind((self._w, 'bind', tagOrId), sequence, func, add)

    def canvasx(self, screenx, gridspacing=None):
        """Return the canvas x coordinate of window x coordinate *screenx*."""
        return self.tk.getdouble(self.tk.call(self._w, 'canvasx', screenx, gridspacing))

    def canvasy(self, screeny, gridspacing=None):
        """Return the canvas y coordinate of window y coordinate *screeny*."""
        return self.tk.getdouble(self.tk.call(self._w, 'canvasy', screeny, gridspacing))

    def coords(self, *args):
        """Get or set the coordinates of an item."""
        args = _flatten(args)
        return [self.tk.getdouble(x) for x in
                self.tk.splitlist(self.tk.call((self._w, 'coords') + args))]

    def _create(self, itemType, args, kw):
        args = _flatten(args)
        cnf = args[-1]
        if isinstance(cnf, (dict, tuple)):
            args = args[:-1]
        else:
            cnf = {}
        return self.tk.getint(self.tk.call(
            self._w, 'create', itemType, *(args + self._options(cnf, kw))))

    def create_arc(self, *args, **kw):
        """Create an arc item; return its id."""
        return self._create('arc', args, kw)

    def create_bitmap(self, *args, **kw):
        """Create a bitmap item; return its id."""
        return self._create('bitmap', args, kw)

    def create_image(self, *args, **kw):
        """Create an image item; return its id."""
        return self._create('image', args, kw)

    def create_line(self, *args, **kw):
        """Create a line item; return its id."""
        return self._create('line', args, kw)

    def create_oval(self, *args, **kw):
        """Create an oval item; return its id."""
        return self._create('oval', args, kw)

    def create_polygon(self, *args, **kw):
        """Create a polygon item; return its id."""
        return self._create('polygon', args, kw)

    def create_rectangle(self, *args, **kw):
        """Create a rectangle item; return its id."""
        return self._create('rectangle', args, kw)

    def create_text(self, *args, **kw):
        """Create a text item; return its id."""
        return self._create('text', args, kw)

    def create_window(self, *args, **kw):
        """Create a window item; return its id."""
        return self._create('window', args, kw)

    def dchars(self, *args):
        """Delete characters of text items."""
        self.tk.call((self._w, 'dchars') + args)

    def delete(self, *args):
        """Delete the given items."""
        self.tk.call((self._w, 'delete') + args)

    def dtag(self, *args):
        """Remove a tag from items."""
        self.tk.call((self._w, 'dtag') + args)

    def find(self, *args):
        """Internal: search items (see the find_* methods)."""
        return self._getints(self.tk.call((self._w, 'find') + args)) or ()

    def find_above(self, tagOrId):
        """Return the item just above *tagOrId*."""
        return self.find('above', tagOrId)

    def find_all(self):
        """Return all items."""
        return self.find('all')

    def find_below(self, tagOrId):
        """Return the item just below *tagOrId*."""
        return self.find('below', tagOrId)

    def find_closest(self, x, y, halo=None, start=None):
        """Return the item closest to (x, y)."""
        return self.find('closest', x, y, halo, start)

    def find_enclosed(self, x1, y1, x2, y2):
        """Return the items inside the rectangle."""
        return self.find('enclosed', x1, y1, x2, y2)

    def find_overlapping(self, x1, y1, x2, y2):
        """Return the items overlapping the rectangle."""
        return self.find('overlapping', x1, y1, x2, y2)

    def find_withtag(self, tagOrId):
        """Return the items matching *tagOrId*."""
        return self.find('withtag', tagOrId)

    def focus(self, *args):
        """Set the keyboard focus to an item (or return the focus item)."""
        return self.tk.call((self._w, 'focus') + args)

    def gettags(self, *args):
        """Return the tags of an item."""
        return self.tk.splitlist(self.tk.call((self._w, 'gettags') + args))

    def icursor(self, *args):
        """Set the insertion cursor of a text item."""
        self.tk.call((self._w, 'icursor') + args)

    def index(self, *args):
        """Return a numerical index in a text item."""
        return self.tk.getint(self.tk.call((self._w, 'index') + args))

    def insert(self, *args):
        """Insert text into text items."""
        self.tk.call((self._w, 'insert') + args)

    def itemcget(self, tagOrId, option):
        """Return the value of an item option."""
        return self.tk.call((self._w, 'itemcget') + (tagOrId, '-' + option))

    def itemconfigure(self, tagOrId, cnf=None, **kw):
        """Query or change the options of an item."""
        return self._configure(('itemconfigure', tagOrId), cnf, kw)

    itemconfig = itemconfigure

    def tag_lower(self, *args):
        """Lower items in the display list."""
        self.tk.call((self._w, 'lower') + args)

    lower = tag_lower

    def move(self, *args):
        """Move items by (dx, dy)."""
        self.tk.call((self._w, 'move') + args)

    def moveto(self, tagOrId, x='', y=''):
        """Move items so that their first coordinate is at (x, y)."""
        self.tk.call(self._w, 'moveto', tagOrId, x, y)

    def postscript(self, cnf={}, **kw):
        """Return a PostScript rendering of the canvas."""
        return self.tk.call((self._w, 'postscript') + self._options(cnf, kw))

    def tag_raise(self, *args):
        """Raise items in the display list."""
        self.tk.call((self._w, 'raise') + args)

    lift = tkraise = tag_raise

    def scale(self, *args):
        """Scale item coordinates."""
        self.tk.call((self._w, 'scale') + args)

    def scan_mark(self, x, y):
        """Remember the position for scan_dragto."""
        self.tk.call(self._w, 'scan', 'mark', x, y)

    def scan_dragto(self, x, y, gain=10):
        """Scroll relative to the position of scan_mark."""
        self.tk.call(self._w, 'scan', 'dragto', x, y, gain)

    def select_adjust(self, tagOrId, index):
        """Adjust the end of the selection near *index*."""
        self.tk.call(self._w, 'select', 'adjust', tagOrId, index)

    def select_clear(self):
        """Clear the selection."""
        self.tk.call(self._w, 'select', 'clear')

    def select_from(self, tagOrId, index):
        """Set the fixed end of the selection."""
        self.tk.call(self._w, 'select', 'from', tagOrId, index)

    def select_item(self):
        """Return the item holding the selection."""
        return self.tk.call(self._w, 'select', 'item') or None

    def select_to(self, tagOrId, index):
        """Set the variable end of the selection."""
        self.tk.call(self._w, 'select', 'to', tagOrId, index)

    def type(self, tagOrId):
        """Return the type of an item."""
        return self.tk.call(self._w, 'type', tagOrId) or None


_checkbutton_count = 0


class Checkbutton(Widget):
    """Checkbutton widget."""

    def __init__(self, master=None, cnf={}, **kw):
        """Create a checkbutton.

        Options: activebackground, activeforeground, anchor, background,
        bd, bg, bitmap, borderwidth, command, cursor, disabledforeground,
        fg, font, foreground, height, highlightbackground, highlightcolor,
        highlightthickness, image, indicatoron, justify, offvalue, onvalue,
        padx, pady, relief, selectcolor, selectimage, state, takefocus,
        text, textvariable, underline, variable, width, wraplength.
        """
        Widget.__init__(self, master, 'checkbutton', cnf, kw)

    def _setup(self, master, cnf):
        # Without an explicit variable Tk would use a global variable named
        # after the last path component, shared between same-named buttons.
        if not cnf.get('name'):
            global _checkbutton_count
            name = self.__class__.__name__.lower()
            _checkbutton_count += 1
            cnf['name'] = '!%s%d' % (name, _checkbutton_count)
        super()._setup(master, cnf)

    def deselect(self):
        """Turn the button off."""
        self.tk.call(self._w, 'deselect')

    def flash(self):
        """Flash the button."""
        self.tk.call(self._w, 'flash')

    def invoke(self):
        """Toggle the button and invoke its command."""
        return self.tk.call(self._w, 'invoke')

    def select(self):
        """Turn the button on."""
        self.tk.call(self._w, 'select')

    def toggle(self):
        """Toggle the button."""
        self.tk.call(self._w, 'toggle')


class Entry(Widget, XView):
    """Entry widget: a single line of editable text."""

    def __init__(self, master=None, cnf={}, **kw):
        """Create an entry.

        Options: background, bd, bg, borderwidth, cursor, exportselection,
        fg, font, foreground, highlightbackground, highlightcolor,
        highlightthickness, insertbackground, insertborderwidth,
        insertofftime, insertontime, insertwidth, invalidcommand,
        invcmd, justify, relief, selectbackground, selectborderwidth,
        selectforeground, show, state, takefocus, textvariable, validate,
        validatecommand, vcmd, width, xscrollcommand.
        """
        Widget.__init__(self, master, 'entry', cnf, kw)

    def delete(self, first, last=None):
        """Delete the text from *first* up to (not including) *last*."""
        self.tk.call(self._w, 'delete', first, last)

    def get(self):
        """Return the text."""
        return self.tk.call(self._w, 'get')

    def icursor(self, index):
        """Move the insertion cursor before *index*."""
        self.tk.call(self._w, 'icursor', index)

    def index(self, index):
        """Return the numerical index of *index*."""
        return self.tk.getint(self.tk.call(self._w, 'index', index))

    def insert(self, index, string):
        """Insert *string* at *index*."""
        self.tk.call(self._w, 'insert', index, string)

    def scan_mark(self, x):
        """Remember the position for scan_dragto."""
        self.tk.call(self._w, 'scan', 'mark', x)

    def scan_dragto(self, x):
        """Scroll relative to the position of scan_mark."""
        self.tk.call(self._w, 'scan', 'dragto', x)

    def selection_adjust(self, index):
        """Adjust the end of the selection near *index*."""
        self.tk.call(self._w, 'selection', 'adjust', index)

    select_adjust = selection_adjust

    def selection_clear(self):
        """Clear the selection."""
        self.tk.call(self._w, 'selection', 'clear')

    select_clear = selection_clear

    def selection_from(self, index):
        """Set the fixed end of the selection."""
        self.tk.call(self._w, 'selection', 'from', index)

    select_from = selection_from

    def selection_present(self):
        """Return True if some text is selected."""
        return self.tk.getboolean(self.tk.call(self._w, 'selection', 'present'))

    select_present = selection_present

    def selection_range(self, start, end):
        """Select the text from *start* to *end*."""
        self.tk.call(self._w, 'selection', 'range', start, end)

    select_range = selection_range

    def selection_to(self, index):
        """Set the variable end of the selection."""
        self.tk.call(self._w, 'selection', 'to', index)

    select_to = selection_to


class Frame(Widget):
    """Frame widget: a container."""

    def __init__(self, master=None, cnf={}, **kw):
        """Create a frame.

        Options: background, bd, bg, borderwidth, class, colormap,
        container, cursor, height, highlightbackground, highlightcolor,
        highlightthickness, relief, takefocus, visual, width.
        """
        cnf = _cnfmerge((cnf, kw))
        extra = ()
        if 'class_' in cnf:
            extra = ('-class', cnf['class_'])
            del cnf['class_']
        elif 'class' in cnf:
            extra = ('-class', cnf['class'])
            del cnf['class']
        Widget.__init__(self, master, 'frame', cnf, {}, extra)


class Label(Widget):
    """Label widget: displays text or an image."""

    def __init__(self, master=None, cnf={}, **kw):
        """Create a label.

        Standard options: activebackground, activeforeground, anchor,
        background, bitmap, borderwidth, cursor, disabledforeground, font,
        foreground, highlightbackground, highlightcolor,
        highlightthickness, image, justify, padx, pady, relief, takefocus,
        text, textvariable, underline, wraplength.
        Widget-specific options: height, state, width.
        """
        Widget.__init__(self, master, 'label', cnf, kw)


class Listbox(Widget, XView, YView):
    """Listbox widget: a list of selectable strings."""

    def __init__(self, master=None, cnf={}, **kw):
        """Create a listbox.

        Options: background, bd, bg, borderwidth, cursor, exportselection,
        fg, font, foreground, height, highlightbackground, highlightcolor,
        highlightthickness, relief, selectbackground, selectborderwidth,
        selectforeground, selectmode, setgrid, takefocus, width,
        xscrollcommand, yscrollcommand, listvariable.
        """
        Widget.__init__(self, master, 'listbox', cnf, kw)

    def activate(self, index):
        """Make the item at *index* the active one."""
        self.tk.call(self._w, 'activate', index)

    def bbox(self, index):
        """Return the bounding box (x, y, width, height) of an item."""
        return self._getints(self.tk.call(self._w, 'bbox', index)) or None

    def curselection(self):
        """Return the indices of the selected items."""
        return self._getints(self.tk.call(self._w, 'curselection')) or ()

    def delete(self, first, last=None):
        """Delete the items from *first* to *last* (inclusive)."""
        self.tk.call(self._w, 'delete', first, last)

    def get(self, first, last=None):
        """Return the item at *first* (or the items up to *last*)."""
        if last is not None:
            return self.tk.splitlist(self.tk.call(self._w, 'get', first, last))
        return self.tk.call(self._w, 'get', first)

    def index(self, index):
        """Return the numerical index of *index* (or None)."""
        i = self.tk.call(self._w, 'index', index)
        if i == 'none':
            return None
        return self.tk.getint(i)

    def insert(self, index, *elements):
        """Insert *elements* before *index*."""
        self.tk.call((self._w, 'insert', index) + elements)

    def nearest(self, y):
        """Return the index of the item nearest to window y coordinate *y*."""
        return self.tk.getint(self.tk.call(self._w, 'nearest', y))

    def scan_mark(self, x, y):
        """Remember the position for scan_dragto."""
        self.tk.call(self._w, 'scan', 'mark', x, y)

    def scan_dragto(self, x, y):
        """Scroll relative to the position of scan_mark."""
        self.tk.call(self._w, 'scan', 'dragto', x, y)

    def see(self, index):
        """Scroll so that the item at *index* is visible."""
        self.tk.call(self._w, 'see', index)

    def selection_anchor(self, index):
        """Set the selection anchor to *index*."""
        self.tk.call(self._w, 'selection', 'anchor', index)

    select_anchor = selection_anchor

    def selection_clear(self, first, last=None):
        """Deselect the items from *first* to *last*."""
        self.tk.call(self._w, 'selection', 'clear', first, last)

    select_clear = selection_clear

    def selection_includes(self, index):
        """Return True if the item at *index* is selected."""
        return self.tk.getboolean(self.tk.call(self._w, 'selection', 'includes', index))

    select_includes = selection_includes

    def selection_set(self, first, last=None):
        """Select the items from *first* to *last*."""
        self.tk.call(self._w, 'selection', 'set', first, last)

    select_set = selection_set

    def size(self):
        """Return the number of items."""
        return self.tk.getint(self.tk.call(self._w, 'size'))

    def itemcget(self, index, option):
        """Return the value of an item option."""
        return self.tk.call((self._w, 'itemcget') + (index, '-' + option))

    def itemconfigure(self, index, cnf=None, **kw):
        """Query or change the options (background, foreground,
        selectbackground, selectforeground) of an item."""
        return self._configure(('itemconfigure', index), cnf, kw)

    itemconfig = itemconfigure


class Menu(Widget):
    """Menu widget: menubars, pull-down and pop-up menus."""

    def __init__(self, master=None, cnf={}, **kw):
        """Create a menu.

        Options: activebackground, activeborderwidth, activeforeground,
        background, bd, bg, borderwidth, cursor, disabledforeground, fg,
        font, foreground, postcommand, relief, selectcolor, takefocus,
        tearoff, tearoffcommand, title, type.
        """
        Widget.__init__(self, master, 'menu', cnf, kw)

    def tk_popup(self, x, y, entry=""):
        """Post the menu at screen position (x, y)."""
        self.tk.call('tk_popup', self._w, x, y, entry)

    def activate(self, index):
        """Activate the entry at *index*."""
        self.tk.call(self._w, 'activate', index)

    def add(self, itemType, cnf={}, **kw):
        """Internal: add an entry of *itemType*."""
        self.tk.call((self._w, 'add', itemType) + self._options(cnf, kw))

    def add_cascade(self, cnf={}, **kw):
        """Add a cascade (submenu) entry."""
        self.add('cascade', cnf or kw)

    def add_checkbutton(self, cnf={}, **kw):
        """Add a checkbutton entry."""
        self.add('checkbutton', cnf or kw)

    def add_command(self, cnf={}, **kw):
        """Add a command entry."""
        self.add('command', cnf or kw)

    def add_radiobutton(self, cnf={}, **kw):
        """Add a radiobutton entry."""
        self.add('radiobutton', cnf or kw)

    def add_separator(self, cnf={}, **kw):
        """Add a separator."""
        self.add('separator', cnf or kw)

    def insert(self, index, itemType, cnf={}, **kw):
        """Internal: insert an entry of *itemType* before *index*."""
        self.tk.call((self._w, 'insert', index, itemType) + self._options(cnf, kw))

    def insert_cascade(self, index, cnf={}, **kw):
        """Insert a cascade entry before *index*."""
        self.insert(index, 'cascade', cnf or kw)

    def insert_checkbutton(self, index, cnf={}, **kw):
        """Insert a checkbutton entry before *index*."""
        self.insert(index, 'checkbutton', cnf or kw)

    def insert_command(self, index, cnf={}, **kw):
        """Insert a command entry before *index*."""
        self.insert(index, 'command', cnf or kw)

    def insert_radiobutton(self, index, cnf={}, **kw):
        """Insert a radiobutton entry before *index*."""
        self.insert(index, 'radiobutton', cnf or kw)

    def insert_separator(self, index, cnf={}, **kw):
        """Insert a separator before *index*."""
        self.insert(index, 'separator', cnf or kw)

    def delete(self, index1, index2=None):
        """Delete the entries from *index1* to *index2*."""
        if index2 is None:
            index2 = index1
        num_index1, num_index2 = self.index(index1), self.index(index2)
        if num_index1 is None or num_index2 is None:
            num_index1, num_index2 = 0, -1
        for i in range(num_index1, num_index2 + 1):
            if 'command' in self.entryconfig(i):
                c = str(self.entrycget(i, 'command'))
                if c:
                    self.deletecommand(c)
        self.tk.call(self._w, 'delete', index1, index2)

    def entrycget(self, index, option):
        """Return the value of an entry option."""
        return self.tk.call(self._w, 'entrycget', index, '-' + option)

    def entryconfigure(self, index, cnf=None, **kw):
        """Query or change the options of an entry."""
        return self._configure(('entryconfigure', index), cnf, kw)

    entryconfig = entryconfigure

    def index(self, index):
        """Return the numerical index of *index* (or None)."""
        i = self.tk.call(self._w, 'index', index)
        if i in ('', 'none'):
            return None
        return self.tk.getint(i)

    def invoke(self, index):
        """Invoke the entry at *index*."""
        return self.tk.call(self._w, 'invoke', index)

    def post(self, x, y):
        """Display the menu at screen position (x, y)."""
        self.tk.call(self._w, 'post', x, y)

    def type(self, index):
        """Return the type of the entry at *index*."""
        return self.tk.call(self._w, 'type', index)

    def unpost(self):
        """Unmap the menu."""
        self.tk.call(self._w, 'unpost')

    def xposition(self, index):
        """Return the x offset of the entry at *index*."""
        return self.tk.getint(self.tk.call(self._w, 'xposition', index))

    def yposition(self, index):
        """Return the y offset of the entry at *index*."""
        return self.tk.getint(self.tk.call(self._w, 'yposition', index))


class Menubutton(Widget):
    """Menubutton widget: a button that posts a menu."""

    def __init__(self, master=None, cnf={}, **kw):
        Widget.__init__(self, master, 'menubutton', cnf, kw)


class Message(Widget):
    """Message widget: multi-line text display."""

    def __init__(self, master=None, cnf={}, **kw):
        Widget.__init__(self, master, 'message', cnf, kw)


class Radiobutton(Widget):
    """Radiobutton widget: one of a group of exclusive choices."""

    def __init__(self, master=None, cnf={}, **kw):
        """Create a radiobutton.

        Options: activebackground, activeforeground, anchor, background,
        bd, bg, bitmap, borderwidth, command, cursor, disabledforeground,
        fg, font, foreground, height, highlightbackground, highlightcolor,
        highlightthickness, image, indicatoron, justify, padx, pady,
        relief, selectcolor, selectimage, state, takefocus, text,
        textvariable, underline, value, variable, width, wraplength.
        """
        Widget.__init__(self, master, 'radiobutton', cnf, kw)

    def deselect(self):
        """Deselect the button."""
        self.tk.call(self._w, 'deselect')

    def flash(self):
        """Flash the button."""
        self.tk.call(self._w, 'flash')

    def invoke(self):
        """Select the button and invoke its command."""
        return self.tk.call(self._w, 'invoke')

    def select(self):
        """Select the button."""
        self.tk.call(self._w, 'select')


class Scale(Widget):
    """Scale widget: a slider for a numeric value."""

    def __init__(self, master=None, cnf={}, **kw):
        """Create a scale.

        Options: activebackground, background, bigincrement, bd, bg,
        borderwidth, command, cursor, digits, fg, font, foreground, from_,
        highlightbackground, highlightcolor, highlightthickness, label,
        length, orient, relief, repeatdelay, repeatinterval, resolution,
        showvalue, sliderlength, sliderrelief, state, takefocus,
        tickinterval, to, troughcolor, variable, width.
        """
        Widget.__init__(self, master, 'scale', cnf, kw)

    def get(self):
        """Return the current value."""
        value = self.tk.call(self._w, 'get')
        try:
            return self.tk.getint(value)
        except (ValueError, TypeError, TclError):
            return self.tk.getdouble(value)

    def set(self, value):
        """Set the value."""
        self.tk.call(self._w, 'set', value)

    def coords(self, value=None):
        """Return the (x, y) of the point of the trough for *value*."""
        return self._getints(self.tk.call(self._w, 'coords', value))

    def identify(self, x, y):
        """Return the part of the scale at (x, y)."""
        return self.tk.call(self._w, 'identify', x, y)


class Scrollbar(Widget):
    """Scrollbar widget."""

    def __init__(self, master=None, cnf={}, **kw):
        """Create a scrollbar.

        Options: activebackground, activerelief, background, bd, bg,
        borderwidth, command, cursor, elementborderwidth,
        highlightbackground, highlightcolor, highlightthickness, jump,
        orient, relief, repeatdelay, repeatinterval, takefocus,
        troughcolor, width.
        """
        Widget.__init__(self, master, 'scrollbar', cnf, kw)

    def activate(self, index=None):
        """Get or set the active element (arrow1, slider, arrow2)."""
        return self.tk.call(self._w, 'activate', index) or None

    def delta(self, deltax, deltay):
        """Return the fraction change for a slider move of (deltax, deltay)."""
        return self.tk.getdouble(self.tk.call(self._w, 'delta', deltax, deltay))

    def fraction(self, x, y):
        """Return the fraction of the trough at (x, y)."""
        return self.tk.getdouble(self.tk.call(self._w, 'fraction', x, y))

    def identify(self, x, y):
        """Return the element at (x, y)."""
        return self.tk.call(self._w, 'identify', x, y)

    def get(self):
        """Return the current slider position (first, last)."""
        return self._getdoubles(self.tk.call(self._w, 'get'))

    def set(self, first, last):
        """Set the slider position."""
        self.tk.call(self._w, 'set', first, last)


class Text(Widget, XView, YView):
    """Text widget: multi-line formatted text."""

    def __init__(self, master=None, cnf={}, **kw):
        """Create a text widget.

        Standard options: background, borderwidth, cursor,
        exportselection, font, foreground, highlightbackground,
        highlightcolor, highlightthickness, insertbackground,
        insertborderwidth, insertofftime, insertontime, insertwidth, padx,
        pady, relief, selectbackground, selectborderwidth,
        selectforeground, setgrid, takefocus, xscrollcommand,
        yscrollcommand.
        Widget-specific options: autoseparators, height, maxundo, spacing1,
        spacing2, spacing3, state, tabs, undo, width, wrap.
        """
        Widget.__init__(self, master, 'text', cnf, kw)

    def bbox(self, index):
        """Return the bounding box (x, y, width, height) of a character."""
        return self._getints(self.tk.call(self._w, 'bbox', index)) or None

    def compare(self, index1, op, index2):
        """Compare two indices with the operator *op* ('<', '==', ...)."""
        return self.tk.getboolean(self.tk.call(self._w, 'compare', index1, op, index2))

    def count(self, index1, index2, *options, return_ints=False):
        """Count characters, lines, pixels, ... between two indices.

        *options* may be chars, displaychars, displayindices,
        displaylines, indices, lines, xpixels, ypixels, update.  With a
        single option the result is a number (or a 1-tuple unless
        *return_ints*), otherwise a tuple.
        """
        options = ['-%s' % arg for arg in options]
        res = self.tk.call(self._w, 'count', *options, index1, index2)
        if not isinstance(res, int):
            res = self._getints(res)
            if len(res) == 1:
                res, = res
        if not return_ints:
            if not res:
                res = None
            elif len(options) <= 1:
                res = (res,)
        return res

    def debug(self, boolean=None):
        """Get or set the internal consistency checks flag."""
        if boolean is None:
            return self.tk.getboolean(self.tk.call(self._w, 'debug'))
        self.tk.call(self._w, 'debug', boolean)

    def delete(self, index1, index2=None):
        """Delete the characters from *index1* to *index2*."""
        self.tk.call(self._w, 'delete', index1, index2)

    def dlineinfo(self, index):
        """Return (x, y, width, height, baseline) of the display line of
        *index*."""
        return self._getints(self.tk.call(self._w, 'dlineinfo', index))

    def dump(self, index1, index2=None, command=None, **kw):
        """Return the contents between two indices as (key, value, index)
        triples; *kw* flags select all, image, mark, tag, text, window.
        With *command*, call it for each triple instead."""
        args = []
        func_name = None
        result = None
        if not command:
            result = []
            def append_triple(key, value, index, result=result):
                result.append((key, value, index))
            command = append_triple
        try:
            if not isinstance(command, str):
                func_name = command = self._register(command)
            args += ["-command", command]
            for key in kw:
                if kw[key]:
                    args.append("-" + key)
            args.append(index1)
            if index2:
                args.append(index2)
            self.tk.call(self._w, "dump", *args)
            return result
        finally:
            if func_name:
                self.deletecommand(func_name)

    def edit(self, *args):
        """Internal: undo/redo commands (see the edit_* methods)."""
        return self.tk.call(self._w, 'edit', *args)

    def edit_modified(self, arg=None):
        """Get or set the modified flag."""
        return self.edit("modified", arg)

    def edit_redo(self):
        """Redo the last undone change."""
        return self.edit("redo")

    def edit_reset(self):
        """Clear the undo and redo stacks."""
        return self.edit("reset")

    def edit_separator(self):
        """Insert a separator in the undo stack."""
        return self.edit("separator")

    def edit_undo(self):
        """Undo the last change."""
        return self.edit("undo")

    def get(self, index1, index2=None):
        """Return the text from *index1* to *index2*."""
        return self.tk.call(self._w, 'get', index1, index2)

    def image_cget(self, index, option):
        """Return an option of an embedded image."""
        if option[:1] != "-":
            option = "-" + option
        if option[-1:] == "_":
            option = option[:-1]
        return self.tk.call(self._w, "image", "cget", index, option)

    def image_configure(self, index, cnf=None, **kw):
        """Query or change the options of an embedded image."""
        return self._configure(('image', 'configure', index), cnf, kw)

    def image_create(self, index, cnf={}, **kw):
        """Embed an image at *index*."""
        return self.tk.call(self._w, "image", "create", index, *self._options(cnf, kw))

    def image_names(self):
        """Return the names of the embedded images."""
        return self.tk.call(self._w, "image", "names")

    def index(self, index):
        """Return *index* in the form "line.char"."""
        return str(self.tk.call(self._w, 'index', index))

    def insert(self, index, chars, *args):
        """Insert *chars* at *index*; *args* alternate tag lists and more
        strings."""
        self.tk.call((self._w, 'insert', index, chars) + args)

    def mark_gravity(self, markName, direction=None):
        """Get or set the gravity ("left"/"right") of a mark."""
        return self.tk.call((self._w, 'mark', 'gravity', markName, direction))

    def mark_names(self):
        """Return all mark names."""
        return self.tk.splitlist(self.tk.call(self._w, 'mark', 'names'))

    def mark_set(self, markName, index):
        """Set the mark *markName* before the character at *index*."""
        self.tk.call(self._w, 'mark', 'set', markName, index)

    def mark_unset(self, *markNames):
        """Delete marks."""
        self.tk.call((self._w, 'mark', 'unset') + markNames)

    def mark_next(self, index):
        """Return the name of the next mark after *index*."""
        return self.tk.call(self._w, 'mark', 'next', index) or None

    def mark_previous(self, index):
        """Return the name of the previous mark before *index*."""
        return self.tk.call(self._w, 'mark', 'previous', index) or None

    def peer_create(self, newPathName, cnf={}, **kw):
        """Create a peer text widget sharing this widget's contents."""
        self.tk.call(self._w, 'peer', 'create', newPathName, *self._options(cnf, kw))

    def peer_names(self):
        """Return the peers of this widget."""
        return self.tk.splitlist(self.tk.call(self._w, 'peer', 'names'))

    def replace(self, index1, index2, chars, *args):
        """Replace the text from *index1* to *index2* by *chars*."""
        self.tk.call(self._w, 'replace', index1, index2, chars, *args)

    def scan_mark(self, x, y):
        """Remember the position for scan_dragto."""
        self.tk.call(self._w, 'scan', 'mark', x, y)

    def scan_dragto(self, x, y):
        """Scroll relative to the position of scan_mark."""
        self.tk.call(self._w, 'scan', 'dragto', x, y)

    def search(self, pattern, index, stopindex=None,
               forwards=None, backwards=None, exact=None,
               regexp=None, nocase=None, count=None, elide=None,
               *, nolinestop=None, strictlimits=None):
        """Search for *pattern* starting at *index*; return the index of
        the first match or ''."""
        args = [self._w, 'search']
        if forwards:
            args.append('-forwards')
        if backwards:
            args.append('-backwards')
        if exact:
            args.append('-exact')
        if regexp:
            args.append('-regexp')
        if nocase:
            args.append('-nocase')
        if elide:
            args.append('-elide')
        if nolinestop:
            args.append('-nolinestop')
        if strictlimits:
            args.append('-strictlimits')
        if count:
            args.append('-count')
            args.append(count)
        if pattern and pattern[0] == '-':
            args.append('--')
        args.append(pattern)
        args.append(index)
        if stopindex:
            args.append(stopindex)
        return str(self.tk.call(tuple(args)))

    def search_all(self, pattern, index, stopindex=None, *,
                   forwards=None, backwards=None, exact=None,
                   regexp=None, nocase=None, count=None,
                   elide=None, nolinestop=None, overlap=None,
                   strictlimits=None):
        """Return the indices of all matches of *pattern*."""
        args = [self._w, 'search', '-all']
        for flag, name in ((forwards, 'forwards'), (backwards, 'backwards'),
                           (exact, 'exact'), (regexp, 'regexp'),
                           (nocase, 'nocase'), (elide, 'elide'),
                           (nolinestop, 'nolinestop'), (overlap, 'overlap'),
                           (strictlimits, 'strictlimits')):
            if flag:
                args.append('-' + name)
        if count:
            args.append('-count')
            args.append(count)
        if pattern and pattern[0] == '-':
            args.append('--')
        args.append(pattern)
        args.append(index)
        if stopindex:
            args.append(stopindex)
        result = self.tk.call(tuple(args))
        return self.tk.splitlist(result)

    def see(self, index):
        """Scroll so that the character at *index* is visible."""
        self.tk.call(self._w, 'see', index)

    def tag_add(self, tagName, index1, *args):
        """Add *tagName* to the characters from *index1* (to the next
        index of *args*, ...)."""
        self.tk.call((self._w, 'tag', 'add', tagName, index1) + args)

    def tag_unbind(self, tagName, sequence, funcid=None):
        """Remove a binding of the tag *tagName*."""
        self._unbind((self._w, 'tag', 'bind', tagName, sequence), funcid)

    def tag_bind(self, tagName, sequence, func, add=None):
        """Bind *func* to an event on characters with the tag *tagName*."""
        return self._bind((self._w, 'tag', 'bind', tagName), sequence, func, add)

    def _tag_bind(self, tagName, sequence=None, func=None, add=None):
        return self._bind((self._w, 'tag', 'bind', tagName), sequence, func, add)

    def tag_cget(self, tagName, option):
        """Return the value of a tag option."""
        if option[:1] != '-':
            option = '-' + option
        if option[-1:] == '_':
            option = option[:-1]
        return self.tk.call(self._w, 'tag', 'cget', tagName, option)

    def tag_configure(self, tagName, cnf=None, **kw):
        """Query or change the options of a tag."""
        return self._configure(('tag', 'configure', tagName), cnf, kw)

    tag_config = tag_configure

    def tag_delete(self, *tagNames):
        """Delete tags (and their bindings)."""
        self.tk.call((self._w, 'tag', 'delete') + tagNames)

    def tag_lower(self, tagName, belowThis=None):
        """Lower the priority of a tag."""
        self.tk.call(self._w, 'tag', 'lower', tagName, belowThis)

    def tag_names(self, index=None):
        """Return all tags (or the tags of the character at *index*)."""
        return self.tk.splitlist(self.tk.call(self._w, 'tag', 'names', index))

    def tag_nextrange(self, tagName, index1, index2=None):
        """Return the next range (start, end) with the tag after *index1*."""
        return self.tk.splitlist(self.tk.call(
            self._w, 'tag', 'nextrange', tagName, index1, index2))

    def tag_prevrange(self, tagName, index1, index2=None):
        """Return the previous range with the tag before *index1*."""
        return self.tk.splitlist(self.tk.call(
            self._w, 'tag', 'prevrange', tagName, index1, index2))

    def tag_raise(self, tagName, aboveThis=None):
        """Raise the priority of a tag."""
        self.tk.call(self._w, 'tag', 'raise', tagName, aboveThis)

    def tag_ranges(self, tagName):
        """Return the ranges of the tag as a flat tuple of indices."""
        return self.tk.splitlist(self.tk.call(self._w, 'tag', 'ranges', tagName))

    def tag_remove(self, tagName, index1, index2=None):
        """Remove the tag from the characters between the indices."""
        self.tk.call(self._w, 'tag', 'remove', tagName, index1, index2)

    def window_cget(self, index, option):
        """Return an option of an embedded window."""
        if option[:1] != '-':
            option = '-' + option
        if option[-1:] == '_':
            option = option[:-1]
        return self.tk.call(self._w, 'window', 'cget', index, option)

    def window_configure(self, index, cnf=None, **kw):
        """Query or change the options of an embedded window."""
        return self._configure(('window', 'configure', index), cnf, kw)

    window_config = window_configure

    def window_create(self, index, cnf={}, **kw):
        """Embed a window at *index*."""
        self.tk.call((self._w, 'window', 'create', index) + self._options(cnf, kw))

    def window_names(self):
        """Return the embedded windows."""
        return self.tk.splitlist(self.tk.call(self._w, 'window', 'names'))

    def yview_pickplace(self, *what):
        """Obsolete: scroll so that the index is visible."""
        self.tk.call((self._w, 'yview', '-pickplace') + what)


class _setit:
    """Internal: menu-item callback of OptionMenu."""

    def __init__(self, var, value, callback=None):
        self.__value = value
        self.__var = var
        self.__callback = callback

    def __call__(self, *args):
        self.__var.set(self.__value)
        if self.__callback is not None:
            self.__callback(self.__value, *args)


class OptionMenu(Menubutton):
    """A menubutton offering a choice among values."""

    def __init__(self, master, variable, value, *values, **kwargs):
        """Create a menubutton showing *variable*, with a menu of *value*
        and *values*; the keyword *command* is called with the chosen
        value."""
        kw = {"borderwidth": 2, "textvariable": variable,
              "indicatoron": 1, "relief": RAISED, "anchor": "c",
              "highlightthickness": 2, "name": kwargs.pop('name', None)}
        Widget.__init__(self, master, "menubutton", kw)
        self.widgetName = 'tk_optionMenu'
        menu = self.__menu = Menu(self, name="menu", tearoff=0)
        self.menuname = menu._w
        callback = kwargs.get('command')
        if 'command' in kwargs:
            del kwargs['command']
        if kwargs:
            raise TclError('unknown option -' + next(iter(kwargs)))
        menu.add_command(label=value, command=_setit(variable, value, callback))
        for v in values:
            menu.add_command(label=v, command=_setit(variable, v, callback))
        self["menu"] = menu

    def __getitem__(self, name):
        if name == 'menu':
            return self.__menu
        return Widget.__getitem__(self, name)

    def destroy(self):
        """Destroy the widget and its menu."""
        Menubutton.destroy(self)
        self.__menu = None


class Image:
    """Base class for images."""

    _last_id = 0

    def __init__(self, imgtype, name=None, cnf={}, master=None, **kw):
        self.name = None
        if master is None:
            master = _get_default_root('create image')
        self.tk = getattr(master, 'tk', master)
        if not name:
            Image._last_id += 1
            name = "pyimage%r" % (Image._last_id,)
        if kw and cnf:
            cnf = _cnfmerge((cnf, kw))
        elif kw:
            cnf = kw
        options = ()
        for k, v in cnf.items():
            if callable(v):
                v = self._register(v)
            options = options + ('-' + k, v)
        self.tk.call(('image', 'create', imgtype, name) + options)
        self.name = name

    def __str__(self):
        return self.name

    def __del__(self):
        if self.name:
            try:
                self.tk.call('image', 'delete', self.name)
            except TclError:
                pass

    def __setitem__(self, key, value):
        self.tk.call(self.name, 'configure', '-' + key, value)

    def __getitem__(self, key):
        return self.tk.call(self.name, 'configure', '-' + key)

    def configure(self, **kw):
        """Change image options."""
        res = ()
        for k, v in _cnfmerge(kw).items():
            if v is not None:
                if k[-1] == '_':
                    k = k[:-1]
                if callable(v):
                    v = self._register(v)
                res = res + ('-' + k, v)
        self.tk.call((self.name, 'config') + res)

    config = configure

    def height(self):
        """Return the height of the image."""
        return self.tk.getint(self.tk.call('image', 'height', self.name))

    def type(self):
        """Return the type of the image ("photo" or "bitmap")."""
        return self.tk.call('image', 'type', self.name)

    def width(self):
        """Return the width of the image."""
        return self.tk.getint(self.tk.call('image', 'width', self.name))


class PhotoImage(Image):
    """Full-color image (GIF, PNG, PPM/PGM)."""

    def __init__(self, name=None, cnf={}, master=None, **kw):
        """Create a photo image.

        Options: data, format, file, gamma, height, palette, width.
        """
        Image.__init__(self, 'photo', name, cnf, master, **kw)

    def blank(self):
        """Make the image fully transparent."""
        self.tk.call(self.name, 'blank')

    def cget(self, option):
        """Return the value of an image option."""
        return self.tk.call(self.name, 'cget', '-' + option)

    def __getitem__(self, key):
        return self.tk.call(self.name, 'cget', '-' + key)

    def copy(self, *, from_coords=None, zoom=None, subsample=None):
        """Return a new image that is a (scaled) copy of this one."""
        destImage = PhotoImage(master=self.tk)
        destImage.copy_replace(self, from_coords=from_coords,
                               zoom=zoom, subsample=subsample)
        return destImage

    def zoom(self, x, y='', *, from_coords=None):
        """Return a new image magnified x (and y) times."""
        if y == '':
            y = x
        return self.copy(zoom=(x, y), from_coords=from_coords)

    def subsample(self, x, y='', *, from_coords=None):
        """Return a new image using only every x-th (y-th) pixel."""
        if y == '':
            y = x
        return self.copy(subsample=(x, y), from_coords=from_coords)

    def copy_replace(self, sourceImage, *, from_coords=None, to=None,
                     shrink=False, zoom=None, subsample=None,
                     compositingrule=None):
        """Copy (a region of) *sourceImage* into this image."""
        options = []
        if from_coords is not None:
            options.extend(('-from', *from_coords))
        if to is not None:
            options.extend(('-to', *to))
        if shrink:
            options.append('-shrink')
        if zoom is not None:
            if not isinstance(zoom, (tuple, list)):
                zoom = (zoom,)
            options.extend(('-zoom', *zoom))
        if subsample is not None:
            if not isinstance(subsample, (tuple, list)):
                subsample = (subsample,)
            options.extend(('-subsample', *subsample))
        if compositingrule:
            options.extend(('-compositingrule', compositingrule))
        self.tk.call(self.name, 'copy', sourceImage, *options)

    def get(self, x, y):
        """Return the (red, green, blue) color of pixel (x, y)."""
        return self._getints(self.tk.call(self.name, 'get', x, y))

    def _getints(self, string):
        if string:
            return tuple(map(self.tk.getint, self.tk.splitlist(string)))

    def put(self, data, to=None):
        """Put row-formatted color data into the image at *to*."""
        args = (self.name, 'put', data)
        if to:
            if to[0] == '-to':
                to = to[1:]
            args = args + ('-to',) + tuple(to)
        self.tk.call(args)

    def read(self, filename, format=None, *, from_coords=None, to=None,
             shrink=False):
        """Read image data from a file into the image."""
        options = ()
        if format is not None:
            options += ('-format', format)
        if from_coords is not None:
            options += ('-from', *from_coords)
        if shrink:
            options += ('-shrink',)
        if to is not None:
            options += ('-to', *to)
        self.tk.call(self.name, 'read', filename, *options)

    def write(self, filename, format=None, from_coords=None, *,
              background=None, grayscale=False):
        """Write the image to a file."""
        options = ()
        if format is not None:
            options += ('-format', format)
        if from_coords is not None:
            options += ('-from', *from_coords)
        if grayscale:
            options += ('-grayscale',)
        if background is not None:
            options += ('-background', background)
        self.tk.call(self.name, 'write', filename, *options)

    def data(self, format=None, *, from_coords=None, background=None,
             grayscale=False):
        """Return the image data (in *format*, default a list of rows)."""
        options = ()
        if format is not None:
            options += ('-format', format)
        if from_coords is not None:
            options += ('-from', *from_coords)
        if grayscale:
            options += ('-grayscale',)
        if background is not None:
            options += ('-background', background)
        data = self.tk.call(self.name, 'data', *options)
        if isinstance(data, str):
            if format is None:
                data = self.tk.splitlist(data)
        return data

    def transparency_get(self, x, y):
        """Return True if pixel (x, y) is transparent."""
        return self.tk.getboolean(self.tk.call(
            self.name, 'transparency', 'get', x, y))

    def transparency_set(self, x, y, boolean):
        """Set the transparency of pixel (x, y)."""
        self.tk.call(self.name, 'transparency', 'set', x, y, boolean)


class BitmapImage(Image):
    """Two-color image (XBM)."""

    def __init__(self, name=None, cnf={}, master=None, **kw):
        """Create a bitmap image.

        Options: background, data, file, foreground, maskdata, maskfile.
        """
        Image.__init__(self, 'bitmap', name, cnf, master, **kw)


def image_names():
    """Return the names of all existing images."""
    tk = _get_default_root('use image_names()').tk
    return tk.splitlist(tk.call('image', 'names'))


def image_types():
    """Return the available image types."""
    tk = _get_default_root('use image_types()').tk
    return tk.splitlist(tk.call('image', 'types'))


class Spinbox(Widget, XView):
    """Spinbox widget: an entry with up/down arrows."""

    def __init__(self, master=None, cnf={}, **kw):
        """Create a spinbox.

        Standard options: activebackground, background, borderwidth,
        cursor, exportselection, font, foreground, highlightbackground,
        highlightcolor, highlightthickness, insertbackground,
        insertborderwidth, insertofftime, insertontime, insertwidth,
        justify, relief, repeatdelay, repeatinterval, selectbackground,
        selectborderwidth, selectforeground, takefocus, textvariable,
        xscrollcommand.
        Widget-specific options: buttonbackground, buttoncursor,
        buttondownrelief, buttonuprelief, command, disabledbackground,
        disabledforeground, format, from, invalidcommand, increment,
        readonlybackground, state, to, validate, validatecommand, values,
        width, wrap.
        """
        Widget.__init__(self, master, 'spinbox', cnf, kw)

    def bbox(self, index):
        """Return the bounding box of the character at *index*."""
        return self._getints(self.tk.call(self._w, 'bbox', index)) or None

    def delete(self, first, last=None):
        """Delete the characters from *first* to *last*."""
        return self.tk.call(self._w, 'delete', first, last)

    def get(self):
        """Return the text."""
        return self.tk.call(self._w, 'get')

    def icursor(self, index):
        """Move the insertion cursor before *index*."""
        return self.tk.call(self._w, 'icursor', index)

    def identify(self, x, y):
        """Return the element at (x, y): none, buttondown, buttonup, entry."""
        return self.tk.call(self._w, 'identify', x, y)

    def index(self, index):
        """Return the numerical index of *index*."""
        return self.tk.getint(self.tk.call(self._w, 'index', index))

    def insert(self, index, s):
        """Insert *s* at *index*."""
        return self.tk.call(self._w, 'insert', index, s)

    def invoke(self, element):
        """Invoke a button element (buttonup or buttondown)."""
        return self.tk.call(self._w, 'invoke', element)

    def scan(self, *args):
        """Internal: scan commands."""
        return self._getints(self.tk.call((self._w, 'scan') + args)) or ()

    def scan_mark(self, x):
        """Remember the position for scan_dragto."""
        return self.scan("mark", x)

    def scan_dragto(self, x):
        """Scroll relative to the position of scan_mark."""
        return self.scan("dragto", x)

    def selection(self, *args):
        """Internal: selection commands."""
        return self._getints(self.tk.call((self._w, 'selection') + args)) or ()

    def selection_adjust(self, index):
        """Adjust the end of the selection near *index*."""
        return self.selection("adjust", index)

    def selection_clear(self):
        """Clear the selection."""
        return self.selection("clear")

    def selection_element(self, element=None):
        """Get or set the selected element."""
        return self.tk.call(self._w, 'selection', 'element', element)

    def selection_from(self, index):
        """Set the fixed end of the selection."""
        self.selection('from', index)

    def selection_present(self):
        """Return True if some text is selected."""
        return self.tk.getboolean(self.tk.call(self._w, 'selection', 'present'))

    def selection_range(self, start, end):
        """Select the characters from *start* to *end*."""
        self.selection('range', start, end)

    def selection_to(self, index):
        """Set the variable end of the selection."""
        self.selection('to', index)


class LabelFrame(Widget):
    """LabelFrame widget: a frame with a border and a title."""

    def __init__(self, master=None, cnf={}, **kw):
        """Create a labelframe.

        Standard options: borderwidth, cursor, font, foreground,
        highlightbackground, highlightcolor, highlightthickness, padx,
        pady, relief, takefocus, text.
        Widget-specific options: background, class, colormap, container,
        height, labelanchor, labelwidget, visual, width.
        """
        Widget.__init__(self, master, 'labelframe', cnf, kw)


class PanedWindow(Widget):
    """PanedWindow widget: panes separated by draggable sashes."""

    def __init__(self, master=None, cnf={}, **kw):
        """Create a panedwindow.

        Standard options: background, borderwidth, cursor, height, orient,
        relief, width.
        Widget-specific options: handlepad, handlesize, opaqueresize,
        sashcursor, sashpad, sashrelief, sashwidth, showhandle.
        """
        Widget.__init__(self, master, 'panedwindow', cnf, kw)

    def add(self, child, **kw):
        """Add *child* as a new pane."""
        self.tk.call((self._w, 'add', child) + self._options(kw))

    def remove(self, child):
        """Remove the pane containing *child*."""
        self.tk.call(self._w, 'forget', child)

    forget = remove

    def identify(self, x, y):
        """Return the sash or handle at (x, y)."""
        return self.tk.call(self._w, 'identify', x, y)

    def proxy(self, *args):
        """Internal: proxy commands."""
        return self._getints(self.tk.call((self._w, 'proxy') + args)) or ()

    def proxy_coord(self):
        """Return the position of the sash proxy."""
        return self.proxy("coord")

    def proxy_forget(self):
        """Remove the sash proxy."""
        return self.proxy("forget")

    def proxy_place(self, x, y):
        """Place the sash proxy at (x, y)."""
        return self.proxy("place", x, y)

    def sash(self, *args):
        """Internal: sash commands."""
        return self._getints(self.tk.call((self._w, 'sash') + args)) or ()

    def sash_coord(self, index):
        """Return the position of sash *index*."""
        return self.sash("coord", index)

    def sash_mark(self, index):
        """Remember the position for dragging sash *index*."""
        return self.sash("mark", index)

    def sash_place(self, index, x, y):
        """Move sash *index* to (x, y)."""
        return self.sash("place", index, x, y)

    def panecget(self, child, option):
        """Return a pane option of *child*."""
        return self.tk.call((self._w, 'panecget') + (child, '-' + option))

    def paneconfigure(self, tagOrId, cnf=None, **kw):
        """Query or change the pane options of a child (after, before,
        height, minsize, padx, pady, sticky, width, ...)."""
        if cnf is None and not kw:
            return self._getconfigure(self._w, 'paneconfigure', tagOrId)
        if isinstance(cnf, str) and not kw:
            return self._getconfigure1(self._w, 'paneconfigure', tagOrId, '-' + cnf)
        self.tk.call((self._w, 'paneconfigure', tagOrId) + self._options(cnf, kw))

    paneconfig = paneconfigure

    def panes(self):
        """Return the child widgets of the panes."""
        return self.tk.splitlist(self.tk.call(self._w, 'panes'))


def _test():
    root = Tk()
    text = "This is Tcl/Tk %s" % root.globalgetvar('tk_patchLevel')
    text += "\nThis should be a cedilla: \xe7"
    label = Label(root, text=text)
    label.pack()
    test = Button(root, text="Click me!",
                  command=lambda root=root: root.test.configure(
                      text="[%s]" % root.test['text']))
    test.pack()
    root.test = test
    quit = Button(root, text="QUIT", command=root.destroy)
    quit.pack()
    root.iconify()
    root.update()
    root.deiconify()
    root.mainloop()


__all__ = [name for name, obj in globals().items()
           if not name.startswith('_') and not isinstance(obj, types.ModuleType)
           and name not in {'wantobjects'}]

if __name__ == '__main__':
    _test()
