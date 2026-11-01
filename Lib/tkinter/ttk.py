"""Themed Tk widgets (the Tk "ttk" widget set).

The ttk widgets look native on each platform and separate a widget's
behaviour from its appearance: appearance is described by styles, managed
with the Style class.  Many widgets (Button, Checkbutton, Entry, Frame,
Label, LabelFrame, Menubutton, PanedWindow, Radiobutton, Scale, Scrollbar)
replace their classic tkinter counterparts; Combobox, Notebook, Progressbar,
Separator, Sizegrip, Spinbox and Treeview are new.
"""

__version__ = "0.3.1"

__all__ = ["Button", "Checkbutton", "Combobox", "Entry", "Frame", "Label",
           "Labelframe", "LabelFrame", "Menubutton", "Notebook", "Panedwindow",
           "PanedWindow", "Progressbar", "Radiobutton", "Scale", "Scrollbar",
           "Separator", "Sizegrip", "Spinbox", "Style", "Treeview",
           "LabeledScale", "OptionMenu",
           "tclobjs_to_py", "setup_master"]

import tkinter
from tkinter import _flatten, _join, _stringify, _splitdict


# ---------------------------------------------------------------------------
# Formatting Python values as ttk option values and scripts

def _format_optvalue(value, script=False):
    """Format one option value (as a script fragment if *script*)."""
    if script:
        return _stringify(value)
    if isinstance(value, (list, tuple)):
        return _join(value)
    return value


def _format_optdict(optdict, script=False, ignore=None):
    """Turn {'opt': value, ...} into ('-opt', value, ...), skipping the
    names in *ignore*; a value of None yields only the option name."""
    opts = []
    for opt, value in optdict.items():
        if ignore and opt in ignore:
            continue
        opts.append('-%s' % opt)
        if value is not None:
            opts.append(_format_optvalue(value, script))
    return _flatten(opts)


def _mapdict_values(items):
    """Flatten [(state..., value), ...] into [statespec, value, ...].

    Several states are joined into one string; a single state is used as is
    (or '' when it is false)."""
    flat = []
    for *states, value in items:
        if len(states) == 1:
            spec = states[0] or ''
        else:
            spec = ' '.join(states)
        flat.append(spec)
        flat.append(value)
    return flat


def _format_mapdict(mapdict, script=False):
    """Turn {'opt': [(state..., value), ...]} into ('-opt', spec, ...)."""
    opts = []
    for opt, value in mapdict.items():
        opts.append('-%s' % opt)
        opts.append(_format_optvalue(_mapdict_values(value), script))
    return _flatten(opts)


def _format_elemcreate(etype, script=False, *args, **kw):
    """Format the arguments of `ttk::style element create` for *etype*."""
    if etype in ('image', 'vsapi'):
        if etype == 'image':
            iname = args[0]
            spec = _format_optvalue([iname] + _mapdict_values(args[1:]), script)
        else:
            class_name, part_id = args[:2]
            statemap = _mapdict_values(args[2] if len(args) > 2 else [('', 1)])
            if script:
                spec = '%s %s %s' % (class_name, part_id, _stringify(statemap))
            else:
                spec = (class_name, part_id, tuple(statemap))
        opts = _format_optdict(kw, script)
        if script:
            opts = ' '.join(opts)
        if etype == 'vsapi' and not script:
            return spec + (opts,)
        return spec, opts
    if etype == 'from':
        spec = args[0]
        opts = tuple(args[1:2])
        if script:
            opts = ' '.join(opts)
        return spec, opts
    return None, ()


