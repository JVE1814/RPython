"""Command-line parsing library.

Build an ArgumentParser, describe the arguments the program accepts with
add_argument() (positional arguments, options, flags, sub-commands, ...)
and call parse_args() to turn sys.argv into a Namespace:

    parser = argparse.ArgumentParser(description='Sum some integers.')
    parser.add_argument('integers', metavar='N', type=int, nargs='+',
                        help='an integer to add')
    parser.add_argument('--verbose', action='store_true')
    args = parser.parse_args()

Usage and help messages are generated from the argument descriptions;
invalid command lines produce an error message and exit status 2.

Public names: ArgumentParser, ArgumentError, ArgumentTypeError,
BooleanOptionalAction, FileType, HelpFormatter, ArgumentDefaultsHelpFormatter,
RawDescriptionHelpFormatter, RawTextHelpFormatter, MetavarTypeHelpFormatter,
Namespace, Action, and the constants ONE_OR_MORE, OPTIONAL, PARSER,
REMAINDER, SUPPRESS and ZERO_OR_MORE.
"""

__version__ = '1.1'
__all__ = [
    'ArgumentParser',
    'ArgumentError',
    'ArgumentTypeError',
    'BooleanOptionalAction',
    'FileType',
    'HelpFormatter',
    'ArgumentDefaultsHelpFormatter',
    'RawDescriptionHelpFormatter',
    'RawTextHelpFormatter',
    'MetavarTypeHelpFormatter',
    'Namespace',
    'Action',
    'ONE_OR_MORE',
    'OPTIONAL',
    'PARSER',
    'REMAINDER',
    'SUPPRESS',
    'ZERO_OR_MORE',
]


import os as _os
import re as _re
import sys as _sys

from gettext import gettext as _, ngettext

SUPPRESS = '==SUPPRESS=='

OPTIONAL = '?'
ZERO_OR_MORE = '*'
ONE_OR_MORE = '+'
PARSER = 'A...'
REMAINDER = '...'
_UNRECOGNIZED_ARGS_ATTR = '_unrecognized_args'


# ---------------------------------------------------------------------------
# Small helpers
# ---------------------------------------------------------------------------

class _AttributeHolder(object):
    """Gives a class a repr listing its (keyword) attributes.

    _get_kwargs() and _get_args() say what to show."""

    def __repr__(self):
        type_name = type(self).__name__
        pieces = [repr(arg) for arg in self._get_args()]
        unnamed = {}
        for name, value in self._get_kwargs():
            if name.isidentifier():
                pieces.append('%s=%r' % (name, value))
            else:
                unnamed[name] = value
        if unnamed:
            pieces.append('**%s' % repr(unnamed))
        return '%s(%s)' % (type_name, ', '.join(pieces))

    def _get_kwargs(self):
        return list(self.__dict__.items())

    def _get_args(self):
        return []


def _copy_items(items):
    """A copy of a list attribute value (None becomes [])."""
    if items is None:
        return []
    if type(items) is list:
        # the common case, without importing copy
        return items[:]
    import copy
    return copy.copy(items)


def _identity(value):
    return value


def _is_long_option(option):
    # "--name", "++name", "-+name": a second prefix character
    return len(option) > 1 and not option[1].isalnum()


class _ColorlessTheme:
    """Stand-in for an argparse colour theme: every colour is ''."""

    def __getattr__(self, name):
        if name.startswith('_'):
            raise AttributeError(name)
        return ''


_colorless_theme = _ColorlessTheme()


def _prog_name(prog=None):
    """The program name shown in messages: the script name, or
    'python -m module' / 'python dir' as appropriate."""
    if prog is not None:
        return prog
    arg0 = _sys.argv[0]
    try:
        spec = _sys.modules['__main__'].__spec__
    except (KeyError, AttributeError):
        spec = None
    if spec is None:
        return _os.path.basename(arg0)
    python = _os.path.basename(_sys.executable)
    if spec.name == '__main__':
        # a directory or zip file given on the command line
        return f'{python} {arg0}'
    origin = getattr(spec, 'origin', None)
    if origin is not None and arg0 not in ('-m', '-c') and \
            _os.path.abspath(arg0) != _os.path.abspath(origin):
        # run as __main__ by other means (sys.argv untouched)
        return _os.path.basename(arg0)
    module = spec.name.removesuffix('.__main__')
    return f'{python} -m {module}'


# ---------------------------------------------------------------------------
# Help formatting
# ---------------------------------------------------------------------------

