"""Terminal colour support shared by the standard library.

Provides the ANSI escape codes, the decision whether output to a given
stream should be coloured (honouring PYTHON_COLORS, NO_COLOR, FORCE_COLOR
and TERM) and the colour *theme*: one section of named colours per
consumer (traceback, argparse, unittest, ...).
"""

import os
import sys

from collections.abc import Mapping
from dataclasses import dataclass, field, fields

# alias: some section fields shadow builtin names inside class bodies
_Str = str

# Cleared to switch colouring off globally (e.g. by tests).
COLORIZE = True


class ANSIColors:
    RESET = '\x1b[0m'

    BLACK = '\x1b[30m'
    BLUE = '\x1b[34m'
    CYAN = '\x1b[36m'
    GREEN = '\x1b[32m'
    GREY = '\x1b[90m'
    MAGENTA = '\x1b[35m'
    RED = '\x1b[31m'
    WHITE = '\x1b[37m'
    YELLOW = '\x1b[33m'

    BOLD = '\x1b[1m'
    BOLD_BLACK = '\x1b[1;30m'
    BOLD_BLUE = '\x1b[1;34m'
    BOLD_CYAN = '\x1b[1;36m'
    BOLD_GREEN = '\x1b[1;32m'
    BOLD_MAGENTA = '\x1b[1;35m'
    BOLD_RED = '\x1b[1;31m'
    BOLD_WHITE = '\x1b[1;37m'
    BOLD_YELLOW = '\x1b[1;33m'

    INTENSE_BLACK = '\x1b[90m'
    INTENSE_BLUE = '\x1b[94m'
    INTENSE_CYAN = '\x1b[96m'
    INTENSE_GREEN = '\x1b[92m'
    INTENSE_MAGENTA = '\x1b[95m'
    INTENSE_RED = '\x1b[91m'
    INTENSE_WHITE = '\x1b[97m'
    INTENSE_YELLOW = '\x1b[93m'

    BACKGROUND_BLACK = '\x1b[40m'
    BACKGROUND_BLUE = '\x1b[44m'
    BACKGROUND_CYAN = '\x1b[46m'
    BACKGROUND_GREEN = '\x1b[42m'
    BACKGROUND_MAGENTA = '\x1b[45m'
    BACKGROUND_RED = '\x1b[41m'
    BACKGROUND_WHITE = '\x1b[47m'
    BACKGROUND_YELLOW = '\x1b[43m'

    INTENSE_BACKGROUND_BLACK = '\x1b[100m'
    INTENSE_BACKGROUND_BLUE = '\x1b[104m'
    INTENSE_BACKGROUND_CYAN = '\x1b[106m'
    INTENSE_BACKGROUND_GREEN = '\x1b[102m'
    INTENSE_BACKGROUND_MAGENTA = '\x1b[105m'
    INTENSE_BACKGROUND_RED = '\x1b[101m'
    INTENSE_BACKGROUND_WHITE = '\x1b[107m'
    INTENSE_BACKGROUND_YELLOW = '\x1b[103m'


# All escape codes, longest first so that removal never leaves fragments.
ColorCodes = sorted({value for name, value in vars(ANSIColors).items()
                     if not name.startswith('_')},
                    key=len, reverse=True)


class NoColors:
    """ANSIColors look-alike whose codes are all empty strings."""


for _name in vars(ANSIColors):
    if not _name.startswith('_'):
        setattr(NoColors, _name, '')
del _name


def get_colors(colorize=False, *, file=None):
    """ANSIColors if output to *file* should be coloured, else NoColors."""
    if colorize or can_colorize(file=file):
        return ANSIColors()
    return NoColors


def decolor(text):
    """Remove the ANSI colour codes from *text*."""
    for code in ColorCodes:
        text = text.replace(code, '')
    return text


def can_colorize(*, file=None):
    """Should output written to *file* (default: sys.stdout) use colour?"""
    if file is None:
        file = sys.stdout

    environ = os.environ
    if not sys.flags.ignore_environment:
        python_colors = environ.get('PYTHON_COLORS')
        if python_colors == '0':
            return False
        if python_colors == '1':
            return True
    if environ.get('NO_COLOR'):
        return False
    if not COLORIZE:
        return False
    if environ.get('FORCE_COLOR'):
        return True
    if environ.get('TERM') == 'dumb':
        return False

    if not hasattr(file, 'fileno'):
        return False

    if sys.platform == 'win32':
        try:
            import nt
            if not nt._supports_virtual_terminal():
                return False
        except (ImportError, AttributeError):
            return False

    try:
        return os.isatty(file.fileno())
    except OSError:
        # includes io.UnsupportedOperation: no real file descriptor
        return hasattr(file, 'isatty') and file.isatty()