def _format_layoutlist(layout, indent=0, indent_size=2):
    """Format a layout list [(element, {options, 'children': [...]})]
    as the Tcl layout description; returns (text, indent)."""
    lines = []
    pad = ' ' * indent
    for elem, opts in layout:
        opts = opts or {}
        has_children = 'children' in opts
        head = pad + elem
        fopts = ' '.join(_format_optdict(opts, True, ('children',)))
        if fopts:
            head = head + ' ' + fopts
        if has_children:
            lines.append(head + ' -children {')
            sub, _ = _format_layoutlist(opts['children'], indent + indent_size,
                                        indent_size)
            lines.append(sub)
            lines.append(pad + '}')
        else:
            lines.append(head)
    return '\n'.join(lines), indent


def _script_from_settings(settings):
    """Build the Tcl script for Style.theme_create/theme_settings."""
    script = []
    for name, opts in settings.items():
        if opts.get('configure'):
            s = ' '.join(_format_optdict(opts['configure'], True))
            script.append('ttk::style configure %s %s;' % (name, s))
        if opts.get('map'):
            s = ' '.join(_format_mapdict(opts['map'], True))
            script.append('ttk::style map %s %s;' % (name, s))
        if 'layout' in opts:
            if opts['layout']:
                s = _format_layoutlist(opts['layout'])[0]
            else:
                s = 'null'
            script.append('ttk::style layout %s {\n%s\n}' % (name, s))
        if opts.get('element create'):
            eopts = opts['element create']
            etype = eopts[0]
            n = 1
            while n < len(eopts) and not hasattr(eopts[n], 'items'):
                n += 1
            elemargs = eopts[1:n]
            elemkw = eopts[n] if n < len(eopts) and eopts[n] else {}
            spec, eo = _format_elemcreate(etype, True, *elemargs, **elemkw)
            script.append('ttk::style element create %s %s %s %s'
                          % (name, etype, spec, eo))
    return '\n'.join(script)


# ---------------------------------------------------------------------------
# Converting Tcl results back

def _list_from_statespec(stuple):
    """Turn a flat (statespec, value, ...) sequence into
    [(state..., value), ...]."""
    items = []
    for val in stuple:
        typename = getattr(val, 'typename', None)
        if typename is not None:
            val = str(val)
            if typename == 'StateSpec':
                val = val.split()
        items.append(val)
    it = iter(items)
    return [_flatten(spec) + (val,) for spec, val in zip(it, it)]


def _list_from_layouttuple(tk, ltuple):
    """Turn a Tcl layout description into [(element, {options}), ...]."""
    ltuple = tk.splitlist(ltuple)
    result = []
    i = 0
    while i < len(ltuple):
        name = ltuple[i]
        opts = {}
        result.append((name, opts))
        i += 1
        while i < len(ltuple):
            opt, val = ltuple[i:i + 2]
            if not opt.startswith('-'):
                break
            opt = opt[1:]
            i += 2
            if opt == 'children':
                val = _list_from_layouttuple(tk, val)
            opts[opt] = val
    return result


def _val_or_dict(tk, options, *args):
    """Run the command *args* with the formatted *options*.

    With no options, or with name/value pairs, the result is parsed into a
    dict; a single option name (value None) returns that option's value."""
    options = _format_optdict(options)
    if not options:
        res = tk.call(*args)
        return _splitdict(tk, res, conv=_tclobj_to_py)
    res = tk.call(*(args + options))
    if len(options) % 2:
        return res
    return _splitdict(tk, res, conv=_tclobj_to_py)


def _convert_stringval(value):
    """Return str(value) as an int if it looks like one."""
    value = str(value)
    try:
        value = int(value)
    except (ValueError, TypeError):
        pass
    return value


def _to_number(x):
    if isinstance(x, str):
        if '.' in x:
            x = float(x)
        else:
            x = int(x)
    return x


def _tclobj_to_py(val):
    """Convert a value returned by Tcl into a friendlier Python value."""
    if val and hasattr(val, '__len__') and not isinstance(val, str):
        if getattr(val[0], 'typename', None) == 'StateSpec':
            val = _list_from_statespec(val)
        else:
            val = list(map(_convert_stringval, val))
    elif hasattr(val, 'typename'):
        val = _convert_stringval(val)
    return val


