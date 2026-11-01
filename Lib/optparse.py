"""Command-line option parsing (the "Optik" interface).

Build an OptionParser, add_option() for each option, then call
parse_args() to get a (Values, positional_args) pair.  This module is
retained for compatibility; argparse is the recommended replacement.
"""

__all__ = ['Option',
           'make_option',
           'SUPPRESS_HELP',
           'SUPPRESS_USAGE',
           'Values',
           'OptionContainer',
           'OptionGroup',
           'OptionParser',
           'HelpFormatter',
           'IndentedHelpFormatter',
           'TitledHelpFormatter',
           'OptParseError',
           'OptionError',
           'OptionConflictError',
           'OptionValueError',
           'BadOptionError',
           'check_choice']

import sys
import os

try:
    from gettext import gettext as _, ngettext
except ImportError:
    def _(message):
        return message

    def ngettext(singular, plural, n):
        return singular if n == 1 else plural


def __getattr__(name):
    if name == "__version__":
        import warnings
        warnings.warn("'__version__' is deprecated and slated for removal "
                      "in Python 3.20", DeprecationWarning, stacklevel=2)
        return "1.5.3"
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")


def _repr(self):
    return "<%s at 0x%x: %s>" % (self.__class__.__name__, id(self), self)


# -- exceptions --------------------------------------------------------------

class OptParseError(Exception):
    def __init__(self, msg):
        self.msg = msg

    def __str__(self):
        return self.msg


class OptionError(OptParseError):
    """A problem with how an Option was defined (not with its use)."""

    def __init__(self, msg, option):
        self.msg = msg
        self.option_id = str(option)

    def __str__(self):
        if self.option_id:
            return "option %s: %s" % (self.option_id, self.msg)
        return self.msg


class OptionConflictError(OptionError):
    """Two options were given the same option string."""


class OptionValueError(OptParseError):
    """An option was given an unusable value on the command line."""


class BadOptionError(OptParseError):
    """An unknown option was found on the command line."""

    def __init__(self, opt_str):
        self.opt_str = opt_str

    def __str__(self):
        return _("no such option: %s") % self.opt_str


class AmbiguousOptionError(BadOptionError):
    """An abbreviated long option matched more than one option."""

    def __init__(self, opt_str, possibilities):
        BadOptionError.__init__(self, opt_str)
        self.possibilities = possibilities

    def __str__(self):
        return (_("ambiguous option: %s (%s?)")
                % (self.opt_str, ", ".join(self.possibilities)))


# -- help formatting ---------------------------------------------------------