class HelpFormatter(object):
    """Formats usage and help messages.

    Only the name of this class is considered a public API; the methods are
    implementation details (although subclasses commonly override
    _fill_text, _split_lines, _get_help_string and the metavar hooks).
    """

    def __init__(self, prog, indent_increment=2, max_help_position=24,
                 width=None):
        if width is None:
            import shutil
            width = shutil.get_terminal_size().columns
            width -= 2

        self._prog = prog
        self._indent_increment = indent_increment
        self._max_help_position = min(max_help_position,
                                      max(width - 20, indent_increment * 2))
        self._width = width
        self._current_indent = 0
        self._level = 0
        self._action_max_length = 0

        self._root_section = self._Section(self, None)
        self._current_section = self._root_section

        self._whitespace_matcher = _re.compile(r'\s+', _re.ASCII)
        self._long_break_matcher = _re.compile(r'\n\n\n+')

        self._set_color(False)

    def _set_color(self, color, *, file=None):
        if color:
            from _colorize import can_colorize, decolor, get_theme
            if can_colorize(file=file):
                self._theme = get_theme(force_color=True).argparse
                self._decolor = decolor
                return
        self._theme = _colorless_theme
        self._decolor = _identity

    # -- sections and indentation ------------------------------------------------

    def _indent(self):
        self._current_indent += self._indent_increment
        self._level += 1

    def _dedent(self):
        self._current_indent -= self._indent_increment
        assert self._current_indent >= 0, 'Indent decreased below 0.'
        self._level -= 1

    class _Section(object):

        def __init__(self, formatter, parent, heading=None):
            self.formatter = formatter
            self.parent = parent
            self.heading = heading
            self.items = []

        def format_help(self):
            formatter = self.formatter
            if self.parent is not None:
                formatter._indent()
            body = formatter._join_parts([func(*args)
                                          for func, args in self.items])
            if self.parent is not None:
                formatter._dedent()
            if not body:
                # empty sections are left out entirely
                return ''
            if self.heading is not SUPPRESS and self.heading is not None:
                theme = formatter._theme
                text = _('%(heading)s:') % dict(heading=self.heading)
                indent = ' ' * formatter._current_indent
                heading = f'{indent}{theme.heading}{text}{theme.reset}\n'
            else:
                heading = ''
            return formatter._join_parts(['\n', heading, body, '\n'])

    def _add_item(self, func, args):
        self._current_section.items.append((func, args))

    # -- adding content ----------------------------------------------------------

    def start_section(self, heading):
        self._indent()
        section = self._Section(self, self._current_section, heading)
        self._add_item(section.format_help, [])
        self._current_section = section

    def end_section(self):
        self._current_section = self._current_section.parent
        self._dedent()

    def add_text(self, text):
        if text is not SUPPRESS and text is not None:
            self._add_item(self._format_text, [text])

    def add_usage(self, usage, actions, groups, prefix=None):
        if usage is not SUPPRESS:
            self._add_item(self._format_usage, [usage, actions, groups,
                                                prefix])

    def add_argument(self, action):
        if action.help is SUPPRESS:
            return
        # the widest invocation (with indentation) decides where help starts
        invocation = self._format_action_invocation
        widths = [len(self._decolor(invocation(action)))
                  + self._current_indent]
        for subaction in self._iter_indented_subactions(action):
            widths.append(len(self._decolor(invocation(subaction)))
                          + self._current_indent)
        self._action_max_length = max(self._action_max_length, max(widths))
        self._add_item(self._format_action, [action])

    def add_arguments(self, actions):
        for action in actions:
            self.add_argument(action)

    # -- producing the text ---------------------------------------------------------

    def format_help(self):
        text = self._root_section.format_help()
        if text:
            text = self._long_break_matcher.sub('\n\n', text)
            text = text.strip('\n') + '\n'
        return text

    def _join_parts(self, part_strings):
        return ''.join(part for part in part_strings
                       if part and part is not SUPPRESS)

    # -- usage -------------------------------------------------------------------------

    def _format_usage(self, usage, actions, groups, prefix):
        theme = self._theme
        if prefix is None:
            prefix = _('usage: ')

        if usage is not None:
            # a user supplied usage string; %(prog)s is highlighted
            prog = f'{theme.prog}{self._prog}{theme.reset}{theme.prog_extra}'
            usage = theme.prog_extra + usage % {'prog': prog} + theme.reset
        elif not actions:
            usage = f'{theme.prog}{self._prog}{theme.reset}'
        else:
            usage = self._build_usage(actions, groups, prefix)

        return f'{theme.usage}{prefix}{theme.reset}{usage}\n\n'

    def _build_usage(self, actions, groups, prefix):
        theme = self._theme
        prog = '%(prog)s' % dict(prog=self._prog)

        # optionals first, then positionals
        optionals = [a for a in actions if a.option_strings]
        positionals = [a for a in actions if not a.option_strings]
        parts, pos_start = self._get_actions_usage_parts(
            optionals + positionals, groups)

        text = ' '.join(filter(None, [prog, *parts]))
        width = self._width - self._current_indent
        if len(prefix) + len(self._decolor(text)) > width:
            text = '\n'.join(self._wrap_usage(prog, parts[:pos_start],
                                              parts[pos_start:], prefix,
                                              width))
        text = text.removeprefix(prog)
        return f'{theme.prog}{prog}{theme.reset}{text}'

    def _wrap_usage(self, prog, opt_parts, pos_parts, prefix, width):
        measure = self._decolor

        def lines_of(parts, indent, prefix=None):
            lines = []
            line = []
            if prefix is not None:
                used = len(prefix) - 1
            else:
                used = len(indent) - 1
            for part in parts:
                size = len(measure(part))
                if line and used + 1 + size > width:
                    lines.append(indent + ' '.join(line))
                    line = []
                    used = len(indent) - 1
                line.append(part)
                used += size + 1
            if line:
                lines.append(indent + ' '.join(line))
            if prefix is not None:
                lines[0] = lines[0][len(indent):]
            return lines

        prog_size = len(measure(prog))
        if len(prefix) + prog_size <= 0.75 * width:
            # short program name: continuation lines line up after it
            indent = ' ' * (len(prefix) + prog_size + 1)
            if opt_parts:
                lines = lines_of([prog] + opt_parts, indent, prefix)
                lines.extend(lines_of(pos_parts, indent))
            elif pos_parts:
                lines = lines_of([prog] + pos_parts, indent, prefix)
            else:
                lines = [prog]
        else:
            # long program name: it gets a line of its own
            indent = ' ' * len(prefix)
            lines = lines_of(opt_parts + pos_parts, indent)
            if len(lines) > 1:
                lines = (lines_of(opt_parts, indent) +
                         lines_of(pos_parts, indent))
            lines = [prog] + lines
        return lines

    def _get_actions_usage_parts(self, actions, groups):
        """The usage text of each action (mutually exclusive groups
        bracketed together), and the index of the first positional part."""
        # find the groups whose actions appear together
        in_groups = set()
        spans = {}
        for group in groups:
            if not group._group_actions:
                raise ValueError(f'empty group {group}')
            if all(a.help is SUPPRESS for a in group._group_actions):
                continue
            try:
                start = min(actions.index(a) for a in group._group_actions)
            except ValueError:
                continue
            end = start + len(group._group_actions)
            if set(actions[start:end]) == set(group._group_actions):
                in_groups.update(group._group_actions)
                spans[start, end] = group

        theme = self._theme
        parts = []
        for action in actions:
            if action.help is SUPPRESS:
                parts.append(None)
                continue
            if not action.option_strings:
                default = self._get_default_metavar_for_positional(action)
                text = self._format_args(action, default)
                # the brackets of an optional positional in a group go
                if action in in_groups and len(text) > 1 and \
                        text[0] == '[' and text[-1] == ']':
                    text = text[1:-1]
                parts.append(f'{theme.summary_action}{text}{theme.reset}')
                continue
            option = action.option_strings[0]
            if action.nargs == 0:
                text = action.format_usage()
                text = self._color_summary_options(text)
            else:
                default = self._get_default_metavar_for_optional(action)
                args = self._format_args(action, default)
                color = self._summary_option_color(option)
                text = (f'{color}{option} {theme.summary_label}{args}'
                        f'{theme.reset}')
            if not action.required and action not in in_groups:
                text = f'[{text}]'
            parts.append(text)

        separated = set()
        for start, end in sorted(spans, reverse=True):
            group = spans[start, end]
            members = [p for p in parts[start:end] if p is not None]
            if group.required:
                opening, closing = '()' if len(members) > 1 else ('', '')
            else:
                opening, closing = '[]'
            members[0] = opening + members[0]
            members[-1] = members[-1] + closing
            for index in range(start, start + len(members) - 1):
                # separators are only added once (nested groups)
                if index not in separated:
                    separated.add(index)
                    members[index - start] += ' |'
            parts[start:end] = members

        optional_count = sum(1 for a in actions if a.option_strings)
        visible_optionals = 0
        for action in actions[:optional_count]:
            if action.help is not SUPPRESS:
                visible_optionals += 1
        cleaned = [p for p in parts if p is not None]
        return cleaned, min(visible_optionals, len(cleaned))

    def _summary_option_color(self, option):
        theme = self._theme
        if _is_long_option(option):
            return theme.summary_long_option
        return theme.summary_short_option

    def _color_summary_options(self, text):
        theme = self._theme
        if theme is _colorless_theme or not theme.reset:
            return text
        pieces = []
        for word in text.split(' | '):
            pieces.append(f'{self._summary_option_color(word)}{word}'
                          f'{theme.reset}')
        return ' | '.join(pieces)

    # -- text paragraphs -----------------------------------------------------------------

    def _format_text(self, text):
        if '%(prog)' in text:
            text = text % dict(prog=self._prog)
        width = max(self._width - self._current_indent, 11)
        indent = ' ' * self._current_indent
        filled = self._fill_text(text, width, indent)
        return self._apply_markup(filled) + '\n\n'

    def _apply_markup(self, text):
        """Show `literal` and ``literal`` spans highlighted (colour only)."""
        theme = self._theme
        if not theme.reset or '`' not in text:
            return text
        return _re.sub(r'``(.+?)``|`([^`]+?)`',
                       lambda m: (f'{theme.prog_extra}'
                                  f'{m.group(1) or m.group(2)}{theme.reset}'),
                       text, flags=_re.DOTALL)

    # -- the action list -------------------------------------------------------------------

    def _format_action(self, action):
        help_position = min(self._action_max_length + 2,
                            self._max_help_position)
        help_width = max(self._width - help_position, 11)
        action_width = help_position - self._current_indent - 2
        header = self._format_action_invocation(action)
        header_size = len(self._decolor(header))
        indent = ' ' * self._current_indent

        if not action.help:
            header = f'{indent}{header}\n'
        elif header_size <= action_width:
            # short enough: the help starts on the same line
            padding = ' ' * (action_width - header_size)
            header = f'{indent}{header}{padding}  '
            first_indent = 0
        else:
            first_indent = help_position
            header = f'{indent}{header}\n'

        parts = [header]
        if action.help and action.help.strip():
            help_text = self._expand_help(action)
            if help_text:
                lines = self._split_help(help_text, help_width)
                parts.append(' ' * first_indent + lines[0] + '\n')
                for line in lines[1:]:
                    parts.append(' ' * help_position + line + '\n')
        elif not header.endswith('\n'):
            parts.append('\n')

        for subaction in self._iter_indented_subactions(action):
            parts.append(self._format_action(subaction))
        return self._join_parts(parts)

    def _split_help(self, text, width):
        """Split help text into lines, deciding the line breaks on the text
        without colours and re-applying the colours afterwards."""
        plain = self._decolor(text)
        if plain == text:
            return [self._apply_markup(line)
                    for line in self._split_lines(text, width)]
        lines = self._split_lines(plain, width)
        return [self._apply_markup(line)
                for line in _recolor_lines(text, plain, lines)]

    def _format_action_invocation(self, action):
        theme = self._theme
        if not action.option_strings:
            default = self._get_default_metavar_for_positional(action)
            metavar = ' '.join(self._metavar_formatter(action, default)(1))
            return f'{theme.action}{metavar}{theme.reset}'

        options = ', '.join(self._color_option(option)
                            for option in action.option_strings)
        if action.nargs == 0:
            return options
        default = self._get_default_metavar_for_optional(action)
        args = self._format_args(action, default)
        return f'{options} {theme.label}{args}{theme.reset}'

    def _color_option(self, option):
        theme = self._theme
        if _is_long_option(option):
            return f'{theme.long_option}{option}{theme.reset}'
        return f'{theme.short_option}{option}{theme.reset}'

    def _metavar_formatter(self, action, default_metavar):
        if action.metavar is not None:
            result = action.metavar
        elif action.choices is not None:
            result = '{%s}' % ','.join(map(str, action.choices))
        else:
            result = default_metavar

        def format(tuple_size):
            if isinstance(result, tuple):
                return result
            return (result,) * tuple_size
        return format

    def _format_args(self, action, default_metavar):
        get_metavar = self._metavar_formatter(action, default_metavar)
        nargs = action.nargs
        if nargs is None:
            return '%s' % get_metavar(1)
        if nargs == OPTIONAL:
            return '[%s]' % get_metavar(1)
        if nargs == ZERO_OR_MORE:
            metavar = get_metavar(1)
            if len(metavar) == 2:
                return '[%s [%s ...]]' % metavar
            return '[%s ...]' % metavar
        if nargs == ONE_OR_MORE:
            return '%s [%s ...]' % get_metavar(2)
        if nargs == REMAINDER:
            return '...'
        if nargs == PARSER:
            return '%s ...' % get_metavar(1)
        if nargs == SUPPRESS:
            return ''
        try:
            formats = ['%s'] * nargs
        except TypeError:
            raise ValueError('invalid nargs value') from None
        return ' '.join(formats) % get_metavar(nargs)

    def _expand_help(self, action):
        help_string = self._get_help_string(action)
        params = dict(vars(action), prog=self._prog)
        for name in list(params):
            value = params[name]
            if value is SUPPRESS:
                del params[name]
            elif hasattr(value, '__name__'):
                params[name] = value.__name__
        if params.get('choices') is not None:
            params['choices'] = ', '.join(str(c) for c in params['choices'])

        theme = self._theme
        if not theme.reset:
            return help_string % params
        return _interpolate_colored(help_string, params, theme)

    def _iter_indented_subactions(self, action):
        try:
            get_subactions = action._get_subactions
        except AttributeError:
            pass
        else:
            self._indent()
            yield from get_subactions()
            self._dedent()

    def _split_lines(self, text, width):
        text = self._whitespace_matcher.sub(' ', text).strip()
        # textwrap is only imported when help is actually needed
        import textwrap
        return textwrap.wrap(text, width)

    def _fill_text(self, text, width, indent):
        text = self._whitespace_matcher.sub(' ', text).strip()
        import textwrap
        return textwrap.fill(text, width, initial_indent=indent,
                             subsequent_indent=indent)

    def _get_help_string(self, action):
        return action.help

    def _get_default_metavar_for_optional(self, action):
        return action.dest.upper()

    def _get_default_metavar_for_positional(self, action):
        return action.dest