def tclobjs_to_py(adict):
    """Convert the Tcl values in *adict* (in place) and return it."""
    for opt, val in adict.items():
        adict[opt] = _tclobj_to_py(val)
    return adict


def setup_master(master=None):
    """Return *master*, or the default root window when it is None."""
    if master is None:
        master = tkinter._get_default_root()
    return master


# ---------------------------------------------------------------------------
# Styles

class Style(object):
    """Access to the ttk style database."""

    _name = "ttk::style"

    def __init__(self, master=None):
        master = setup_master(master)
        self.master = master
        self.tk = self.master.tk

    def configure(self, style, query_opt=None, **kw):
        """Query or set the default options of *style*."""
        if query_opt:
            kw[query_opt] = None
        result = _val_or_dict(self.tk, kw, self._name, "configure", style)
        if result or query_opt:
            return result

    def map(self, style, query_opt=None, **kw):
        """Query or set state-dependent option values of *style*.

        Each keyword value is a list of (state..., value) tuples."""
        if query_opt is not None:
            result = self.tk.call(self._name, "map", style, '-%s' % query_opt)
            return _list_from_statespec(self.tk.splitlist(result))
        result = self.tk.call(self._name, "map", style, *_format_mapdict(kw))
        return {k: _list_from_statespec(self.tk.splitlist(v))
                for k, v in _splitdict(self.tk, result).items()}

    def lookup(self, style, option, state=None, default=None):
        """Return the value of *option* in *style* for *state*."""
        state = ' '.join(state) if state else ''
        return self.tk.call(self._name, "lookup", style, '-%s' % option,
                            state, default)

    def layout(self, style, layoutspec=None):
        """Query or define the layout of *style*.

        A layout is a list of (element, options) pairs where options may
        include side, sticky, expand, border and children (a nested
        layout).  An empty list makes the style have no layout."""
        lspec = None
        if layoutspec:
            lspec = _format_layoutlist(layoutspec)[0]
        elif layoutspec is not None:
            lspec = "null"
        return _list_from_layouttuple(self.tk,
            self.tk.call(self._name, "layout", style, lspec))

    def element_create(self, elementname, etype, *args, **kw):
        """Create an element of type *etype* in the current theme."""
        *spec, opts = _format_elemcreate(etype, False, *args, **kw)
        self.tk.call(self._name, "element", "create", elementname, etype,
                     *spec, *opts)

    def element_names(self):
        """Return the elements defined in the current theme."""
        return tuple(n.lstrip('-') for n in self.tk.splitlist(
            self.tk.call(self._name, "element", "names")))

    def element_options(self, elementname):
        """Return the options of *elementname*."""
        return tuple(o.lstrip('-') for o in self.tk.splitlist(
            self.tk.call(self._name, "element", "options", elementname)))

    def theme_create(self, themename, parent=None, settings=None):
        """Create a new theme, optionally based on *parent*."""
        script = _script_from_settings(settings) if settings else ''
        if parent:
            self.tk.call(self._name, "theme", "create", themename,
                         "-parent", parent, "-settings", script)
        else:
            self.tk.call(self._name, "theme", "create", themename,
                         "-settings", script)

    def theme_settings(self, themename, settings):
        """Change the settings of *themename* (see theme_create)."""
        script = _script_from_settings(settings)
        self.tk.call(self._name, "theme", "settings", themename, script)

    def theme_names(self):
        """Return the names of all known themes."""
        return self.tk.splitlist(self.tk.call(self._name, "theme", "names"))

    def theme_use(self, themename=None):
        """Return the current theme, or switch to *themename*."""
        if themename is None:
            return self.tk.eval("return $ttk::currentTheme")
        self.tk.call("ttk::setTheme", themename)


# ---------------------------------------------------------------------------
# Widgets