class HelpFormatter:
    """Lays out the help text of an OptionParser.

    indent_increment -- columns added per nesting level
    max_help_position -- the help column never starts further right
    width -- total width (default: terminal width minus 2)
    short_first -- list short option strings before long ones
    """

    NO_DEFAULT_VALUE = "none"

    def __init__(self, indent_increment, max_help_position, width,
                 short_first):
        self.parser = None
        self.indent_increment = indent_increment
        if width is None:
            # $COLUMNS (even a silly value) wins over the terminal size
            try:
                width = int(os.environ["COLUMNS"])
            except (KeyError, ValueError):
                width = 80
            width -= 2
        self.width = width
        self.help_position = self.max_help_position = \
            min(max_help_position, max(width - 20, indent_increment * 2))
        self.current_indent = 0
        self.level = 0
        self.help_width = None
        self.short_first = short_first
        self.default_tag = "%default"
        self.option_strings = {}
        self._short_opt_fmt = "%s %s"
        self._long_opt_fmt = "%s=%s"

    def set_parser(self, parser):
        self.parser = parser

    def set_short_opt_delimiter(self, delim):
        if delim not in ("", " "):
            raise ValueError(
                "invalid metavar delimiter for short options: %r" % delim)
        self._short_opt_fmt = "%s" + delim + "%s"

    def set_long_opt_delimiter(self, delim):
        if delim not in ("=", " "):
            raise ValueError(
                "invalid metavar delimiter for long options: %r" % delim)
        self._long_opt_fmt = "%s" + delim + "%s"

    def indent(self):
        self.current_indent += self.indent_increment
        self.level += 1

    def dedent(self):
        self.current_indent -= self.indent_increment
        assert self.current_indent >= 0, "Indent decreased below 0."
        self.level -= 1

    def format_usage(self, usage):
        raise NotImplementedError("subclasses must implement")

    def format_heading(self, heading):
        raise NotImplementedError("subclasses must implement")

    def _format_text(self, text):
        """Fill *text* to the current indentation and width."""
        import textwrap
        width = max(self.width - self.current_indent, 11)
        pad = " " * self.current_indent
        return textwrap.fill(text, width,
                             initial_indent=pad, subsequent_indent=pad)

    def format_description(self, description):
        if not description:
            return ""
        return self._format_text(description) + "\n"

    def format_epilog(self, epilog):
        if not epilog:
            return ""
        return "\n" + self._format_text(epilog) + "\n"

    def expand_default(self, option):
        if self.parser is None or not self.default_tag:
            return option.help
        value = self.parser.defaults.get(option.dest)
        if value is NO_DEFAULT or value is None:
            value = self.NO_DEFAULT_VALUE
        return option.help.replace(self.default_tag, str(value))

    def format_option(self, option):
        # The option strings go in the left column; when they are too
        # wide, the help starts on the next line in the help column.
        import textwrap
        strings = self.option_strings[option]
        column = self.help_position - self.current_indent - 2
        if len(strings) > column:
            head = "%*s%s\n" % (self.current_indent, "", strings)
            first_indent = self.help_position
        else:
            head = "%*s%-*s  " % (self.current_indent, "", column, strings)
            first_indent = 0
        out = [head]
        if option.help:
            lines = textwrap.wrap(self.expand_default(option),
                                  self.help_width)
            out.append("%*s%s\n" % (first_indent, "", lines[0]))
            for line in lines[1:]:
                out.append("%*s%s\n" % (self.help_position, "", line))
        elif not head.endswith("\n"):
            out.append("\n")
        return "".join(out)

    def store_option_strings(self, parser):
        self.indent()
        widest = 0
        for option in parser.option_list:
            strings = self.format_option_strings(option)
            self.option_strings[option] = strings
            widest = max(widest, len(strings) + self.current_indent)
        self.indent()
        for group in parser.option_groups:
            for option in group.option_list:
                strings = self.format_option_strings(option)
                self.option_strings[option] = strings
                widest = max(widest, len(strings) + self.current_indent)
        self.dedent()
        self.dedent()
        self.help_position = min(widest + 2, self.max_help_position)
        self.help_width = max(self.width - self.help_position, 11)

    def format_option_strings(self, option):
        """'-f FILE, --file=FILE' (or without metavars for flags)."""
        if option.takes_value():
            metavar = option.metavar or option.dest.upper()
            shorts = [self._short_opt_fmt % (s, metavar)
                      for s in option._short_opts]
            longs = [self._long_opt_fmt % (l, metavar)
                     for l in option._long_opts]
        else:
            shorts = option._short_opts
            longs = option._long_opts
        if self.short_first:
            strings = shorts + longs
        else:
            strings = longs + shorts
        return ", ".join(strings)


class IndentedHelpFormatter(HelpFormatter):
    """Headings at the left margin, options indented below them."""

    def __init__(self, indent_increment=2, max_help_position=24,
                 width=None, short_first=1):
        HelpFormatter.__init__(self, indent_increment, max_help_position,
                               width, short_first)

    def format_usage(self, usage):
        return _("Usage: %s\n") % usage

    def format_heading(self, heading):
        return "%*s%s:\n" % (self.current_indent, "", heading)


class TitledHelpFormatter(HelpFormatter):
    """Underlined headings, options not indented."""

    def __init__(self, indent_increment=0, max_help_position=24,
                 width=None, short_first=0):
        HelpFormatter.__init__(self, indent_increment, max_help_position,
                               width, short_first)

    def format_usage(self, usage):
        return "%s  %s\n" % (self.format_heading(_("Usage")), usage)

    def format_heading(self, heading):
        return "%s\n%s\n" % (heading, "=-"[self.level] * len(heading))