# The colour codes around an interpolated value and the "(default: ...)"
# suffix added by ArgumentDefaultsHelpFormatter.
_DEFAULT_SUFFIX = ' (default: %(default)s)'

_conversion_pattern = _re.compile(
    r'%(?:\((?P<key>[^)]*)\)(?P<spec>[-#0 +]*\*?\d*(?:\.\d+)?[a-zA-Z%])'
    r'|%)')


def _interpolate_colored(help_string, params, theme):
    """help_string % params, with each interpolated value highlighted and
    a trailing default suffix shown in the 'default' colour."""
    suffix = ''
    text = str(help_string)
    if isinstance(help_string, _DefaultedHelp):
        text = text[:-len(_DEFAULT_SUFFIX)]
        value = ('%(default)s' % params)
        suffix = (f' {theme.default}(default: {theme.reset}'
                  f'{theme.interpolated_value}{value}{theme.reset}'
                  f'{theme.default}){theme.reset}')

    def substitute(match):
        key = match.group('key')
        if key is None:
            # '%%'
            return '%'
        spec = match.group('spec')
        if spec == '%':
            return '%'
        value = ('%(' + key + ')' + spec) % params
        return f'{theme.interpolated_value}{value}{theme.reset}'

    # validate like the plain % operator would (raises on bad formats)
    help_string % params
    return _conversion_pattern.sub(substitute, text) + suffix


def _recolor_lines(colored, plain, lines):
    """Re-apply the colours of *colored* (which is *plain* plus escape
    sequences) to *lines*, the result of wrapping *plain*.

    Wrapping only drops or collapses whitespace, so the characters of the
    lines can be matched up with those of the plain text in order."""
    styles = []
    current = ''
    i = 0
    while i < len(colored):
        if colored[i] == '\x1b':
            end = colored.find('m', i)
            if end < 0:
                break
            code = colored[i:end + 1]
            current = '' if code == '\x1b[0m' else code
            i = end + 1
            continue
        styles.append(current)
        i += 1

    result = []
    pos = 0
    for line in lines:
        pieces = []
        active = ''
        for ch in line:
            if ch.isspace() and pos < len(plain) and plain[pos].isspace():
                style = styles[pos]
                pos += 1
            else:
                # whitespace dropped or collapsed by the wrapper
                while (pos < len(plain) and plain[pos].isspace()
                       and plain[pos] != ch):
                    pos += 1
                style = styles[pos] if pos < len(styles) else ''
                pos += 1
            if style != active:
                if active:
                    pieces.append('\x1b[0m')
                if style:
                    pieces.append(style)
                active = style
            pieces.append(ch)
        if active:
            pieces.append('\x1b[0m')
        result.append(''.join(pieces))
    return result


class _DefaultedHelp(str):
    """Help text to which ArgumentDefaultsHelpFormatter appended the
    default value suffix."""


class RawDescriptionHelpFormatter(HelpFormatter):
    """Help message formatter which retains any formatting in descriptions.

    Only the name of this class is considered a public API.
    """

    def _fill_text(self, text, width, indent):
        return ''.join(indent + line for line in text.splitlines(keepends=True))


class RawTextHelpFormatter(RawDescriptionHelpFormatter):
    """Help message formatter which retains formatting of all help text.

    Only the name of this class is considered a public API.
    """

    def _split_lines(self, text, width):
        return text.splitlines()


class ArgumentDefaultsHelpFormatter(HelpFormatter):
    """Help message formatter which adds default values to argument help.

    Only the name of this class is considered a public API.
    """

    def _get_help_string(self, action):
        text = action.help
        if text is None:
            text = ''
        if '%(default)' not in text and action.default is not SUPPRESS:
            if action.option_strings or action.nargs in (OPTIONAL,
                                                          ZERO_OR_MORE):
                text = _DefaultedHelp(text + _(' (default: %(default)s)'))
        return text


class MetavarTypeHelpFormatter(HelpFormatter):
    """Help message formatter which uses the argument 'type' as the default
    metavar value (instead of the argument 'dest').

    Only the name of this class is considered a public API.
    """

    def _get_default_metavar_for_optional(self, action):
        return action.type.__name__

    def _get_default_metavar_for_positional(self, action):
        return action.type.__name__


# ---------------------------------------------------------------------------
# Errors
# ---------------------------------------------------------------------------

def _get_action_name(argument):
    if argument is None:
        return None
    if argument.option_strings:
        return '/'.join(argument.option_strings)
    if argument.metavar not in (None, SUPPRESS):
        metavar = argument.metavar
        if not isinstance(metavar, tuple):
            return metavar
        if argument.nargs == ZERO_OR_MORE and len(metavar) == 2:
            return '%s[, %s]' % metavar
        if argument.nargs == ONE_OR_MORE:
            return '%s[, %s]' % metavar
        return ', '.join(metavar)
    if argument.dest not in (None, SUPPRESS):
        return argument.dest
    if argument.choices:
        return '{%s}' % ','.join(map(str, argument.choices))
    return None


class ArgumentError(Exception):
    """An error from creating or using an argument (optional or
    positional).

    The string value of this exception is the message, augmented with
    information about the argument that caused it.
    """

    def __init__(self, argument, message):
        self.argument_name = _get_action_name(argument)
        self.message = message

    def __str__(self):
        if self.argument_name is None:
            return '%(message)s' % dict(message=self.message)
        return _('argument %(argument_name)s: %(message)s') % dict(
            message=self.message, argument_name=self.argument_name)


class ArgumentTypeError(Exception):
    """An error from trying to convert a command line string to a type."""


# ---------------------------------------------------------------------------
# Actions
# ---------------------------------------------------------------------------

class Action(_AttributeHolder):
    """Information about how to convert command line strings to Python
    objects.

    Action objects are used by an ArgumentParser to represent the
    information needed to parse a single argument from one or more strings
    from the command line.  The keyword arguments to the Action constructor
    are also all attributes of Action instances:

    - option_strings: the option strings (e.g. ['-f', '--foo']); empty for
      positional arguments.
    - dest: the attribute of the namespace that receives the value.
    - nargs: how many command line arguments are consumed (an integer,
      '?', '*', '+', argparse.REMAINDER or None for a single one).
    - const: the value used by some actions without command line value.
    - default: the value used when the argument is absent.
    - type: a callable converting the command line string.
    - choices: the allowed values.
    - required: whether an option must be given.
    - help: the help text.
    - metavar: the name used for the value in usage and help messages.
    - deprecated: whether using the argument emits a warning.
    """

    def __init__(self, option_strings, dest, nargs=None, const=None,
                 default=None, type=None, choices=None, required=False,
                 help=None, metavar=None, deprecated=False):
        self.option_strings = option_strings
        self.dest = dest
        self.nargs = nargs
        self.const = const
        self.default = default
        self.type = type
        self.choices = choices
        self.required = required
        self.help = help
        self.metavar = metavar
        self.deprecated = deprecated

    def _get_kwargs(self):
        names = ['option_strings', 'dest', 'nargs', 'const', 'default',
                 'type', 'choices', 'required', 'help', 'metavar',
                 'deprecated']
        return [(name, getattr(self, name)) for name in names]

    def format_usage(self):
        return self.option_strings[0]

    def __call__(self, parser, namespace, values, option_string=None):
        raise NotImplementedError('.__call__() not defined')


class BooleanOptionalAction(Action):
    """A flag with a negative form: --foo sets True, --no-foo sets False."""

    def __init__(self, option_strings, dest, default=None, required=False,
                 help=None, deprecated=False):
        expanded = []
        for option in option_strings:
            expanded.append(option)
            if option.startswith('--'):
                if option.startswith('--no-'):
                    raise ValueError(f'invalid option name {option!r} '
                                     f'for BooleanOptionalAction')
                expanded.append('--no-' + option[2:])
        super().__init__(option_strings=expanded, dest=dest, nargs=0,
                         default=default, required=required, help=help,
                         deprecated=deprecated)

    def __call__(self, parser, namespace, values, option_string=None):
        if option_string in self.option_strings:
            setattr(namespace, self.dest,
                    not option_string.startswith('--no-'))

    def format_usage(self):
        return ' | '.join(self.option_strings)


class _StoreAction(Action):

    def __init__(self, option_strings, dest, nargs=None, const=None,
                 default=None, type=None, choices=None, required=False,
                 help=None, metavar=None, deprecated=False):
        if nargs == 0:
            raise ValueError('nargs for store actions must be != 0; if you '
                             'have nothing to store, actions such as store '
                             'true or store const may be more appropriate')
        if const is not None and nargs != OPTIONAL:
            raise ValueError('nargs must be %r to supply const' % OPTIONAL)
        super(_StoreAction, self).__init__(
            option_strings=option_strings, dest=dest, nargs=nargs,
            const=const, default=default, type=type, choices=choices,
            required=required, help=help, metavar=metavar,
            deprecated=deprecated)

    def __call__(self, parser, namespace, values, option_string=None):
        setattr(namespace, self.dest, values)