class Widget(tkinter.Widget):
    """Base class of the themed widgets."""

    def __init__(self, master, widgetname, kw=None):
        """Create a themed widget of Tk class *widgetname*.

        Standard options: class, cursor, takefocus, style.
        Scrollable widgets: xscrollcommand, yscrollcommand.
        Label widgets: text, textvariable, underline, image, compound,
        width.
        """
        master = setup_master(master)
        tkinter.Widget.__init__(self, master, widgetname, kw=kw)

    def identify(self, x, y):
        """Return the name of the element at (x, y), or ''."""
        return self.tk.call(self._w, "identify", x, y)

    def instate(self, statespec, callback=None, *args, **kw):
        """Test the widget state; if it matches and *callback* is given,
        return callback(*args, **kw)."""
        ret = self.tk.getboolean(
            self.tk.call(self._w, "instate", ' '.join(statespec)))
        if ret and callback is not None:
            return callback(*args, **kw)
        return ret

    def state(self, statespec=None):
        """Return the widget state, or change it (returning the changes
        needed to undo it)."""
        if statespec is not None:
            statespec = ' '.join(statespec)
        return self.tk.splitlist(str(self.tk.call(self._w, "state", statespec)))


class Button(Widget):
    """Themed push button."""

    def __init__(self, master=None, **kw):
        """Options: command, default, width (plus the standard and label
        options)."""
        Widget.__init__(self, master, "ttk::button", kw)

    def invoke(self):
        """Invoke the command of the button."""
        return self.tk.call(self._w, "invoke")


class Checkbutton(Widget):
    """Themed check button."""

    def __init__(self, master=None, **kw):
        """Options: command, offvalue, onvalue, variable."""
        Widget.__init__(self, master, "ttk::checkbutton", kw)

    def invoke(self):
        """Toggle the button and invoke its command."""
        return self.tk.call(self._w, "invoke")


class Entry(Widget, tkinter.Entry):
    """Themed single-line text entry."""

    def __init__(self, master=None, widget=None, **kw):
        """Options: exportselection, invalidcommand, justify, show, state,
        textvariable, validate, validatecommand, width."""
        Widget.__init__(self, master, widget or "ttk::entry", kw)

    def bbox(self, index):
        """Return the bounding box of the character at *index*."""
        return self._getints(self.tk.call(self._w, "bbox", index))

    def identify(self, x, y):
        """Return the element at (x, y)."""
        return self.tk.call(self._w, "identify", x, y)

    def validate(self):
        """Force validation; return True if the contents are valid."""
        return self.tk.getboolean(self.tk.call(self._w, "validate"))


class Combobox(Entry):
    """Entry with a drop-down list of values."""

    def __init__(self, master=None, **kw):
        """Options: exportselection, justify, height, postcommand, state,
        textvariable, values, width."""
        Entry.__init__(self, master, "ttk::combobox", **kw)

    def current(self, newindex=None):
        """Return the index of the current value (-1 if not in the list),
        or select the value at *newindex*."""
        if newindex is None:
            res = self.tk.call(self._w, "current")
            if res == '':
                return -1
            return self.tk.getint(res)
        return self.tk.call(self._w, "current", newindex)

    def set(self, value):
        """Set the value."""
        self.tk.call(self._w, "set", value)


class Frame(Widget):
    """Themed frame."""

    def __init__(self, master=None, **kw):
        """Options: borderwidth, relief, padding, width, height."""
        Widget.__init__(self, master, "ttk::frame", kw)


class Label(Widget):
    """Themed label."""

    def __init__(self, master=None, **kw):
        """Options: anchor, background, font, foreground, justify, padding,
        relief, text, wraplength."""
        Widget.__init__(self, master, "ttk::label", kw)


class Labelframe(Widget):
    """Themed frame with a label."""

    def __init__(self, master=None, **kw):
        """Options: labelanchor, text, underline, padding, labelwidget,
        width, height."""
        Widget.__init__(self, master, "ttk::labelframe", kw)