# ---------------------------------------------------------------------------
# Themes
# ---------------------------------------------------------------------------

@dataclass(frozen=True, kw_only=True)
class ThemeSection(Mapping[str, str]):
    """A named group of colours for one consumer.

    Sections are immutable mappings from colour names to escape codes.
    """

    def __post_init__(self):
        values = {f.name: getattr(self, f.name) for f in fields(self)}
        object.__setattr__(self, '_ThemeSection__values', values)

    def copy_with(self, **kwargs):
        """A copy of this section with some colours replaced."""
        merged = dict(self.__values)
        merged.update(kwargs)
        return type(self)(**merged)

    @classmethod
    def no_colors(cls):
        """An instance of this section with every colour empty."""
        return cls(**{f.name: '' for f in fields(cls)})

    def __getitem__(self, key):
        return self.__values[key]

    def __len__(self):
        return len(self.__values)

    def __iter__(self):
        return iter(self.__values)


@dataclass(frozen=True, kw_only=True)
class Argparse(ThemeSection):
    usage: str = ANSIColors.BOLD_BLUE
    prog: str = ANSIColors.BOLD_MAGENTA
    prog_extra: str = ANSIColors.MAGENTA
    heading: str = ANSIColors.BOLD_BLUE
    summary_long_option: str = ANSIColors.CYAN
    summary_short_option: str = ANSIColors.GREEN
    summary_label: str = ANSIColors.YELLOW
    summary_action: str = ANSIColors.GREEN
    long_option: str = ANSIColors.BOLD_CYAN
    short_option: str = ANSIColors.BOLD_GREEN
    label: str = ANSIColors.BOLD_YELLOW
    action: str = ANSIColors.BOLD_GREEN
    default: str = ANSIColors.GREY
    interpolated_value: str = ANSIColors.YELLOW
    message: str = ANSIColors.MAGENTA
    error: str = ANSIColors.BOLD_MAGENTA
    warning: str = ANSIColors.BOLD_YELLOW
    reset: str = ANSIColors.RESET


@dataclass(frozen=True, kw_only=True)
class Difflib(ThemeSection):
    added: str = ANSIColors.GREEN
    context: str = ANSIColors.RESET
    header: str = ANSIColors.BOLD
    hunk: str = ANSIColors.CYAN
    removed: str = ANSIColors.RED
    reset: str = ANSIColors.RESET


@dataclass(frozen=True, kw_only=True)
class FancyCompleter(ThemeSection):
    module: _Str = ANSIColors.BLUE
    bool: _Str = ANSIColors.BOLD_CYAN
    type: _Str = ANSIColors.BOLD_MAGENTA
    none: _Str = ANSIColors.GREY
    str: _Str = ANSIColors.GREEN
    bytes: _Str = ANSIColors.BOLD_GREEN
    int: _Str = ANSIColors.BOLD_YELLOW
    float: _Str = ANSIColors.BOLD_YELLOW
    complex: _Str = ANSIColors.BOLD_YELLOW
    function: _Str = ANSIColors.BOLD_BLUE
    builtin_function: _Str = ANSIColors.BOLD_BLUE
    method: _Str = ANSIColors.CYAN
    builtin_method: _Str = ANSIColors.CYAN
    exception: _Str = ANSIColors.BOLD_RED
    reset: _Str = ANSIColors.RESET


@dataclass(frozen=True, kw_only=True)
class HttpServer(ThemeSection):
    error: str = ANSIColors.YELLOW
    path: str = ANSIColors.CYAN
    serving: str = ANSIColors.GREEN
    size: str = ANSIColors.GREY
    status_informational: str = ANSIColors.RESET
    status_ok: str = ANSIColors.GREEN
    status_redirect: str = ANSIColors.INTENSE_CYAN
    status_client_error: str = ANSIColors.YELLOW
    status_server_error: str = ANSIColors.RED
    timestamp: str = ANSIColors.GREY
    url: str = ANSIColors.CYAN
    reset: str = ANSIColors.RESET