class _StoreConstAction(Action):

    def __init__(self, option_strings, dest, const=None, default=None,
                 required=False, help=None, metavar=None, deprecated=False):
        super(_StoreConstAction, self).__init__(
            option_strings=option_strings, dest=dest, nargs=0, const=const,
            default=default, required=required, help=help,
            deprecated=deprecated)

    def __call__(self, parser, namespace, values, option_string=None):
        setattr(namespace, self.dest, self.const)


class _StoreTrueAction(_StoreConstAction):

    def __init__(self, option_strings, dest, default=False, required=False,
                 help=None, deprecated=False):
        super(_StoreTrueAction, self).__init__(
            option_strings=option_strings, dest=dest, const=True,
            deprecated=deprecated, required=required, help=help,
            default=default)


class _StoreFalseAction(_StoreConstAction):

    def __init__(self, option_strings, dest, default=True, required=False,
                 help=None, deprecated=False):
        super(_StoreFalseAction, self).__init__(
            option_strings=option_strings, dest=dest, const=False,
            default=default, required=required, help=help,
            deprecated=deprecated)


class _AppendAction(Action):

    def __init__(self, option_strings, dest, nargs=None, const=None,
                 default=None, type=None, choices=None, required=False,
                 help=None, metavar=None, deprecated=False):
        if nargs == 0:
            raise ValueError('nargs for append actions must be != 0; if arg '
                             'strings are not supplying the value to append, '
                             'the append const action may be more '
                             'appropriate')
        if const is not None and nargs != OPTIONAL:
            raise ValueError('nargs must be %r to supply const' % OPTIONAL)
        super(_AppendAction, self).__init__(
            option_strings=option_strings, dest=dest, nargs=nargs,
            const=const, default=default, type=type, choices=choices,
            required=required, help=help, metavar=metavar,
            deprecated=deprecated)

    def __call__(self, parser, namespace, values, option_string=None):
        items = getattr(namespace, self.dest, None)
        # a fresh list: the default must never be modified
        items = _copy_items(items)
        items.append(values)
        setattr(namespace, self.dest, items)


class _AppendConstAction(Action):

    def __init__(self, option_strings, dest, const=None, default=None,
                 required=False, help=None, metavar=None, deprecated=False):
        super(_AppendConstAction, self).__init__(
            option_strings=option_strings, dest=dest, nargs=0, const=const,
            default=default, required=required, help=help, metavar=metavar,
            deprecated=deprecated)

    def __call__(self, parser, namespace, values, option_string=None):
        items = _copy_items(getattr(namespace, self.dest, None))
        items.append(self.const)
        setattr(namespace, self.dest, items)


class _CountAction(Action):

    def __init__(self, option_strings, dest, default=None, required=False,
                 help=None, deprecated=False):
        super(_CountAction, self).__init__(
            option_strings=option_strings, dest=dest, nargs=0,
            default=default, required=required, help=help,
            deprecated=deprecated)

    def __call__(self, parser, namespace, values, option_string=None):
        count = getattr(namespace, self.dest, None)
        if count is None:
            count = 0
        setattr(namespace, self.dest, count + 1)


class _HelpAction(Action):

    def __init__(self, option_strings, dest=SUPPRESS, default=SUPPRESS,
                 help=None, deprecated=False):
        super(_HelpAction, self).__init__(
            option_strings=option_strings, dest=dest, default=default,
            nargs=0, help=help, deprecated=deprecated)

    def __call__(self, parser, namespace, values, option_string=None):
        parser.print_help()
        parser.exit()


class _VersionAction(Action):

    def __init__(self, option_strings, version=None, dest=SUPPRESS,
                 default=SUPPRESS, help=None, deprecated=False):
        if help is None:
            help = _("show program's version number and exit")
        super(_VersionAction, self).__init__(
            option_strings=option_strings, dest=dest, default=default,
            nargs=0, help=help)
        self.version = version

    def __call__(self, parser, namespace, values, option_string=None):
        version = self.version
        if version is None:
            version = parser.version
        formatter = parser._get_formatter()
        formatter.add_text(version)
        parser._print_message(formatter.format_help(), _sys.stdout)
        parser.exit()


class _SubParsersAction(Action):

    class _ChoicesPseudoAction(Action):

        def __init__(self, name, aliases, help):
            metavar = dest = name
            if aliases:
                metavar += ' (%s)' % ', '.join(aliases)
            sup = super(_SubParsersAction._ChoicesPseudoAction, self)
            sup.__init__(option_strings=[], dest=dest, help=help,
                         metavar=metavar)

    def __init__(self, option_strings, prog, parser_class, dest=SUPPRESS,
                 required=False, help=None, metavar=None):
        self._prog_prefix = prog
        self._parser_class = parser_class
        self._name_parser_map = {}
        self._choices_actions = []
        self._deprecated = set()
        self._color = True

        super(_SubParsersAction, self).__init__(
            option_strings=option_strings, dest=dest, nargs=PARSER,
            choices=self._name_parser_map, required=required, help=help,
            metavar=metavar)

    def add_parser(self, name, *, deprecated=False, **kwargs):
        """Add a sub-command and return its (new) parser.

        Keyword arguments go to the parser class, except 'aliases' (other
        names for the command), 'help' (its help in the command list) and
        'deprecated'."""
        if kwargs.get('prog') is None:
            kwargs['prog'] = '%s %s' % (self._prog_prefix, name)

        # inherit the parent's colour setting unless given
        if 'color' not in kwargs:
            kwargs['color'] = self._color

        aliases = kwargs.pop('aliases', ())

        if name in self._name_parser_map:
            raise ValueError('conflicting subparser: %s' % name)
        for alias in aliases:
            if alias in self._name_parser_map:
                raise ValueError('conflicting subparser alias: %s' % alias)

        if 'help' in kwargs:
            help = kwargs.pop('help')
            choice_action = self._ChoicesPseudoAction(name, aliases, help)
            self._choices_actions.append(choice_action)
        else:
            choice_action = None

        parser = self._parser_class(**kwargs)
        if choice_action is not None:
            parser._check_help(choice_action)
        self._name_parser_map[name] = parser
        for alias in aliases:
            self._name_parser_map[alias] = parser

        if deprecated:
            self._deprecated.add(name)
            self._deprecated.update(aliases)

        return parser

    def _get_subactions(self):
        return self._choices_actions

    def __call__(self, parser, namespace, values, option_string=None):
        parser_name = values[0]
        arg_strings = values[1:]

        if self.dest is not SUPPRESS:
            setattr(namespace, self.dest, parser_name)

        try:
            subparser = self._name_parser_map[parser_name]
        except KeyError:
            args = {'parser_name': parser_name,
                    'choices': ', '.join(self._name_parser_map)}
            msg = _('unknown parser %(parser_name)r (choices: '
                    '%(choices)s)') % args
            raise ArgumentError(self, msg)

        if parser_name in self._deprecated:
            parser._warning(_("command '%(parser_name)s' is deprecated")
                            % {'parser_name': parser_name})

        # The sub-parser fills a fresh namespace whose attributes then
        # override the parent's ones (defaults of the sub-parser win over
        # values already set by the parent).
        subnamespace, arg_strings = subparser._parse_known_args2(
            arg_strings, None, intermixed=False)
        for key, value in vars(subnamespace).items():
            setattr(namespace, key, value)

        if arg_strings:
            if not hasattr(namespace, _UNRECOGNIZED_ARGS_ATTR):
                setattr(namespace, _UNRECOGNIZED_ARGS_ATTR, [])
            getattr(namespace, _UNRECOGNIZED_ARGS_ATTR).extend(arg_strings)


class _ExtendAction(_AppendAction):

    def __call__(self, parser, namespace, values, option_string=None):
        items = _copy_items(getattr(namespace, self.dest, None))
        items.extend(values)
        setattr(namespace, self.dest, items)


# ---------------------------------------------------------------------------
# FileType (deprecated)
# ---------------------------------------------------------------------------

class FileType(object):
    """Deprecated factory for creating file object types.

    Instances of FileType are typically passed as type= arguments to the
    ArgumentParser add_argument() method.  The argument '-' stands for
    stdin/stdout.

    Keyword Arguments:
        - mode -- A string indicating how the file is to be opened. Accepts
            the same values as the builtin open() function.
        - bufsize -- The file's desired buffer size. Accepts the same values
            as the builtin open() function.
        - encoding -- The file's encoding. Accepts the same values as the
            builtin open() function.
        - errors -- A string indicating how encoding and decoding errors are
            to be handled. Accepts the same value as the builtin open()
            function.
    """

    def __init__(self, mode='r', bufsize=-1, encoding=None, errors=None):
        import warnings
        warnings.warn(
            "FileType is deprecated. Simply open files after parsing "
            "arguments.",
            category=PendingDeprecationWarning,
            stacklevel=2)
        self._mode = mode
        self._bufsize = bufsize
        self._encoding = encoding
        self._errors = errors

    def __call__(self, string):
        if string == '-':
            if 'r' in self._mode:
                stream = _sys.stdin
            elif any(c in self._mode for c in 'wax'):
                stream = _sys.stdout
            else:
                msg = _('argument "-" with mode %r') % self._mode
                raise ValueError(msg)
            return stream.buffer if 'b' in self._mode else stream

        try:
            return open(string, self._mode, self._bufsize, self._encoding,
                        self._errors)
        except OSError as e:
            args = {'filename': string, 'error': e}
            message = _("can't open '%(filename)s': %(error)s")
            raise ArgumentTypeError(message % args)

    def __repr__(self):
        args = self._mode, self._bufsize
        kwargs = [('encoding', self._encoding), ('errors', self._errors)]
        args_str = ', '.join([repr(arg) for arg in args if arg != -1] +
                             ['%s=%r' % (kw, arg) for kw, arg in kwargs
                              if arg is not None])
        return '%s(%s)' % (type(self).__name__, args_str)