LabelFrame = Labelframe


class Menubutton(Widget):
    """Themed button that posts a menu."""

    def __init__(self, master=None, **kw):
        """Options: direction, menu."""
        Widget.__init__(self, master, "ttk::menubutton", kw)


class Notebook(Widget):
    """Tabbed container of windows."""

    def __init__(self, master=None, **kw):
        """Options: height, padding, width.

        Tab options: state, sticky, padding, text, image, compound,
        underline."""
        Widget.__init__(self, master, "ttk::notebook", kw)

    def add(self, child, **kw):
        """Add *child* as a new tab (or show a hidden one)."""
        self.tk.call(self._w, "add", child, *(_format_optdict(kw)))

    def forget(self, tab_id):
        """Remove the tab *tab_id* (the window is unmanaged)."""
        self.tk.call(self._w, "forget", tab_id)

    def hide(self, tab_id):
        """Hide the tab *tab_id* (add() shows it again)."""
        self.tk.call(self._w, "hide", tab_id)

    def identify(self, x, y):
        """Return the element of the tab at (x, y)."""
        return self.tk.call(self._w, "identify", x, y)

    def index(self, tab_id):
        """Return the numeric index of *tab_id* ("end" gives the count)."""
        return self.tk.getint(self.tk.call(self._w, "index", tab_id))

    def insert(self, pos, child, **kw):
        """Insert a tab for *child* at *pos*."""
        self.tk.call(self._w, "insert", pos, child, *(_format_optdict(kw)))

    def select(self, tab_id=None):
        """Select *tab_id*, or return the selected tab's window."""
        return self.tk.call(self._w, "select", tab_id)

    def tab(self, tab_id, option=None, **kw):
        """Query or modify the options of tab *tab_id*."""
        if option is not None:
            kw[option] = None
        return _val_or_dict(self.tk, kw, self._w, "tab", tab_id)

    def tabs(self):
        """Return the windows managed by the notebook."""
        return self.tk.splitlist(self.tk.call(self._w, "tabs") or ())

    def enable_traversal(self):
        """Enable keyboard traversal (Control-Tab, Alt-mnemonics)."""
        self.tk.call("ttk::notebook::enableTraversal", self._w)


class Panedwindow(Widget, tkinter.PanedWindow):
    """Themed paned window."""

    def __init__(self, master=None, **kw):
        """Options: orient, width, height.  Pane option: weight."""
        Widget.__init__(self, master, "ttk::panedwindow", kw)

    forget = tkinter.PanedWindow.forget

    def insert(self, pos, child, **kw):
        """Insert a pane for *child* at *pos*."""
        self.tk.call(self._w, "insert", pos, child, *(_format_optdict(kw)))

    def pane(self, pane, option=None, **kw):
        """Query or modify the options of *pane*."""
        if option is not None:
            kw[option] = None
        return _val_or_dict(self.tk, kw, self._w, "pane", pane)

    def sashpos(self, index, newpos=None):
        """Return (or set) the position of sash *index*."""
        return self.tk.getint(self.tk.call(self._w, "sashpos", index, newpos))

PanedWindow = Panedwindow


class Progressbar(Widget):
    """Progress indicator."""

    def __init__(self, master=None, **kw):
        """Options: orient, length, mode, maximum, value, variable, phase."""
        Widget.__init__(self, master, "ttk::progressbar", kw)

    def start(self, interval=None):
        """Auto-increment every *interval* milliseconds (default 50)."""
        self.tk.call(self._w, "start", interval)

    def step(self, amount=None):
        """Increment the value by *amount* (default 1.0)."""
        self.tk.call(self._w, "step", amount)

    def stop(self):
        """Stop auto-increment mode."""
        self.tk.call(self._w, "stop")