@dataclass(frozen=True, kw_only=True)
class Syntax(ThemeSection):
    prompt: str = ANSIColors.BOLD_MAGENTA
    keyword: str = ANSIColors.BOLD_BLUE
    builtin: str = ANSIColors.CYAN
    comment: str = ANSIColors.RED
    string: str = ANSIColors.GREEN
    number: str = ANSIColors.YELLOW
    op: str = ANSIColors.RESET
    definition: str = ANSIColors.BOLD
    soft_keyword: str = ANSIColors.BOLD_BLUE
    reset: str = ANSIColors.RESET


@dataclass(frozen=True, kw_only=True)
class ProfilerDump(ThemeSection):
    header: str = ANSIColors.BOLD_MAGENTA
    status: str = ANSIColors.CYAN
    filename: str = ANSIColors.MAGENTA
    line_no: str = ANSIColors.MAGENTA
    frame: str = ANSIColors.MAGENTA
    opcode: str = ANSIColors.GREY
    source_highlight: str = ANSIColors.BOLD_RED
    reset: str = ANSIColors.RESET


@dataclass(frozen=True, kw_only=True)
class Timeit(ThemeSection):
    timing: str = ANSIColors.CYAN
    best: str = ANSIColors.GREEN
    per_loop: str = ANSIColors.GREEN
    punctuation: str = ANSIColors.GREY
    reset: str = ANSIColors.RESET


@dataclass(frozen=True, kw_only=True)
class Traceback(ThemeSection):
    type: str = ANSIColors.BOLD_MAGENTA
    message: str = ANSIColors.MAGENTA
    filename: str = ANSIColors.MAGENTA
    line_no: str = ANSIColors.MAGENTA
    frame: str = ANSIColors.MAGENTA
    error_highlight: str = ANSIColors.BOLD_RED
    error_range: str = ANSIColors.RED
    note: str = ANSIColors.CYAN
    reset: str = ANSIColors.RESET


@dataclass(frozen=True, kw_only=True)
class Unittest(ThemeSection):
    passed: str = ANSIColors.GREEN
    warn: str = ANSIColors.YELLOW
    fail: str = ANSIColors.RED
    fail_info: str = ANSIColors.BOLD_RED
    reset: str = ANSIColors.RESET


@dataclass(frozen=True, kw_only=True)
class Theme:
    """All colour sections; customise with copy_with() and set_theme()."""

    argparse: Argparse = field(default_factory=Argparse)
    difflib: Difflib = field(default_factory=Difflib)
    fancycompleter: FancyCompleter = field(default_factory=FancyCompleter)
    http_server: HttpServer = field(default_factory=HttpServer)
    profiler_dump: ProfilerDump = field(default_factory=ProfilerDump)
    syntax: Syntax = field(default_factory=Syntax)
    timeit: Timeit = field(default_factory=Timeit)
    traceback: Traceback = field(default_factory=Traceback)
    unittest: Unittest = field(default_factory=Unittest)

    def copy_with(self, **sections):
        """A copy of the theme with the given sections replaced (None
        keeps the current section)."""
        current = {f.name: getattr(self, f.name) for f in fields(self)}
        for name, section in sections.items():
            if name not in current:
                raise TypeError(f'unknown theme section: {name!r}')
            if section is not None:
                current[name] = section
        return type(self)(**current)

    def no_colors(self):
        """A theme of the same shape without any colours."""
        return type(self)(**{f.name: getattr(self, f.name).no_colors()
                             for f in fields(self)})


def get_theme(*, tty_file=None, force_color=False, force_no_color=False):
    """The theme to use for output to *tty_file*: the current theme if
    colouring applies (or is forced), else a colourless one."""
    if force_color or (not force_no_color and can_colorize(file=tty_file)):
        return _theme
    return theme_no_color


def set_theme(t):
    """Make *t* (a Theme) the current theme."""
    global _theme
    if not isinstance(t, Theme):
        raise ValueError(f'Expected Theme object, found {t}')
    _theme = t


default_theme = Theme()
theme_no_color = default_theme.no_colors()
_theme = default_theme