# -- value conversion --------------------------------------------------------

def _parse_num(val, type):
    """Convert *val* with *type*, honouring 0x / 0b / 0 (octal) prefixes."""
    prefix = val[:2].lower()
    if prefix == "0x":
        return type(val, 16)
    if prefix == "0b":
        return type(val[2:] or "0", 2)
    if val[:1] == "0":
        return type(val, 8)
    return type(val, 10)


def _parse_int(val):
    return _parse_num(val, int)


_builtin_cvt = {"int": (_parse_int, _("integer")),
                "long": (_parse_int, _("integer")),
                "float": (float, _("floating-point")),
                "complex": (complex, _("complex"))}


def check_builtin(option, opt, value):
    convert, what = _builtin_cvt[option.type]
    try:
        return convert(value)
    except ValueError:
        raise OptionValueError(
            _("option %s: invalid %s value: %r") % (opt, what, value))


def check_choice(option, opt, value):
    if value in option.choices:
        return value
    choices = ", ".join(map(repr, option.choices))
    raise OptionValueError(
        _("option %s: invalid choice: %r (choose from %s)")
        % (opt, value, choices))


# Marks "no default given" (None is a legitimate default).
NO_DEFAULT = ("NO", "DEFAULT")


class Option:
    """One command-line option: its option strings and what to do with
    it (action, type, dest, default, nargs, const, choices, callback,
    callback_args, callback_kwargs, help, metavar)."""

    ATTRS = ['action',
             'type',
             'dest',
             'default',
             'nargs',
             'const',
             'choices',
             'callback',
             'callback_args',
             'callback_kwargs',
             'help',
             'metavar']

    ACTIONS = ("store",
               "store_const",
               "store_true",
               "store_false",
               "append",
               "append_const",
               "count",
               "callback",
               "help",
               "version")

    # actions that store something into a destination
    STORE_ACTIONS = ("store",
                     "store_const",
                     "store_true",
                     "store_false",
                     "append",
                     "append_const",
                     "count")

    # actions that accept a type
    TYPED_ACTIONS = ("store",
                     "append",
                     "callback")

    # actions that get the default type when none is given
    ALWAYS_TYPED_ACTIONS = ("store",
                            "append")

    # actions that need a const
    CONST_ACTIONS = ("store_const",
                     "append_const")

    TYPES = ("string", "int", "long", "float", "complex", "choice")

    TYPE_CHECKER = {"int": check_builtin,
                    "long": check_builtin,
                    "float": check_builtin,
                    "complex": check_builtin,
                    "choice": check_choice,
                    }

    # Filled in below with the validation methods run by __init__.
    CHECK_METHODS = None

    def __init__(self, *opts, **attrs):
        self._short_opts = []
        self._long_opts = []
        opts = self._check_opt_strings(opts)
        self._set_opt_strings(opts)
        self._set_attrs(attrs)
        for check in self.CHECK_METHODS:
            check(self)

    def _check_opt_strings(self, opts):
        opts = [opt for opt in opts if opt]
        if not opts:
            raise TypeError("at least one option string must be supplied")
        return opts

    def _set_opt_strings(self, opts):
        for opt in opts:
            if len(opt) < 2:
                raise OptionError(
                    "invalid option string %r: "
                    "must be at least two characters long" % opt, self)
            if len(opt) == 2:
                if opt[0] != "-" or opt[1] == "-":
                    raise OptionError(
                        "invalid short option string %r: "
                        "must be of the form -x, (x any non-dash char)"
                        % opt, self)
                self._short_opts.append(opt)
            else:
                if opt[:2] != "--" or opt[2] == "-":
                    raise OptionError(
                        "invalid long option string %r: "
                        "must start with --, followed by non-dash" % opt,
                        self)
                self._long_opts.append(opt)

    def _set_attrs(self, attrs):
        for name in self.ATTRS:
            if name in attrs:
                setattr(self, name, attrs.pop(name))
            elif name == 'default':
                self.default = NO_DEFAULT
            else:
                setattr(self, name, None)
        if attrs:
            raise OptionError("invalid keyword arguments: %s"
                              % ", ".join(sorted(attrs)), self)

    # validation, run in CHECK_METHODS order

    def _check_action(self):
        if self.action is None:
            self.action = "store"
        elif self.action not in self.ACTIONS:
            raise OptionError("invalid action: %r" % self.action, self)

    def _check_type(self):
        if self.type is None:
            if self.action in self.ALWAYS_TYPED_ACTIONS:
                self.type = "string" if self.choices is None else "choice"
            return
        if isinstance(self.type, type):
            # a builtin type object stands for its name
            self.type = self.type.__name__
        if self.type == "str":
            self.type = "string"
        if self.type not in self.TYPES:
            raise OptionError("invalid option type: %r" % self.type, self)
        if self.action not in self.TYPED_ACTIONS:
            raise OptionError(
                "must not supply a type for action %r" % self.action, self)

    def _check_choice(self):
        if self.type == "choice":
            if self.choices is None:
                raise OptionError(
                    "must supply a list of choices for type 'choice'", self)
            if not isinstance(self.choices, (tuple, list)):
                raise OptionError(
                    "choices must be a list of strings ('%s' supplied)"
                    % type(self.choices).__name__, self)
        elif self.choices is not None:
            raise OptionError(
                "must not supply choices for type %r" % self.type, self)

    def _check_dest(self):
        takes_value = (self.action in self.STORE_ACTIONS
                       or self.type is not None)
        if self.dest is None and takes_value:
            # derive it from the first long option, else the short one
            if self._long_opts:
                self.dest = self._long_opts[0][2:].replace('-', '_')
            else:
                self.dest = self._short_opts[0][1]

    def _check_const(self):
        if self.action not in self.CONST_ACTIONS and self.const is not None:
            raise OptionError(
                "'const' must not be supplied for action %r" % self.action,
                self)

    def _check_nargs(self):
        if self.action in self.TYPED_ACTIONS:
            if self.nargs is None:
                self.nargs = 1
        elif self.nargs is not None:
            raise OptionError(
                "'nargs' must not be supplied for action %r" % self.action,
                self)

    def _check_callback(self):
        if self.action == "callback":
            if not callable(self.callback):
                raise OptionError(
                    "callback not callable: %r" % self.callback, self)
            if (self.callback_args is not None
                    and not isinstance(self.callback_args, tuple)):
                raise OptionError(
                    "callback_args, if supplied, must be a tuple: not %r"
                    % self.callback_args, self)
            if (self.callback_kwargs is not None
                    and not isinstance(self.callback_kwargs, dict)):
                raise OptionError(
                    "callback_kwargs, if supplied, must be a dict: not %r"
                    % self.callback_kwargs, self)
        else:
            if self.callback is not None:
                raise OptionError(
                    "callback supplied (%r) for non-callback option"
                    % self.callback, self)
            if self.callback_args is not None:
                raise OptionError(
                    "callback_args supplied for non-callback option", self)
            if self.callback_kwargs is not None:
                raise OptionError(
                    "callback_kwargs supplied for non-callback option", self)

    CHECK_METHODS = [_check_action,
                     _check_type,
                     _check_choice,
                     _check_dest,
                     _check_const,
                     _check_nargs,
                     _check_callback]

    def __str__(self):
        return "/".join(self._short_opts + self._long_opts)

    __repr__ = _repr

    def takes_value(self):
        return self.type is not None

    def get_opt_string(self):
        if self._long_opts:
            return self._long_opts[0]
        return self._short_opts[0]

    # processing a value from the command line

    def check_value(self, opt, value):
        checker = self.TYPE_CHECKER.get(self.type)
        if checker is None:
            return value
        return checker(self, opt, value)

    def convert_value(self, opt, value):
        if value is None:
            return None
        if self.nargs == 1:
            return self.check_value(opt, value)
        return tuple(self.check_value(opt, v) for v in value)

    def process(self, opt, value, values, parser):
        value = self.convert_value(opt, value)
        return self.take_action(self.action, self.dest, opt, value,
                                values, parser)

    def take_action(self, action, dest, opt, value, values, parser):
        if action == "store":
            setattr(values, dest, value)
        elif action == "store_const":
            setattr(values, dest, self.const)
        elif action == "store_true":
            setattr(values, dest, True)
        elif action == "store_false":
            setattr(values, dest, False)
        elif action == "append":
            values.ensure_value(dest, []).append(value)
        elif action == "append_const":
            values.ensure_value(dest, []).append(self.const)
        elif action == "count":
            setattr(values, dest, values.ensure_value(dest, 0) + 1)
        elif action == "callback":
            args = self.callback_args or ()
            kwargs = self.callback_kwargs or {}
            self.callback(self, opt, value, parser, *args, **kwargs)
        elif action == "help":
            parser.print_help()
            parser.exit()
        elif action == "version":
            parser.print_version()
            parser.exit()
        else:
            raise ValueError("unknown action %r" % self.action)
        return 1