# ---------------------------------------------------------------------------
# Namespace
# ---------------------------------------------------------------------------

class Namespace(_AttributeHolder):
    """Simple object for storing attributes.

    Implements equality by attribute names and values, and provides a
    simple string representation.
    """

    def __init__(self, **kwargs):
        for name in kwargs:
            setattr(self, name, kwargs[name])

    def __eq__(self, other):
        if not isinstance(other, Namespace):
            return NotImplemented
        return vars(self) == vars(other)

    def __contains__(self, key):
        return key in self.__dict__


# ---------------------------------------------------------------------------
# Containers of actions: parsers and argument groups
# ---------------------------------------------------------------------------

class _ActionsContainer(object):

    def __init__(self, description, prefix_chars, argument_default,
                 conflict_handler):
        super(_ActionsContainer, self).__init__()

        self.description = description
        self.argument_default = argument_default
        self.prefix_chars = prefix_chars
        self.conflict_handler = conflict_handler

        # action names and the action classes they stand for
        self._registries = {}
        self.register('action', None, _StoreAction)
        self.register('action', 'store', _StoreAction)
        self.register('action', 'store_const', _StoreConstAction)
        self.register('action', 'store_true', _StoreTrueAction)
        self.register('action', 'store_false', _StoreFalseAction)
        self.register('action', 'append', _AppendAction)
        self.register('action', 'append_const', _AppendConstAction)
        self.register('action', 'count', _CountAction)
        self.register('action', 'help', _HelpAction)
        self.register('action', 'version', _VersionAction)
        self.register('action', 'parsers', _SubParsersAction)
        self.register('action', 'extend', _ExtendAction)

        self._get_handler()

        self._actions = []
        self._option_string_actions = {}

        self._action_groups = []
        self._mutually_exclusive_groups = []

        self._defaults = {}

        # does any option look like a negative number?
        self._negative_number_matcher = _re.compile(r'-\.?\d')
        self._has_negative_number_optionals = []

    # -- registries --------------------------------------------------------------

    def register(self, registry_name, value, object):
        registry = self._registries.setdefault(registry_name, {})
        registry[value] = object

    def _registry_get(self, registry_name, value, default=None):
        return self._registries[registry_name].get(value, default)

    # -- namespace defaults ---------------------------------------------------------

    def set_defaults(self, **kwargs):
        self._defaults.update(kwargs)
        # matching actions get the new default too
        for action in self._actions:
            if action.dest in kwargs:
                action.default = kwargs[action.dest]

    def get_default(self, dest):
        for action in self._actions:
            if action.dest == dest and action.default is not None:
                return action.default
        return self._defaults.get(dest, None)

    # -- adding arguments -------------------------------------------------------------

    def add_argument(self, *args, **kwargs):
        """
        add_argument(dest, ..., name=value, ...)
        add_argument(option_string, option_string, ..., name=value, ...)
        """
        chars = self.prefix_chars
        if not args or len(args) == 1 and args[0][0] not in chars:
            if args and 'dest' in kwargs:
                raise TypeError('dest supplied twice for positional argument,'
                                ' did you mean metavar?')
            kwargs = self._get_positional_kwargs(*args, **kwargs)
        else:
            kwargs = self._get_optional_kwargs(*args, **kwargs)

        if 'default' not in kwargs:
            dest = kwargs['dest']
            if dest in self._defaults:
                kwargs['default'] = self._defaults[dest]
            elif self.argument_default is not None:
                kwargs['default'] = self.argument_default

        action_name = kwargs.get('action')
        action_class = self._pop_action_class(kwargs)
        if not callable(action_class):
            raise ValueError('unknown action {action_class!r}'.format(
                action_class=action_class))
        action = action_class(**kwargs)

        if not action.option_strings and action.nargs == 0:
            if not isinstance(action_name, str):
                action_name = getattr(action_class, '__name__', action_name)
            raise ValueError(f'action {action_name!r} is not '
                             f'valid for positional arguments')

        type_func = self._registry_get('type', action.type, action.type)
        if not callable(type_func):
            raise TypeError(f'{type_func!r} is not callable')

        if type_func is FileType:
            raise TypeError(f'{type_func!r} is a FileType class object, '
                            f'instance of it must be passed')

        # the metavar must work in usage messages
        if hasattr(self, '_get_formatter'):
            formatter = self._get_formatter()
            try:
                formatter._format_args(action, None)
            except TypeError:
                raise ValueError('length of metavar tuple does not match '
                                 'nargs')
        self._check_help(action)
        return self._add_action(action)

    def add_argument_group(self, *args, **kwargs):
        group = _ArgumentGroup(self, *args, **kwargs)
        self._action_groups.append(group)
        return group

    def add_mutually_exclusive_group(self, **kwargs):
        group = _MutuallyExclusiveGroup(self, **kwargs)
        self._mutually_exclusive_groups.append(group)
        return group

    def _add_action(self, action):
        self._check_conflict(action)

        self._actions.append(action)
        action.container = self

        for option_string in action.option_strings:
            self._option_string_actions[option_string] = action

        # options that look like negative numbers change how such
        # arguments are parsed
        for option_string in action.option_strings:
            if self._negative_number_matcher.match(option_string):
                if not self._has_negative_number_optionals:
                    self._has_negative_number_optionals.append(True)

        return action

    def _remove_action(self, action):
        self._actions.remove(action)

    def _add_container_actions(self, container):
        # the parent parser's groups by title
        title_group_map = {}
        for group in self._action_groups:
            if group.title in title_group_map:
                msg = 'cannot merge actions - two groups are named %r'
                raise ValueError(msg % (group.title))
            title_group_map[group.title] = group

        group_map = {}
        for group in container._action_groups:
            if group.title not in title_group_map:
                title_group_map[group.title] = self.add_argument_group(
                    title=group.title,
                    description=group.description,
                    conflict_handler=group.conflict_handler)
            for action in group._group_actions:
                group_map[action] = title_group_map[group.title]

        for group in container._mutually_exclusive_groups:
            if group._container is container:
                target = self
            else:
                target = title_group_map[group._container.title]
            mutex_group = target.add_mutually_exclusive_group(
                required=group.required)
            for action in group._group_actions:
                group_map[action] = mutex_group

        for action in container._actions:
            group_map.get(action, self)._add_action(action)

    def _get_positional_kwargs(self, dest, **kwargs):
        if 'required' in kwargs:
            msg = "'required' is an invalid argument for positionals"
            raise TypeError(msg)

        if kwargs.get('nargs') == 0:
            raise ValueError('nargs for positionals must be != 0')

        # positionals with optional counts are not required
        if kwargs.get('nargs') not in [OPTIONAL, ZERO_OR_MORE, REMAINDER,
                                       SUPPRESS, 0]:
            kwargs['required'] = True

        return dict(kwargs, dest=dest, option_strings=[])

    def _get_optional_kwargs(self, *args, **kwargs):
        option_strings = []
        for option_string in args:
            if not option_string or option_string[0] not in \
                    self.prefix_chars:
                args = {'option': option_string,
                        'prefix_chars': self.prefix_chars}
                msg = ('invalid option string %(option)r: '
                       'must start with a character %(prefix_chars)r')
                raise ValueError(msg % args)
            option_strings.append(option_string)

        dest = kwargs.pop('dest', None)
        if dest is None:
            # the first long option, else the first short one
            first_long = None
            for option_string in option_strings:
                if len(option_string) > 1 and \
                        option_string[1] in self.prefix_chars:
                    first_long = option_string
                    break
            dest_option_string = first_long or option_strings[0]
            dest = dest_option_string.lstrip(self.prefix_chars)
            if not dest:
                raise TypeError('dest= is required for options like %s'
                                % ', '.join(map(repr, option_strings)))
            dest = dest.replace('-', '_')

        return dict(kwargs, dest=dest, option_strings=option_strings)

    def _pop_action_class(self, kwargs, default=None):
        action = kwargs.pop('action', default)
        return self._registry_get('action', action, action)

    def _get_handler(self):
        handler_func_name = '_handle_conflict_%s' % self.conflict_handler
        try:
            return getattr(self, handler_func_name)
        except AttributeError:
            msg = 'invalid conflict_resolution value: %r'
            raise ValueError(msg % self.conflict_handler)

    def _check_conflict(self, action):
        conflicts = []
        for option_string in action.option_strings:
            if option_string in self._option_string_actions:
                conflicts.append(
                    (option_string,
                     self._option_string_actions[option_string]))
        if conflicts:
            self._get_handler()(action, conflicts)

    def _handle_conflict_error(self, action, conflicting_actions):
        message = ngettext('conflicting option string: %s',
                           'conflicting option strings: %s',
                           len(conflicting_actions))
        conflict_string = ', '.join([option_string
                                     for option_string, action
                                     in conflicting_actions])
        raise ArgumentError(action, message % conflict_string)

    def _handle_conflict_resolve(self, action, conflicting_actions):
        # the newer action takes over the conflicting option strings
        for option_string, old in conflicting_actions:
            old.option_strings.remove(option_string)
            self._option_string_actions.pop(option_string, None)
            if not old.option_strings:
                old.container._remove_action(old)

    def _check_help(self, action):
        if action.help and hasattr(self, '_get_formatter'):
            formatter = self._get_formatter()
            try:
                formatter._expand_help(action)
            except (ValueError, TypeError, KeyError) as exc:
                raise ValueError('badly formed help string') from exc