class Radiobutton(Widget):
    """Themed radio button."""

    def __init__(self, master=None, **kw):
        """Options: command, value, variable."""
        Widget.__init__(self, master, "ttk::radiobutton", kw)

    def invoke(self):
        """Select the button and invoke its command."""
        return self.tk.call(self._w, "invoke")


class Scale(Widget, tkinter.Scale):
    """Themed slider."""

    def __init__(self, master=None, **kw):
        """Options: command, from, length, orient, to, value, variable."""
        Widget.__init__(self, master, "ttk::scale", kw)

    def configure(self, cnf=None, **kw):
        """Modify options; changing the range generates <<RangeChanged>>."""
        retval = Widget.configure(self, cnf, **kw)
        if not isinstance(cnf, (type(None), str)):
            kw.update(cnf)
        if any(k in kw for k in ('from', 'from_', 'to')):
            self.event_generate('<<RangeChanged>>')
        return retval

    def get(self, x=None, y=None):
        """Return the current value (or the value at pixel (x, y))."""
        return self.tk.getdouble(self.tk.call(self._w, 'get', x, y))


class Scrollbar(Widget, tkinter.Scrollbar):
    """Themed scrollbar."""

    def __init__(self, master=None, **kw):
        """Options: command, orient."""
        Widget.__init__(self, master, "ttk::scrollbar", kw)


class Separator(Widget):
    """Horizontal or vertical separator line."""

    def __init__(self, master=None, **kw):
        """Option: orient."""
        Widget.__init__(self, master, "ttk::separator", kw)


class Sizegrip(Widget):
    """Grip for resizing the top-level window."""

    def __init__(self, master=None, **kw):
        Widget.__init__(self, master, "ttk::sizegrip", kw)


class Spinbox(Entry):
    """Entry with increment/decrement arrows."""

    def __init__(self, master=None, **kw):
        """Options: values, from, to, increment, format, command, wrap."""
        Entry.__init__(self, master, "ttk::spinbox", **kw)

    def set(self, value):
        """Set the value."""
        self.tk.call(self._w, "set", value)