SUPPRESS_HELP = "SUPPRESS" + "HELP"
SUPPRESS_USAGE = "SUPPRESS" + "USAGE"


class Values:
    """The option values: one attribute per destination."""

    def __init__(self, defaults=None):
        if defaults:
            for name, value in defaults.items():
                setattr(self, name, value)

    def __str__(self):
        return str(self.__dict__)

    __repr__ = _repr

    def __eq__(self, other):
        if isinstance(other, Values):
            return self.__dict__ == other.__dict__
        if isinstance(other, dict):
            return self.__dict__ == other
        return NotImplemented

    def _update_careful(self, dict):
        """Copy the entries of *dict* whose names are already attributes
        (and whose values are not None)."""
        for name in dir(self):
            if name in dict:
                value = dict[name]
                if value is not None:
                    setattr(self, name, value)

    def _update_loose(self, dict):
        """Copy every entry of *dict*."""
        self.__dict__.update(dict)

    def _update(self, dict, mode):
        if mode == "careful":
            self._update_careful(dict)
        elif mode == "loose":
            self._update_loose(dict)
        else:
            raise ValueError("invalid update mode: %r" % mode)

    def read_module(self, modname, mode="careful"):
        __import__(modname)
        self._update(vars(sys.modules[modname]), mode)

    def read_file(self, filename, mode="careful"):
        namespace = {}
        with open(filename) as f:
            exec(f.read(), namespace)
        self._update(namespace, mode)

    def ensure_value(self, attr, value):
        if getattr(self, attr, None) is None:
            setattr(self, attr, value)
        return getattr(self, attr)