class _ArgumentGroup(_ActionsContainer):

    def __init__(self, container, title=None, description=None, **kwargs):
        if 'prefix_chars' in kwargs:
            import warnings
            depr_msg = (
                "The use of the undocumented 'prefix_chars' parameter in "
                "ArgumentParser.add_argument_group() is deprecated."
            )
            warnings.warn(depr_msg, DeprecationWarning, stacklevel=3)

        update = kwargs.setdefault
        update('conflict_handler', container.conflict_handler)
        update('prefix_chars', container.prefix_chars)
        update('argument_default', container.argument_default)
        super_init = super(_ArgumentGroup, self).__init__
        super_init(description=description, **kwargs)

        self.title = title
        self._group_actions = []

        # share the bookkeeping of the container
        self._registries = container._registries
        self._actions = container._actions
        self._option_string_actions = container._option_string_actions
        self._defaults = container._defaults
        self._has_negative_number_optionals = \
            container._has_negative_number_optionals
        self._mutually_exclusive_groups = container._mutually_exclusive_groups

    def _add_action(self, action):
        action = super(_ArgumentGroup, self)._add_action(action)
        self._group_actions.append(action)
        return action

    def _remove_action(self, action):
        super(_ArgumentGroup, self)._remove_action(action)
        self._group_actions.remove(action)

    def add_argument_group(self, *args, **kwargs):
        raise ValueError('argument groups cannot be nested')


class _MutuallyExclusiveGroup(_ArgumentGroup):

    def __init__(self, container, required=False):
        super(_MutuallyExclusiveGroup, self).__init__(container)
        self.required = required
        self._container = container

    def _add_action(self, action):
        if action.required:
            msg = 'mutually exclusive arguments must be optional'
            raise ValueError(msg)
        action = self._container._add_action(action)
        self._group_actions.append(action)
        return action

    def _remove_action(self, action):
        self._container._remove_action(action)
        self._group_actions.remove(action)

    def add_mutually_exclusive_group(self, **kwargs):
        raise ValueError('mutually exclusive groups cannot be nested')


# ---------------------------------------------------------------------------
# ArgumentParser
# ---------------------------------------------------------------------------