class Treeview(Widget, tkinter.XView, tkinter.YView):
    """Hierarchical list of items with columns."""

    def __init__(self, master=None, **kw):
        """Options: columns, displaycolumns, height, padding, selectmode,
        show.  Item options: text, image, values, open, tags."""
        Widget.__init__(self, master, "ttk::treeview", kw)

    def bbox(self, item, column=None):
        """Return the bounding box of *item* (or of one of its cells), or
        '' when it is not visible."""
        return self._getints(self.tk.call(self._w, "bbox", item, column)) or ''

    def get_children(self, item=None):
        """Return the children of *item* (default: the root)."""
        return self.tk.splitlist(
            self.tk.call(self._w, "children", item or '') or ())

    def set_children(self, item, *newchildren):
        """Replace the children of *item*."""
        self.tk.call(self._w, "children", item, newchildren)

    def column(self, column, option=None, **kw):
        """Query or modify the options of *column*."""
        if option is not None:
            kw[option] = None
        return _val_or_dict(self.tk, kw, self._w, "column", column)

    def delete(self, *items):
        """Delete *items* and their descendants."""
        self.tk.call(self._w, "delete", items)

    def detach(self, *items):
        """Unlink *items* from the tree (they can be reattached)."""
        self.tk.call(self._w, "detach", items)

    def exists(self, item):
        """Return True if *item* is in the tree."""
        return self.tk.getboolean(self.tk.call(self._w, "exists", item))

    def focus(self, item=None):
        """Return the focus item, or set it."""
        return self.tk.call(self._w, "focus", item)

    def heading(self, column, option=None, **kw):
        """Query or modify the heading options of *column* (text, image,
        anchor, command)."""
        cmd = kw.get('command')
        if cmd and not isinstance(cmd, str):
            kw['command'] = self.master.register(cmd, self._substitute)
        if option is not None:
            kw[option] = None
        return _val_or_dict(self.tk, kw, self._w, 'heading', column)

    def identify(self, component, x, y):
        """Return the *component* (row, column, element, ...) at (x, y)."""
        return self.tk.call(self._w, "identify", component, x, y)

    def identify_row(self, y):
        """Return the item at y."""
        return self.identify("row", 0, y)

    def identify_column(self, x):
        """Return the column at x ("#n")."""
        return self.identify("column", x, 0)

    def identify_region(self, x, y):
        """Return the region at (x, y): heading, separator, tree, cell,
        nothing."""
        return self.identify("region", x, y)

    def identify_element(self, x, y):
        """Return the element at (x, y)."""
        return self.identify("element", x, y)

    def index(self, item):
        """Return the position of *item* among its siblings."""
        return self.tk.getint(self.tk.call(self._w, "index", item))

    def insert(self, parent, index, iid=None, **kw):
        """Insert a new item under *parent* at *index* ("end" appends);
        return its identifier."""
        opts = _format_optdict(kw)
        if iid is not None:
            res = self.tk.call(self._w, "insert", parent, index, "-id", iid, *opts)
        else:
            res = self.tk.call(self._w, "insert", parent, index, *opts)
        return res

    def item(self, item, option=None, **kw):
        """Query or modify the options of *item*."""
        if option is not None:
            kw[option] = None
        return _val_or_dict(self.tk, kw, self._w, "item", item)

    def move(self, item, parent, index):
        """Move *item* under *parent* at *index*."""
        self.tk.call(self._w, "move", item, parent, index)

    reattach = move

    def next(self, item):
        """Return the next sibling of *item* (or '')."""
        return self.tk.call(self._w, "next", item)

    def parent(self, item):
        """Return the parent of *item* (or '')."""
        return self.tk.call(self._w, "parent", item)

    def prev(self, item):
        """Return the previous sibling of *item* (or '')."""
        return self.tk.call(self._w, "prev", item)

    def see(self, item):
        """Open the ancestors of *item* and scroll it into view."""
        self.tk.call(self._w, "see", item)

    def selection(self):
        """Return the selected items."""
        return self.tk.splitlist(self.tk.call(self._w, "selection"))

    def _selection(self, selop, items):
        if len(items) == 1 and isinstance(items[0], (tuple, list)):
            items = items[0]
        self.tk.call(self._w, "selection", selop, items)

    def selection_set(self, *items):
        """Make *items* the selection."""
        self._selection("set", items)

    def selection_add(self, *items):
        """Add *items* to the selection."""
        self._selection("add", items)

    def selection_remove(self, *items):
        """Remove *items* from the selection."""
        self._selection("remove", items)

    def selection_toggle(self, *items):
        """Toggle the selection of *items*."""
        self._selection("toggle", items)

    def set(self, item, column=None, value=None):
        """Return all values of *item* as a dict, the value in *column*,
        or set the value in *column*."""
        res = self.tk.call(self._w, "set", item, column, value)
        if column is None and value is None:
            return _splitdict(self.tk, res, cut_minus=False, conv=_tclobj_to_py)
        return res

    def tag_bind(self, tagname, sequence=None, callback=None):
        """Bind *callback* to an event on the items with *tagname*."""
        self._bind((self._w, "tag", "bind", tagname), sequence, callback, add=0)

    def tag_configure(self, tagname, option=None, **kw):
        """Query or modify the options of *tagname*."""
        if option is not None:
            kw[option] = None
        return _val_or_dict(self.tk, kw, self._w, "tag", "configure", tagname)

    def tag_has(self, tagname, item=None):
        """Return the items with *tagname*, or whether *item* has it."""
        if item is None:
            return self.tk.splitlist(self.tk.call(self._w, "tag", "has", tagname))
        return self.tk.getboolean(self.tk.call(self._w, "tag", "has", tagname, item))


# ---------------------------------------------------------------------------
# Composite widgets