class OptionContainer:
    """What OptionParser and OptionGroup share: a list of options and the
    tables mapping option strings to them."""

    def __init__(self, option_class, conflict_handler, description):
        self._create_option_list()
        self.option_class = option_class
        self.set_conflict_handler(conflict_handler)
        self.set_description(description)

    def _create_option_mappings(self):
        self._short_opt = {}
        self._long_opt = {}
        self.defaults = {}

    def _share_option_mappings(self, parser):
        # groups use their parser's tables
        self._short_opt = parser._short_opt
        self._long_opt = parser._long_opt
        self.defaults = parser.defaults

    def set_conflict_handler(self, handler):
        if handler not in ("error", "resolve"):
            raise ValueError("invalid conflict_resolution value %r" % handler)
        self.conflict_handler = handler

    def set_description(self, description):
        self.description = description

    def get_description(self):
        return self.description

    def destroy(self):
        """Drop references so the parser can be collected."""
        del self._short_opt
        del self._long_opt
        del self.defaults

    def _check_conflict(self, option):
        clashes = []
        for opt in option._short_opts:
            if opt in self._short_opt:
                clashes.append((opt, self._short_opt[opt]))
        for opt in option._long_opts:
            if opt in self._long_opt:
                clashes.append((opt, self._long_opt[opt]))
        if not clashes:
            return
        if self.conflict_handler == "error":
            raise OptionConflictError(
                "conflicting option string(s): %s"
                % ", ".join(opt for opt, _old in clashes), option)
        # "resolve": the new option wins; older ones lose the strings
        for opt, old in clashes:
            if opt.startswith("--"):
                old._long_opts.remove(opt)
                del self._long_opt[opt]
            else:
                old._short_opts.remove(opt)
                del self._short_opt[opt]
            if not (old._short_opts or old._long_opts):
                old.container.option_list.remove(old)

    def add_option(self, *args, **kwargs):
        """add_option(Option) or add_option(opt_str, ..., kwarg=val, ...)"""
        if isinstance(args[0], str):
            option = self.option_class(*args, **kwargs)
        elif len(args) == 1 and not kwargs:
            option = args[0]
            if not isinstance(option, Option):
                raise TypeError("not an Option instance: %r" % option)
        else:
            raise TypeError("invalid arguments")

        self._check_conflict(option)
        self.option_list.append(option)
        option.container = self
        for opt in option._short_opts:
            self._short_opt[opt] = option
        for opt in option._long_opts:
            self._long_opt[opt] = option

        if option.dest is not None:
            if option.default is not NO_DEFAULT:
                self.defaults[option.dest] = option.default
            elif option.dest not in self.defaults:
                self.defaults[option.dest] = None
        return option

    def add_options(self, option_list):
        for option in option_list:
            self.add_option(option)

    def get_option(self, opt_str):
        return self._short_opt.get(opt_str) or self._long_opt.get(opt_str)

    def has_option(self, opt_str):
        return opt_str in self._short_opt or opt_str in self._long_opt

    def remove_option(self, opt_str):
        option = self._short_opt.get(opt_str)
        if option is None:
            option = self._long_opt.get(opt_str)
        if option is None:
            raise ValueError("no such option %r" % opt_str)
        for opt in option._short_opts:
            del self._short_opt[opt]
        for opt in option._long_opts:
            del self._long_opt[opt]
        option.container.option_list.remove(option)

    # help

    def format_option_help(self, formatter):
        if not self.option_list:
            return ""
        return "".join(formatter.format_option(option)
                       for option in self.option_list
                       if option.help is not SUPPRESS_HELP)

    def format_description(self, formatter):
        return formatter.format_description(self.get_description())

    def format_help(self, formatter):
        parts = []
        if self.description:
            parts.append(self.format_description(formatter))
        if self.option_list:
            parts.append(self.format_option_help(formatter))
        return "\n".join(parts)