class ArgumentParser(_AttributeHolder, _ActionsContainer):
    """Object for parsing command line strings into Python objects.

    Keyword Arguments:
        - prog -- The name of the program (default: derived from
          sys.argv[0] and how the program was started)
        - usage -- A usage message (default: auto-generated from arguments)
        - description -- A description of what the program does
        - epilog -- Text following the argument descriptions
        - parents -- Parsers whose arguments should be copied into this one
        - formatter_class -- HelpFormatter class for printing help messages
        - prefix_chars -- Characters that prefix optional arguments
        - fromfile_prefix_chars -- Characters that prefix files containing
            additional arguments
        - argument_default -- The default value for all arguments
        - conflict_handler -- String indicating how to handle conflicts
        - add_help -- Add a -h/-help option
        - allow_abbrev -- Allow long options to be abbreviated unambiguously
        - exit_on_error -- Determines whether or not ArgumentParser exits with
            error info when an error occurs
        - suggest_on_error - Enables suggestions for mistyped argument choices
            and subparser names
        - color - Allow color output in help messages
    """

    def __init__(self,
                 prog=None,
                 usage=None,
                 description=None,
                 epilog=None,
                 parents=[],
                 formatter_class=HelpFormatter,
                 prefix_chars='-',
                 fromfile_prefix_chars=None,
                 argument_default=None,
                 conflict_handler='error',
                 add_help=True,
                 allow_abbrev=True,
                 exit_on_error=True,
                 *,
                 suggest_on_error=True,
                 color=True,
                 ):
        superinit = super(ArgumentParser, self).__init__
        superinit(description=description,
                  prefix_chars=prefix_chars,
                  argument_default=argument_default,
                  conflict_handler=conflict_handler)

        self.prog = _prog_name(prog)
        self.usage = usage
        self.epilog = epilog
        self.formatter_class = formatter_class
        self.fromfile_prefix_chars = fromfile_prefix_chars
        self.add_help = add_help
        self.allow_abbrev = allow_abbrev
        self.exit_on_error = exit_on_error
        self.suggest_on_error = suggest_on_error
        self.color = color

        add_group = self.add_argument_group
        self._positionals = add_group(_('positional arguments'))
        self._optionals = add_group(_('options'))
        self._subparsers = None

        # values without a type are kept as they are
        self.register('type', None, _identity)

        # '-h' unless '-' is not a prefix character at all
        default_prefix = '-' if '-' in prefix_chars else prefix_chars[0]
        if self.add_help:
            self.add_argument(
                default_prefix + 'h', default_prefix * 2 + 'help',
                action='help', default=SUPPRESS,
                help=_('show this help message and exit'))

        for parent in parents:
            if not isinstance(parent, ArgumentParser):
                raise TypeError('parents must be a list of ArgumentParser')
            self._add_container_actions(parent)
            self._defaults.update(parent._defaults)

    # -- pretty printing ---------------------------------------------------------

    def _get_kwargs(self):
        names = ['prog', 'usage', 'description', 'formatter_class',
                 'conflict_handler', 'add_help']
        return [(name, getattr(self, name)) for name in names]

    # -- sub-commands ---------------------------------------------------------------

    def add_subparsers(self, **kwargs):
        """Add a sub-command argument; returns an object whose add_parser()
        method creates the parser of each sub-command."""
        if self._subparsers is not None:
            raise ValueError('cannot have multiple subparser arguments')

        kwargs.setdefault('parser_class', type(self))

        if 'title' in kwargs or 'description' in kwargs:
            title = kwargs.pop('title', _('subcommands'))
            description = kwargs.pop('description', None)
            self._subparsers = self.add_argument_group(title, description)
        else:
            self._subparsers = self._positionals

        if kwargs.get('prog') is None:
            # the program name plus the positionals before the command
            formatter = self.formatter_class(prog=self.prog)
            positionals = self._get_positional_actions()
            groups = self._mutually_exclusive_groups
            formatter.add_usage(None, positionals, groups, '')
            kwargs['prog'] = formatter.format_help().strip()

        parsers_class = self._pop_action_class(kwargs, 'parsers')
        action = parsers_class(option_strings=[], **kwargs)
        action._color = self.color
        self._check_help(action)
        self._subparsers._add_action(action)
        return action

    def _add_action(self, action):
        if action.option_strings:
            self._optionals._add_action(action)
        else:
            self._positionals._add_action(action)
        return action

    def _get_optional_actions(self):
        return [action for action in self._actions if action.option_strings]

    def _get_positional_actions(self):
        return [action for action in self._actions
                if not action.option_strings]

    # -- parsing entry points ------------------------------------------------------------

    def parse_args(self, args=None, namespace=None):
        args, extras = self.parse_known_args(args, namespace)
        if extras:
            self._unrecognized(extras)
        return args

    def _unrecognized(self, extras):
        msg = _('unrecognized arguments: %s') % ' '.join(extras)
        if self.exit_on_error:
            self.error(msg)
        raise ArgumentError(None, msg)

    def parse_known_args(self, args=None, namespace=None):
        return self._parse_known_args2(args, namespace, intermixed=False)

    def parse_intermixed_args(self, args=None, namespace=None):
        args, extras = self.parse_known_intermixed_args(args, namespace)
        if extras:
            self._unrecognized(extras)
        return args

    def parse_known_intermixed_args(self, args=None, namespace=None):
        # Optionals and positionals may be freely mixed: optionals are
        # consumed first, then the remaining strings fill the positionals.
        for action in self._get_positional_actions():
            if action.nargs in (PARSER, REMAINDER):
                raise TypeError('parse_intermixed_args: positional arg '
                                'with nargs=%s' % action.nargs)
        return self._parse_known_args2(args, namespace, intermixed=True)

    def _parse_known_args2(self, args, namespace, intermixed):
        if args is None:
            args = _sys.argv[1:]
        else:
            args = list(args)
        if namespace is None:
            namespace = Namespace()

        # defaults first; values parsed later overwrite them
        for action in self._actions:
            if action.dest is not SUPPRESS and \
                    not hasattr(namespace, action.dest) and \
                    action.default is not SUPPRESS:
                setattr(namespace, action.dest, action.default)
        for dest, value in self._defaults.items():
            if not hasattr(namespace, dest):
                setattr(namespace, dest, value)

        if self.exit_on_error:
            try:
                namespace, args = self._parse_known_args(args, namespace,
                                                         intermixed)
            except ArgumentError as err:
                self.error(str(err))
        else:
            namespace, args = self._parse_known_args(args, namespace,
                                                     intermixed)

        if hasattr(namespace, _UNRECOGNIZED_ARGS_ATTR):
            args.extend(getattr(namespace, _UNRECOGNIZED_ARGS_ATTR))
            delattr(namespace, _UNRECOGNIZED_ARGS_ATTR)
        return namespace, args

    def _parse_known_args(self, arg_strings, namespace, intermixed):
        if self.fromfile_prefix_chars is not None:
            arg_strings = self._read_args_from_files(arg_strings)
        run = _ParseRun(self, arg_strings, namespace)
        return run.parse(intermixed)

    def _read_args_from_files(self, arg_strings):
        expanded = []
        for arg_string in arg_strings:
            if not arg_string or \
                    arg_string[0] not in self.fromfile_prefix_chars:
                expanded.append(arg_string)
                continue
            try:
                with open(arg_string[1:],
                          encoding=_sys.getfilesystemencoding(),
                          errors=_sys.getfilesystemencodeerrors()) as f:
                    loaded = []
                    for line in f.read().splitlines():
                        loaded.extend(self.convert_arg_line_to_args(line))
                # files may refer to further files
                expanded.extend(self._read_args_from_files(loaded))
            except OSError as err:
                raise ArgumentError(None, str(err))
        return expanded

    def convert_arg_line_to_args(self, arg_line):
        return [arg_line]

    # -- matching argument counts ------------------------------------------------------

    def _match_argument(self, action, arg_strings_pattern):
        match = _re.match(self._get_nargs_pattern(action),
                          arg_strings_pattern)
        if match is not None:
            return len(match.group(1))
        nargs_errors = {
            None: _('expected one argument'),
            OPTIONAL: _('expected at most one argument'),
            ONE_OR_MORE: _('expected at least one argument'),
        }
        msg = nargs_errors.get(action.nargs)
        if msg is None:
            msg = ngettext('expected %s argument',
                           'expected %s arguments',
                           action.nargs) % action.nargs
        raise ArgumentError(action, msg)

    def _match_arguments_partial(self, actions, arg_strings_pattern):
        # as many actions as possible, dropping trailing ones until the
        # patterns match
        for count in range(len(actions), 0, -1):
            pattern = ''.join(self._get_nargs_pattern(action)
                              for action in actions[:count])
            match = _re.match(pattern, arg_strings_pattern)
            if match is None:
                continue
            result = [len(group) for group in match.groups()]
            # an option follows: positionals that matched nothing wait
            # for strings after it
            if match.end() < len(arg_strings_pattern) and \
                    arg_strings_pattern[match.end()] == 'O':
                while result and not result[-1]:
                    del result[-1]
            return result
        return []

    def _parse_optional(self, arg_string):
        if not arg_string:
            return None
        if arg_string[0] not in self.prefix_chars:
            return None
        if arg_string in self._option_string_actions:
            action = self._option_string_actions[arg_string]
            return [(action, arg_string, None, None)]
        if len(arg_string) == 1:
            # a lone prefix character is a positional
            return None

        option_string, sep, explicit_arg = arg_string.partition('=')
        if sep and option_string in self._option_string_actions:
            action = self._option_string_actions[option_string]
            return [(action, option_string, sep, explicit_arg)]

        candidates = self._get_option_tuples(arg_string)
        if candidates:
            # several candidates: reported as ambiguous when consumed
            return candidates

        if self._negative_number_matcher.match(arg_string):
            if not self._has_negative_number_optionals:
                return None
        if ' ' in arg_string:
            return None
        # an unknown option (maybe known to a sub-parser)
        return [(None, arg_string, None, None)]

    def _get_option_tuples(self, option_string):
        result = []
        chars = self.prefix_chars
        known = self._option_string_actions
        if option_string[0] in chars and option_string[1] in chars:
            # long options can only be abbreviated (or use '=')
            if self.allow_abbrev:
                prefix, sep, explicit_arg = option_string.partition('=')
                if not sep:
                    sep = explicit_arg = None
                for candidate in known:
                    if candidate.startswith(prefix):
                        result.append((known[candidate], candidate, sep,
                                       explicit_arg))
        elif option_string[0] in chars and option_string[1] not in chars:
            # short options may have their value attached: -xVALUE
            prefix, sep, explicit_arg = option_string.partition('=')
            if not sep:
                sep = explicit_arg = None
            short_prefix = option_string[:2]
            short_arg = option_string[2:]
            for candidate in known:
                if candidate == short_prefix:
                    result.append((known[candidate], candidate, '',
                                   short_arg))
                elif self.allow_abbrev and candidate.startswith(prefix):
                    result.append((known[candidate], candidate, sep,
                                   explicit_arg))
        else:
            raise ArgumentError(None, _('unexpected option string: %s')
                                % option_string)
        return result

    def _get_nargs_pattern(self, action):
        # In the pattern of argument strings, 'A' is an argument, 'O' an
        # option and '-' the '--' separator.
        nargs = action.nargs
        is_option = bool(action.option_strings)
        if nargs is None:
            pattern = '(-*A-*)'
        elif nargs == OPTIONAL:
            pattern = '(-*A?-*)'
        elif nargs == ZERO_OR_MORE:
            pattern = '(-*[A-]*)'
        elif nargs == ONE_OR_MORE:
            pattern = '(-*A[A-]*)'
        elif nargs == REMAINDER:
            pattern = '([AO]*)' if is_option else '(.*)'
        elif nargs == PARSER:
            pattern = '(-*A[-AO]*)'
        elif nargs == SUPPRESS:
            pattern = '(-*-*)'
        else:
            pattern = '(-*%s-*)' % '-*'.join('A' * nargs)

        if is_option:
            # an option's values never include '--'
            pattern = pattern.replace('-*', '').replace('-', '')
        return pattern

    # -- converting values ----------------------------------------------------------------

    def _get_values(self, action, arg_strings):
        nargs = action.nargs
        if not arg_strings and nargs == OPTIONAL:
            value = action.const if action.option_strings else action.default
            if isinstance(value, str) and value is not SUPPRESS:
                value = self._get_value(action, value)
        elif not arg_strings and nargs == ZERO_OR_MORE and \
                not action.option_strings:
            value = action.default if action.default is not None else []
        elif len(arg_strings) == 1 and nargs in (None, OPTIONAL):
            value = self._get_value(action, arg_strings[0])
            self._check_value(action, value)
        elif nargs == REMAINDER:
            value = [self._get_value(action, v) for v in arg_strings]
        elif nargs == PARSER:
            value = [self._get_value(action, v) for v in arg_strings]
            self._check_value(action, value[0])
        elif nargs == SUPPRESS:
            value = SUPPRESS
        else:
            value = [self._get_value(action, v) for v in arg_strings]
            for item in value:
                self._check_value(action, item)
        return value

    def _get_value(self, action, arg_string):
        type_func = self._registry_get('type', action.type, action.type)
        if not callable(type_func):
            raise TypeError(f'{type_func!r} is not callable')
        try:
            return type_func(arg_string)
        except ArgumentTypeError as err:
            raise ArgumentError(action, str(err))
        except (TypeError, ValueError):
            name = getattr(action.type, '__name__', repr(action.type))
            args = {'type': name, 'value': arg_string}
            msg = _('invalid %(type)s value: %(value)r')
            raise ArgumentError(action, msg % args)

    def _check_value(self, action, value):
        choices = action.choices
        if choices is None:
            return
        if isinstance(choices, str):
            # membership must be by element, not by substring
            choices = iter(choices)
        if value in choices:
            return
        args = {'value': str(value),
                'choices': ', '.join(repr(str(c)) for c in action.choices)}
        msg = _('invalid choice: %(value)r (choose from %(choices)s)')
        if self.suggest_on_error and isinstance(value, str) and \
                all(isinstance(c, str) for c in action.choices):
            import difflib
            close = difflib.get_close_matches(value, list(action.choices), 1)
            if close:
                args['closest'] = close[0]
                msg = _('invalid choice: %(value)r, maybe you meant '
                        '%(closest)r? (choose from %(choices)s)')
        raise ArgumentError(action, msg % args)

    # -- help and usage -----------------------------------------------------------------

    def format_usage(self):
        return self._usage_text(None)

    def _usage_text(self, file):
        formatter = self._get_formatter(file=file)
        formatter.add_usage(self.usage, self._actions,
                            self._mutually_exclusive_groups)
        return formatter.format_help()

    def format_help(self):
        return self._help_text(None)

    def _help_text(self, file):
        formatter = self._get_formatter(file=file)
        formatter.add_usage(self.usage, self._actions,
                            self._mutually_exclusive_groups)
        formatter.add_text(self.description)
        for group in self._action_groups:
            formatter.start_section(group.title)
            formatter.add_text(group.description)
            formatter.add_arguments(group._group_actions)
            formatter.end_section()
        formatter.add_text(self.epilog)
        return formatter.format_help()

    def _get_formatter(self, file=None):
        formatter = self.formatter_class(prog=self.prog)
        set_color = getattr(formatter, '_set_color', None)
        if set_color is not None:
            set_color(self.color, file=file)
        return formatter

    def _check_help(self, action):
        if action.help:
            # validated without colours (and without importing _colorize)
            formatter = self.formatter_class(prog=self.prog)
            try:
                formatter._expand_help(action)
            except (ValueError, TypeError, KeyError) as exc:
                raise ValueError('badly formed help string') from exc

    def print_usage(self, file=None):
        if file is None:
            file = _sys.stdout
        self._print_message(self._usage_text(file), file)

    def print_help(self, file=None):
        if file is None:
            file = _sys.stdout
        self._print_message(self._help_text(file), file)

    def _print_message(self, message, file=None):
        if message:
            file = file or _sys.stderr
            try:
                file.write(message)
            except (AttributeError, OSError):
                pass

    # -- exiting -------------------------------------------------------------------------------

    def exit(self, status=0, message=None):
        if message:
            self._print_message(message, _sys.stderr)
        _sys.exit(status)

    def _keyword_theme(self, file):
        if not self.color:
            return _colorless_theme
        from _colorize import can_colorize, get_theme
        if can_colorize(file=file):
            return get_theme(force_color=True).argparse
        return _colorless_theme

    def _diagnostic(self, template, keyword, color_name, message):
        args = {'prog': self.prog, 'message': message}
        text = template % args
        theme = self._keyword_theme(_sys.stderr)
        color = getattr(theme, color_name)
        if color:
            # highlight the 'error:'/'warning:' keyword after the program
            head, sep, tail = text.partition(f'{self.prog}: ')
            if tail.startswith(keyword):
                text = (f'{head}{sep}{color}{keyword}{theme.reset}'
                        f'{tail[len(keyword):]}')
        return text

    def error(self, message):
        """error(message: string)

        Prints a usage message incorporating the message to stderr and
        exits.

        If you override this in a subclass, it should not return -- it
        should either exit or raise an exception.
        """
        self.print_usage(_sys.stderr)
        self.exit(2, self._diagnostic(_('%(prog)s: error: %(message)s\n'),
                                      'error:', 'error', message))

    def _warning(self, message):
        self._print_message(
            self._diagnostic(_('%(prog)s: warning: %(message)s\n'),
                             'warning:', 'warning', message),
            _sys.stderr)