class LabeledScale(Frame):
    """A ttk Scale with a label above (or below) it showing the value."""

    def __init__(self, master=None, variable=None, from_=0, to=10, **kw):
        """Create the widget; *variable* (default a new IntVar) holds the
        value, *from_*/*to* give the range, and the keyword *compound*
        ('top' or 'bottom') places the label."""
        self._label_top = kw.pop('compound', 'top') == 'top'
        Frame.__init__(self, master, **kw)
        self._variable = variable or tkinter.IntVar(master)
        self._variable.set(from_)
        self._last_valid = from_
        self.label = Label(self)
        self.scale = Scale(self, variable=self._variable, from_=from_, to=to)
        self.scale.bind('<<RangeChanged>>', self._adjust)
        scale_side = 'bottom' if self._label_top else 'top'
        label_side = 'top' if scale_side == 'bottom' else 'bottom'
        self.scale.pack(side=scale_side, fill='x')
        # an empty label reserves the space where the value label is placed
        spacer = Label(self)
        spacer.pack(side=label_side)
        spacer.lower()
        self.label.place(anchor='n' if label_side == 'top' else 's')
        self.__tracecb = self._variable.trace_add('write', self._adjust)
        self.bind('<Configure>', self._adjust)
        self.bind('<Map>', self._adjust)

    def destroy(self):
        """Destroy the widget and stop tracing the variable."""
        try:
            self._variable.trace_remove('write', self.__tracecb)
        except AttributeError:
            pass
        else:
            del self._variable
        super().destroy()
        self.label = None
        self.scale = None

    def _adjust(self, *args):
        # move the label over the slider and show the new value
        def adjust_label():
            self.update_idletasks()
            x, y = self.scale.coords()
            if self._label_top:
                y = self.scale.winfo_y() - self.label.winfo_reqheight()
            else:
                y = self.scale.winfo_reqheight() + self.label.winfo_reqheight()
            self.label.place_configure(x=x, y=y)

        from_ = _to_number(self.scale['from'])
        to = _to_number(self.scale['to'])
        if to < from_:
            from_, to = to, from_
        newval = self._variable.get()
        if not from_ <= newval <= to:
            self.value = self._last_valid
            return
        self._last_valid = newval
        self.label['text'] = newval
        self.after_idle(adjust_label)

    @property
    def value(self):
        """The current value of the scale."""
        return self._variable.get()

    @value.setter
    def value(self, val):
        self._variable.set(val)


class OptionMenu(Menubutton):
    """Themed menubutton offering a choice among values."""

    def __init__(self, master, variable, default=None, *values, **kwargs):
        """Create the widget for *variable*; *default* is the initial value
        and *values* the choices.  Keywords: style, direction, command
        (called with the chosen value), name."""
        kw = {'textvariable': variable, 'style': kwargs.pop('style', None),
              'direction': kwargs.pop('direction', None),
              'name': kwargs.pop('name', None)}
        Menubutton.__init__(self, master, **kw)
        self['menu'] = tkinter.Menu(self, tearoff=False)
        self._variable = variable
        self._callback = kwargs.pop('command', None)
        if kwargs:
            raise tkinter.TclError('unknown option -%s' % next(iter(kwargs)))
        self.set_menu(default, *values)

    def __getitem__(self, item):
        if item == 'menu':
            return self.nametowidget(Menubutton.__getitem__(self, item))
        return Menubutton.__getitem__(self, item)

    def set_menu(self, default=None, *values):
        """Replace the choices by *values* and select *default*."""
        menu = self['menu']
        menu.delete(0, 'end')
        for val in values:
            menu.add_radiobutton(
                label=val,
                command=(None if self._callback is None
                         else lambda val=val: self._callback(val)),
                variable=self._variable)
        if default:
            self._variable.set(default)

    def destroy(self):
        """Destroy the widget and its menu."""
        try:
            del self._variable
        except AttributeError:
            pass
        super().destroy()