class OptionGroup(OptionContainer):
    """A titled set of options shown together in the help."""

    def __init__(self, parser, title, description=None):
        self.parser = parser
        OptionContainer.__init__(self, parser.option_class,
                                 parser.conflict_handler, description)
        self.title = title

    def _create_option_list(self):
        self.option_list = []
        self._share_option_mappings(self.parser)

    def set_title(self, title):
        self.title = title

    def destroy(self):
        OptionContainer.destroy(self)
        del self.option_list

    def format_help(self, formatter):
        text = formatter.format_heading(self.title)
        formatter.indent()
        text += OptionContainer.format_help(self, formatter)
        formatter.dedent()
        return text


class OptionParser(OptionContainer):
    """Parses command lines.

    usage -- usage summary ("%prog" expands to the program name);
             SUPPRESS_USAGE hides it
    option_list -- options to add at once
    option_class -- class used by add_option(opt_str, ...)
    version -- version string (adds --version)
    conflict_handler -- "error" or "resolve"
    description, epilog -- text before / after the option help
    formatter -- a HelpFormatter (default IndentedHelpFormatter)
    add_help_option -- add -h/--help
    prog -- program name (default: basename of sys.argv[0])
    """

    standard_option_list = []

    def __init__(self,
                 usage=None,
                 option_list=None,
                 option_class=Option,
                 version=None,
                 conflict_handler="error",
                 description=None,
                 formatter=None,
                 add_help_option=True,
                 prog=None,
                 epilog=None):
        OptionContainer.__init__(self, option_class, conflict_handler,
                                 description)
        self.set_usage(usage)
        self.prog = prog
        self.version = version
        self.allow_interspersed_args = True
        self.process_default_values = True
        if formatter is None:
            formatter = IndentedHelpFormatter()
        self.formatter = formatter
        self.formatter.set_parser(self)
        self.epilog = epilog

        self._populate_option_list(option_list,
                                   add_help=add_help_option)
        self._init_parsing_state()

    def destroy(self):
        OptionContainer.destroy(self)
        for group in self.option_groups:
            group.destroy()
        del self.option_list
        del self.option_groups
        del self.formatter

    def _create_option_list(self):
        self.option_list = []
        self.option_groups = []
        self._create_option_mappings()

    def _add_help_option(self):
        self.add_option("-h", "--help",
                        action="help",
                        help=_("show this help message and exit"))

    def _add_version_option(self):
        self.add_option("--version",
                        action="version",
                        help=_("show program's version number and exit"))

    def _populate_option_list(self, option_list, add_help=True):
        if self.standard_option_list:
            self.add_options(self.standard_option_list)
        if option_list:
            self.add_options(option_list)
        if self.version:
            self._add_version_option()
        if add_help:
            self._add_help_option()

    def _init_parsing_state(self):
        # set by parse_args() for the benefit of callbacks
        self.rargs = None
        self.largs = None
        self.values = None

    # configuration

    def set_usage(self, usage):
        if usage is None:
            self.usage = _("%prog [options]")
        elif usage is SUPPRESS_USAGE:
            self.usage = None
        elif usage.lower().startswith("usage: "):
            # the formatter adds its own "Usage: "
            self.usage = usage[7:]
        else:
            self.usage = usage

    def enable_interspersed_args(self):
        """Allow positional arguments between options."""
        self.allow_interspersed_args = True

    def disable_interspersed_args(self):
        """Stop at the first positional argument."""
        self.allow_interspersed_args = False

    def set_process_default_values(self, process):
        self.process_default_values = process

    def set_default(self, dest, value):
        self.defaults[dest] = value

    def set_defaults(self, **kwargs):
        self.defaults.update(kwargs)

    def _get_all_options(self):
        options = self.option_list[:]
        for group in self.option_groups:
            options.extend(group.option_list)
        return options

    def get_default_values(self):
        if not self.process_default_values:
            return Values(self.defaults)
        defaults = self.defaults.copy()
        for option in self._get_all_options():
            value = defaults.get(option.dest)
            if isinstance(value, str):
                # string defaults go through the option's type conversion
                defaults[option.dest] = option.check_value(
                    option.get_opt_string(), value)
        return Values(defaults)

    # option groups

    def add_option_group(self, *args, **kwargs):
        if isinstance(args[0], str):
            group = OptionGroup(self, *args, **kwargs)
        elif len(args) == 1 and not kwargs:
            group = args[0]
            if not isinstance(group, OptionGroup):
                raise TypeError("not an OptionGroup instance: %r" % group)
            if group.parser is not self:
                raise ValueError("invalid OptionGroup (wrong parser)")
        else:
            raise TypeError("invalid arguments")
        self.option_groups.append(group)
        return group

    def get_option_group(self, opt_str):
        option = self._short_opt.get(opt_str) or self._long_opt.get(opt_str)
        if option and option.container is not self:
            return option.container
        return None

    # parsing

    def _get_args(self, args):
        if args is None:
            return sys.argv[1:]
        return args[:]

    def parse_args(self, args=None, values=None):
        """Parse *args* (default sys.argv[1:]); returns (values, args)
        where args are the leftover positional arguments."""
        rargs = self._get_args(args)
        if values is None:
            values = self.get_default_values()
        self.rargs = rargs
        self.largs = largs = []
        self.values = values
        try:
            self._process_args(largs, rargs, values)
        except (BadOptionError, OptionValueError) as err:
            self.error(str(err))
        return self.check_values(values, largs + rargs)

    def check_values(self, values, args):
        """Hook for subclasses to validate the result."""
        return (values, args)

    def _process_args(self, largs, rargs, values):
        while rargs:
            arg = rargs[0]
            if arg == "--":
                # end of options
                del rargs[0]
                return
            if arg[:2] == "--":
                self._process_long_opt(rargs, values)
            elif arg[:1] == "-" and len(arg) > 1:
                self._process_short_opts(rargs, values)
            elif self.allow_interspersed_args:
                largs.append(arg)
                del rargs[0]
            else:
                return

    def _match_long_opt(self, opt):
        return _match_abbrev(opt, self._long_opt)

    def _take_values(self, option, opt, rargs):
        nargs = option.nargs
        if len(rargs) < nargs:
            self.error(ngettext(
                "%(option)s option requires %(number)d argument",
                "%(option)s option requires %(number)d arguments",
                nargs) % {"option": opt, "number": nargs})
        if nargs == 1:
            return rargs.pop(0)
        value = tuple(rargs[:nargs])
        del rargs[:nargs]
        return value

    def _process_long_opt(self, rargs, values):
        arg = rargs.pop(0)
        explicit = "=" in arg
        if explicit:
            opt, value = arg.split("=", 1)
            rargs.insert(0, value)
        else:
            opt = arg
        opt = self._match_long_opt(opt)
        option = self._long_opt[opt]
        if option.takes_value():
            value = self._take_values(option, opt, rargs)
        elif explicit:
            self.error(_("%s option does not take a value") % opt)
        else:
            value = None
        option.process(opt, value, values, self)

    def _process_short_opts(self, rargs, values):
        arg = rargs.pop(0)
        for pos in range(1, len(arg)):
            opt = "-" + arg[pos]
            option = self._short_opt.get(opt)
            if not option:
                raise BadOptionError(opt)
            if option.takes_value():
                # the rest of the cluster is the (first) value
                rest = arg[pos + 1:]
                if rest:
                    rargs.insert(0, rest)
                value = self._take_values(option, opt, rargs)
                option.process(opt, value, values, self)
                break
            option.process(opt, None, values, self)

    # output

    def get_prog_name(self):
        if self.prog is None:
            return os.path.basename(sys.argv[0])
        return self.prog

    def expand_prog_name(self, s):
        return s.replace("%prog", self.get_prog_name())

    def get_description(self):
        return self.expand_prog_name(self.description)

    def exit(self, status=0, msg=None):
        if msg:
            sys.stderr.write(msg)
        sys.exit(status)

    def error(self, msg):
        """Print usage and *msg* to stderr and exit with status 2."""
        self.print_usage(sys.stderr)
        self.exit(2, "%s: error: %s\n" % (self.get_prog_name(), msg))

    def get_usage(self):
        if self.usage:
            return self.formatter.format_usage(
                self.expand_prog_name(self.usage))
        return ""

    def print_usage(self, file=None):
        if self.usage:
            print(self.get_usage(), file=file)

    def get_version(self):
        if self.version:
            return self.expand_prog_name(self.version)
        return ""

    def print_version(self, file=None):
        if self.version:
            print(self.get_version(), file=file)

    def format_option_help(self, formatter=None):
        if formatter is None:
            formatter = self.formatter
        formatter.store_option_strings(self)
        if not (self.option_list or self.option_groups):
            return ""
        sections = []
        heading = formatter.format_heading(_("Options"))
        formatter.indent()
        if self.option_list:
            sections.append(OptionContainer.format_option_help(self, formatter))
        for group in self.option_groups:
            sections.append(group.format_help(formatter))
        formatter.dedent()
        return heading + "\n".join(sections)

    def format_epilog(self, formatter):
        return formatter.format_epilog(self.epilog)

    def format_help(self, formatter=None):
        if formatter is None:
            formatter = self.formatter
        parts = []
        if self.usage:
            parts.append(self.get_usage() + "\n")
        if self.description:
            parts.append(self.format_description(formatter) + "\n")
        parts.append(self.format_option_help(formatter))
        parts.append(self.format_epilog(formatter))
        return "".join(parts)

    def print_help(self, file=None):
        if file is None:
            file = sys.stdout
        file.write(self.format_help())


def _match_abbrev(s, wordmap):
    """Return the key of *wordmap* that *s* names exactly or as an unique
    prefix; BadOptionError / AmbiguousOptionError otherwise."""
    if s in wordmap:
        return s
    matches = sorted(word for word in wordmap if word.startswith(s))
    if len(matches) == 1:
        return matches[0]
    if not matches:
        raise BadOptionError(s)
    raise AmbiguousOptionError(s, matches)


# Older name of Option.
make_option = Option