class _ParseRun:
    """The state of one call of ArgumentParser._parse_known_args()."""

    def __init__(self, parser, arg_strings, namespace):
        self.parser = parser
        self.arg_strings = arg_strings
        self.namespace = namespace

        # actions that cannot be combined (mutually exclusive groups)
        self.conflicts = {}
        for group in parser._mutually_exclusive_groups:
            members = group._group_actions
            for index, action in enumerate(members):
                others = self.conflicts.setdefault(action, [])
                others.extend(members[:index])
                others.extend(members[index + 1:])

        # classify the strings: 'O' option, 'A' argument, '-' for '--'
        self.option_indices = {}
        pattern = []
        after_separator = False
        for index, arg_string in enumerate(arg_strings):
            if after_separator:
                pattern.append('A')
            elif arg_string == '--':
                pattern.append('-')
                after_separator = True
            else:
                found = parser._parse_optional(arg_string)
                if found is None:
                    pattern.append('A')
                else:
                    self.option_indices[index] = found
                    pattern.append('O')
        self.pattern = ''.join(pattern)

        self.seen = set()
        self.seen_non_default = set()
        self.warned = set()
        self.extras = []
        self.extras_pattern = []
        self.positionals = parser._get_positional_actions()

    # -- applying actions --------------------------------------------------------

    def take_action(self, action, argument_strings, option_string=None):
        parser = self.parser
        self.seen.add(action)
        values = parser._get_values(action, argument_strings)

        # explicitly given (not just defaulted) arguments must not clash
        if action.option_strings or argument_strings:
            self.seen_non_default.add(action)
            for other in self.conflicts.get(action, []):
                if other in self.seen_non_default:
                    msg = _('not allowed with argument %s')
                    raise ArgumentError(action,
                                        msg % _get_action_name(other))

        if values is not SUPPRESS:
            action(parser, self.namespace, values, option_string)

    def warn_option(self, action, option_string):
        if action.deprecated and option_string not in self.warned:
            self.parser._warning(_("option '%(option)s' is deprecated")
                                 % {'option': option_string})
            self.warned.add(option_string)

    # -- options --------------------------------------------------------------------

    def consume_optional(self, start_index):
        parser = self.parser
        candidates = self.option_indices[start_index]
        if len(candidates) > 1:
            options = ', '.join(option for _, option, _, _ in candidates)
            args = {'option': self.arg_strings[start_index],
                    'matches': options}
            msg = _('ambiguous option: %(option)s could match %(matches)s')
            raise ArgumentError(None, msg % args)

        action, option_string, sep, explicit_arg = candidates[0]
        chars = parser.prefix_chars
        to_apply = []
        while True:
            if action is None:
                # unknown option: left for the caller (or a sub-parser)
                self.extras.append(self.arg_strings[start_index])
                self.extras_pattern.append('O')
                return start_index + 1

            if explicit_arg is None:
                # the values are the following argument strings
                start = start_index + 1
                count = parser._match_argument(action, self.pattern[start:])
                stop = start + count
                to_apply.append((action, self.arg_strings[start:stop],
                                 option_string))
                break

            count = parser._match_argument(action, 'A')
            if count == 1:
                stop = start_index + 1
                to_apply.append((action, [explicit_arg], option_string))
                break
            if (count == 0 and option_string[1] not in chars
                    and explicit_arg != ''):
                # a flag combined with more short options: -xyz
                if sep or explicit_arg[0] in chars:
                    msg = _('ignored explicit argument %r')
                    raise ArgumentError(action, msg % explicit_arg)
                to_apply.append((action, [], option_string))
                prefix = option_string[0]
                option_string = prefix + explicit_arg[0]
                known = parser._option_string_actions
                if option_string not in known:
                    self.extras.append(prefix + explicit_arg)
                    self.extras_pattern.append('O')
                    stop = start_index + 1
                    break
                action = known[option_string]
                explicit_arg = explicit_arg[1:]
                if not explicit_arg:
                    sep = explicit_arg = None
                elif explicit_arg[0] == '=':
                    sep = '='
                    explicit_arg = explicit_arg[1:]
                else:
                    sep = ''
                continue
            msg = _('ignored explicit argument %r')
            raise ArgumentError(action, msg % explicit_arg)

        for action, args, option_string in to_apply:
            self.warn_option(action, option_string)
            self.take_action(action, args, option_string)
        return stop

    # -- positionals ------------------------------------------------------------------

    def consume_positionals(self, start_index):
        parser = self.parser
        counts = parser._match_arguments_partial(self.positionals,
                                                 self.pattern[start_index:])
        for action, count in zip(self.positionals, counts):
            args = self.arg_strings[start_index:start_index + count]
            # the first '--' separates, it is not a value (except for
            # REMAINDER, which takes everything)
            if action.nargs == PARSER:
                if self.pattern[start_index] == '-':
                    assert args[0] == '--'
                    args.remove('--')
            elif action.nargs != REMAINDER:
                if self.pattern.find('-', start_index,
                                     start_index + count) >= 0:
                    args.remove('--')
            start_index += count
            if args and action.deprecated and action.dest not in self.warned:
                parser._warning(_("argument '%(argument_name)s' is "
                                  "deprecated")
                                % {'argument_name': action.dest})
                self.warned.add(action.dest)
            self.take_action(action, args)
        del self.positionals[:len(counts)]
        return start_index

    # -- the whole run --------------------------------------------------------------------

    def parse(self, intermixed):
        arg_strings = self.arg_strings
        indices = self.option_indices
        last_option = max(indices) if indices else -1

        start = 0
        while start <= last_option:
            next_option = start
            while next_option <= last_option and next_option not in indices:
                next_option += 1

            if not intermixed and start != next_option:
                stop = self.consume_positionals(start)
                if stop > start:
                    start = stop
                    continue
                start = stop

            if start not in indices:
                # strings no positional wanted
                self.extras.extend(arg_strings[start:next_option])
                self.extras_pattern.extend(self.pattern[start:next_option])
                start = next_option

            start = self.consume_optional(start)

        if not intermixed:
            stop = self.consume_positionals(start)
            self.extras.extend(arg_strings[stop:])
        else:
            self._finish_intermixed(start)

        self.check_required()
        return self.namespace, self.extras

    def _finish_intermixed(self, start):
        self.extras.extend(self.arg_strings[start:])
        self.extras_pattern.extend(self.pattern[start:])
        pattern = ''.join(self.extras_pattern)
        assert len(pattern) == len(self.extras)

        # now fill the positionals from everything that is not an option
        self.arg_strings = [s for s, kind in zip(self.extras, pattern)
                            if kind != 'O']
        self.pattern = pattern.replace('O', '')
        consumed = self.consume_positionals(0)

        remaining = []
        for item, kind in zip(self.extras, pattern):
            if kind != 'O' and consumed:
                consumed -= 1
                continue
            remaining.append(item)
        self.extras = remaining

    def check_required(self):
        parser = self.parser
        namespace = self.namespace
        missing = []
        for action in parser._actions:
            if action in self.seen:
                continue
            if action.required:
                missing.append(_get_action_name(action))
            elif (action.default is not None
                    and isinstance(action.default, str)
                    and hasattr(namespace, action.dest)
                    and action.default is getattr(namespace, action.dest)):
                # string defaults are converted only when actually used
                setattr(namespace, action.dest,
                        parser._get_value(action, action.default))
        if missing:
            raise ArgumentError(None,
                                _('the following arguments are required: %s')
                                % ', '.join(missing))

        for group in parser._mutually_exclusive_groups:
            if not group.required:
                continue
            if any(action in self.seen_non_default
                   for action in group._group_actions):
                continue
            names = [_get_action_name(action)
                     for action in group._group_actions
                     if action.help is not SUPPRESS]
            msg = _('one of the arguments %s is required')
            raise ArgumentError(None, msg % ' '.join(names))
