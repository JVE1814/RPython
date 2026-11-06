"""Tk font handling: named fonts, font metrics and the available families."""

import itertools
import tkinter

_VERSION = "0.9"


def __getattr__(name):
    if name == "__version__":
        import warnings
        warnings.warn("'__version__' is deprecated and slated for removal "
                      "in Python 3.20", DeprecationWarning, stacklevel=2)
        return _VERSION
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")

__all__ = ["NORMAL", "ROMAN", "BOLD", "ITALIC",
           "nametofont", "Font", "families", "names"]

# weight and slant values
NORMAL = "normal"
ROMAN = "roman"
BOLD = "bold"
ITALIC = "italic"


def nametofont(name, root=None):
    """Return the Font object for the existing named font *name*."""
    return Font(name=name, exists=True, root=root)


class Font:
    """A Tk named font.

    Font(root=None, font=None, name=None, exists=False, **options)

    *font* is a font description or the name of a named font to copy;
    *name* is the font's name (default: a generated one).  With *exists*
    the named font must already exist and is only (re)configured.  Options:
    family, size (points, or pixels when negative), weight (NORMAL or
    BOLD), slant (ROMAN or ITALIC), underline, overstrike.
    """

    counter = itertools.count(1)

    def _set(self, kw):
        options = []
        for k, v in kw.items():
            options.append("-" + k)
            options.append(str(v))
        return tuple(options)

    def _get(self, args):
        return tuple("-" + k for k in args)

    def _mkdict(self, args):
        return {args[i][1:]: args[i + 1] for i in range(0, len(args), 2)}

    def __init__(self, root=None, font=None, name=None, exists=False, **options):
        if root is None:
            root = tkinter._get_default_root('use font')
        tk = getattr(root, 'tk', root)
        if font:
            if str(font) in tk.splitlist(tk.call("font", "names")):
                # a named font: copy its configured options as they are
                font = tk.splitlist(tk.call("font", "configure", str(font)))
            else:
                font = tk.splitlist(tk.call("font", "actual", font))
        else:
            font = self._set(options)
        if not name:
            name = "font" + str(next(self.counter))
        self.name = name
        if exists:
            self.delete_font = False
            if str(name) not in tk.splitlist(tk.call("font", "names")):
                raise tkinter._tkinter.TclError(
                    "named font %s does not already exist" % (name,))
            if font:
                tk.call("font", "configure", self.name, *font)
        else:
            tk.call("font", "create", self.name, *font)
            self.delete_font = True
        self._tk = tk
        self._split = tk.splitlist
        self._call = tk.call

    def __str__(self):
        return str(self.name)

    def __repr__(self):
        return '<%s.%s object %r>' % (self.__class__.__module__,
                                      self.__class__.__qualname__, str(self.name))

    def __eq__(self, other):
        if not isinstance(other, Font):
            return NotImplemented
        return str(self.name) == str(other.name) and self._tk == other._tk

    def __getitem__(self, key):
        return self.cget(key)

    def __setitem__(self, key, value):
        self.configure(**{key: value})

    # fonts are neither containers nor iterables
    __iter__ = None

    def __del__(self):
        try:
            if self.delete_font:
                self._call("font", "delete", self.name)
        except Exception:
            pass

    def copy(self):
        """Return an independent copy of this font."""
        return Font(self._tk, **self.configure())

    def actual(self, option=None, displayof=None):
        """Return the actual attributes of the font (they may differ from
        the requested ones), or one of them."""
        args = ()
        if displayof:
            args = ('-displayof', displayof)
        if option:
            return self._call("font", "actual", self.name, *args, "-" + option)
        return self._mkdict(self._split(self._call("font", "actual", self.name, *args)))

    def cget(self, option):
        """Return a configured font option."""
        return self._call("font", "config", self.name, "-" + option)

    def config(self, **options):
        """Modify font options, or return all of them as a dict."""
        if options:
            self._call("font", "config", self.name, *self._set(options))
        else:
            return self._mkdict(self._split(self._call("font", "config", self.name)))

    configure = config

    def measure(self, text, displayof=None):
        """Return the width of *text* in this font, in pixels."""
        args = (text,)
        if displayof:
            args = ('-displayof', displayof, text)
        return self._tk.getint(self._call("font", "measure", self.name, *args))

    def metrics(self, *options, **kw):
        """Return font metrics (ascent, descent, linespace, fixed): one
        value for a single name, else a dict."""
        args = ()
        displayof = kw.pop('displayof', None)
        if displayof:
            args = ('-displayof', displayof)
        if options:
            return self._tk.getint(
                self._call("font", "metrics", self.name, *args, *self._get(options)))
        res = self._split(self._call("font", "metrics", self.name, *args))
        return {res[i][1:]: self._tk.getint(res[i + 1])
                for i in range(0, len(res), 2)}


def families(root=None, displayof=None):
    """Return the available font families."""
    if root is None:
        root = tkinter._get_default_root('use font.families()')
    args = ()
    if displayof:
        args = ('-displayof', displayof)
    return root.tk.splitlist(root.tk.call("font", "families", *args))


def names(root=None):
    """Return the names of the defined named fonts."""
    if root is None:
        root = tkinter._get_default_root('use font.names()')
    return root.tk.splitlist(root.tk.call("font", "names"))


if __name__ == "__main__":
    root = tkinter.Tk()
    f = Font(family="times", size=30, weight=NORMAL)
    print(f.actual())
    print(f.actual("family"))
    print(f.actual("weight"))
    print(f.config())
    print(f.cget("family"))
    print(f.cget("weight"))
    print(names())
    print(f.measure("hello"), f.metrics("linespace"))
    print(f.metrics(displayof=root))
    f = Font(font=("Courier", 20, "bold"))
    print(f.measure("hello"), f.metrics("linespace", displayof=root))
    w = tkinter.Label(root, text="Hello, world", font=f)
    w.pack()
    w = tkinter.Button(root, text="Quit!", command=root.destroy)
    w.pack()
    fb = Font(font=w["font"]).copy()
    fb.config(weight=BOLD)
    w.config(font=fb)
    tkinter.mainloop()
