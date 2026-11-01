"""Reading and writing INI-style configuration files.

A configuration consists of named sections holding ``option = value``
pairs::

    [server]
    host = example.org
    port: 8080
    motd = first line
        second line (indented continuation)

Option names are case-insensitive (see ``optionxform``), section names are
case-sensitive.  A special default section (``DEFAULT`` unless configured
otherwise) supplies fallback values for every other section.

``RawConfigParser`` returns values verbatim; ``ConfigParser`` additionally
expands ``%(name)s`` references (``BasicInterpolation``) or, when given
``ExtendedInterpolation()``, ``${section:option}`` references.  Parsers also
behave like mappings from section names to ``SectionProxy`` objects.
"""

from collections.abc import MutableMapping
from collections import ChainMap as _ChainMap
import functools
import io
import itertools
import os
import re
import sys

__all__ = ("NoSectionError", "DuplicateOptionError", "DuplicateSectionError",
           "NoOptionError", "InterpolationError", "InterpolationDepthError",
           "InterpolationMissingOptionError", "InterpolationSyntaxError",
           "ParsingError", "MissingSectionHeaderError",
           "MultilineContinuationError", "UnnamedSectionDisabledError",
           "InvalidWriteError", "ConfigParser", "RawConfigParser",
           "Interpolation", "BasicInterpolation", "ExtendedInterpolation",
           "SectionProxy", "ConverterMapping", "DEFAULTSECT",
           "MAX_INTERPOLATION_DEPTH", "UNNAMED_SECTION")

_default_dict = dict
DEFAULTSECT = "DEFAULT"

MAX_INTERPOLATION_DEPTH = 10


# --------------------------------------------------------------------------
# exceptions

class Error(Exception):
    """Base class for configparser exceptions."""

    def __init__(self, msg=''):
        self.message = msg
        Exception.__init__(self, msg)

    def __repr__(self):
        return self.message

    __str__ = __repr__


def _source_prefix(source, lineno):
    text = f"While reading from {source!r}"
    if lineno is not None:
        text += f" [line {lineno:2d}]"
    return text + ": "


class NoSectionError(Error):
    """A section that was asked for does not exist."""

    def __init__(self, section):
        Error.__init__(self, f'No section: {section!r}')
        self.section = section
        self.args = (section,)


class DuplicateSectionError(Error):
    """A section is defined twice (while reading in strict mode, or by
    add_section())."""

    def __init__(self, section, source=None, lineno=None):
        if source is None:
            msg = f"Section {section!r} already exists"
        else:
            msg = (_source_prefix(source, lineno) +
                   f"section {section!r} already exists")
        Error.__init__(self, msg)
        self.section = section
        self.source = source
        self.lineno = lineno
        self.args = (section, source, lineno)


class DuplicateOptionError(Error):
    """An option occurs twice in one section while reading in strict
    mode."""

    def __init__(self, section, option, source=None, lineno=None):
        if source is None:
            msg = f"Option {option!r} in section {section!r} already exists"
        else:
            msg = (_source_prefix(source, lineno) +
                   f"option {option!r} in section {section!r} "
                   f"already exists")
        Error.__init__(self, msg)
        self.section = section
        self.option = option
        self.source = source
        self.lineno = lineno
        self.args = (section, option, source, lineno)


class NoOptionError(Error):
    """An option that was asked for does not exist."""

    def __init__(self, option, section):
        Error.__init__(self, f"No option {option!r} in section: {section!r}")
        self.option = option
        self.section = section
        self.args = (option, section)


class InterpolationError(Error):
    """Base class for problems while expanding references in values."""

    def __init__(self, option, section, msg):
        Error.__init__(self, msg)
        self.option = option
        self.section = section
        self.args = (option, section, msg)


class InterpolationMissingOptionError(InterpolationError):
    """A value refers to an option that does not exist."""

    def __init__(self, option, section, rawval, reference):
        msg = (f"Bad value substitution: option {option!r} in section "
               f"{section!r} contains an interpolation key {reference!r} "
               f"which is not a valid option name. Raw value: {rawval!r}")
        InterpolationError.__init__(self, option, section, msg)
        self.reference = reference
        self.args = (option, section, rawval, reference)


class InterpolationSyntaxError(InterpolationError):
    """A value contains a malformed reference."""


class InterpolationDepthError(InterpolationError):
    """References nest deeper than MAX_INTERPOLATION_DEPTH (usually a
    reference cycle)."""

    def __init__(self, option, section, rawval):
        msg = (f"Recursion limit exceeded in value substitution: option "
               f"{option!r} in section {section!r} contains an "
               f"interpolation key which cannot be substituted in "
               f"{MAX_INTERPOLATION_DEPTH} steps. Raw value: {rawval!r}")
        InterpolationError.__init__(self, option, section, msg)
        self.args = (option, section, rawval)


class ParsingError(Error):
    """The input could not be parsed; ``errors`` lists the offending
    (line number, line) pairs."""

    def __init__(self, source, *errors):
        # *errors* are (line number, line) pairs, reported all at once
        details = ''.join(f'\n\t[line {lineno:2d}]: {line!r}'
                          for lineno, line in errors)
        Error.__init__(self,
                       f'Source contains parsing errors: {source!r}{details}')
        self.source = source
        self.errors = list(errors)
        self.args = (source,)

    def append(self, lineno, line):
        self.errors.append((lineno, line))
        self.message += f'\n\t[line {lineno:2d}]: {line!r}'


class MissingSectionHeaderError(ParsingError):
    """Options appear before the first section header."""

    def __init__(self, filename, lineno, line):
        Error.__init__(
            self,
            f'File contains no section headers.\nfile: {filename!r}, '
            f'line: {lineno:d}\n{line!r}')
        self.source = filename
        self.lineno = lineno
        self.line = line
        self.errors = []
        self.args = (filename, lineno, line)


class MultilineContinuationError(ParsingError):
    """An indented line follows an option that has no value."""

    def __init__(self, filename, lineno, line):
        Error.__init__(
            self,
            f'Key without value continued with an indented line.\n'
            f'file: {filename!r}, line: {lineno:d}\n{line!r}')
        self.source = filename
        self.lineno = lineno
        self.line = line
        self.errors = []
        self.args = (filename, lineno, line)


class UnnamedSectionDisabledError(Error):
    """UNNAMED_SECTION was used by a parser created without
    allow_unnamed_section=True."""

    def __init__(self):
        Error.__init__(self, "Support for UNNAMED_SECTION is disabled.")


class InvalidWriteError(Error):
    """write() was asked to produce output that would read back
    differently (a key containing a delimiter or looking like a section
    header)."""


class _UnnamedSection:
    def __repr__(self):
        return "<UNNAMED_SECTION>"


# options that precede any section header (allow_unnamed_section=True)
UNNAMED_SECTION = _UnnamedSection()

# sentinel for "argument not given"
_UNSET = object()


# --------------------------------------------------------------------------
# interpolation

class Interpolation:
    """No interpolation: values are passed through unchanged.  Subclasses
    hook into reading, writing, setting and getting values."""

    def before_get(self, parser, section, option, value, defaults):
        return value

    def before_set(self, parser, section, option, value):
        return value

    def before_read(self, parser, section, option, value):
        return value

    def before_write(self, parser, section, option, value):
        return value


class BasicInterpolation(Interpolation):
    """``%(name)s`` refers to another option of the same section (or the
    defaults); ``%%`` is a literal percent sign."""

    _REFERENCE = re.compile(r"%\(([^)]+)\)s")

    def before_get(self, parser, section, option, value, defaults):
        pieces = []
        self._expand(parser, option, pieces, value, section, defaults, 1)
        return ''.join(pieces)

    def before_set(self, parser, section, option, value):
        rest = self._REFERENCE.sub('', value.replace('%%', ''))
        if '%' in rest:
            raise ValueError("invalid interpolation syntax in %r at "
                             "position %d" % (value, rest.find('%')))
        return value

    def _expand(self, parser, option, pieces, text, section, lookup, depth):
        raw = parser.get(section, option, raw=True, fallback=text)
        if depth > MAX_INTERPOLATION_DEPTH:
            raise InterpolationDepthError(option, section, raw)
        while text:
            cut = text.find('%')
            if cut < 0:
                pieces.append(text)
                return
            if cut:
                pieces.append(text[:cut])
                text = text[cut:]
            marker = text[1:2]
            if marker == '%':
                pieces.append('%')
                text = text[2:]
            elif marker == '(':
                found = self._REFERENCE.match(text)
                if found is None:
                    raise InterpolationSyntaxError(
                        option, section,
                        "bad interpolation variable reference %r" % text)
                name = parser.optionxform(found.group(1))
                text = text[found.end():]
                try:
                    replacement = lookup[name]
                except KeyError:
                    raise InterpolationMissingOptionError(
                        option, section, raw, name) from None
                if '%' in replacement:
                    self._expand(parser, option, pieces, replacement,
                                 section, lookup, depth + 1)
                else:
                    pieces.append(replacement)
            else:
                raise InterpolationSyntaxError(
                    option, section,
                    "'%%' must be followed by '%%' or '(', found: %r" % text)


class ExtendedInterpolation(Interpolation):
    """``${option}`` refers to an option of the same section,
    ``${section:option}`` to one in any section; ``$$`` is a literal
    dollar sign."""

    _REFERENCE = re.compile(r"\$\{([^}]+)\}")

    def before_get(self, parser, section, option, value, defaults):
        pieces = []
        self._expand(parser, option, pieces, value, section, defaults, 1)
        return ''.join(pieces)

    def before_set(self, parser, section, option, value):
        rest = self._REFERENCE.sub('', value.replace('$$', ''))
        if '$' in rest:
            raise ValueError("invalid interpolation syntax in %r at "
                             "position %d" % (value, rest.find('$')))
        return value

    def _expand(self, parser, option, pieces, text, section, lookup, depth):
        raw = parser.get(section, option, raw=True, fallback=text)
        if depth > MAX_INTERPOLATION_DEPTH:
            raise InterpolationDepthError(option, section, raw)
        while text:
            cut = text.find('$')
            if cut < 0:
                pieces.append(text)
                return
            if cut:
                pieces.append(text[:cut])
                text = text[cut:]
            marker = text[1:2]
            if marker == '$':
                pieces.append('$')
                text = text[2:]
            elif marker == '{':
                found = self._REFERENCE.match(text)
                if found is None:
                    raise InterpolationSyntaxError(
                        option, section,
                        "bad interpolation variable reference %r" % text)
                path = found.group(1).split(':')
                text = text[found.end():]
                where = section
                scope = lookup
                try:
                    if len(path) == 1:
                        name = parser.optionxform(path[0])
                        replacement = lookup[name]
                    elif len(path) == 2:
                        where = path[0]
                        name = parser.optionxform(path[1])
                        replacement = parser.get(where, name, raw=True)
                    else:
                        raise InterpolationSyntaxError(
                            option, section,
                            "More than one ':' found: %r" % (text,))
                except (KeyError, NoSectionError, NoOptionError):
                    raise InterpolationMissingOptionError(
                        option, section, raw, ":".join(path)) from None
                if replacement is None:
                    continue
                if '$' in replacement:
                    if where != section:
                        scope = dict(parser.items(where, raw=True))
                    self._expand(parser, option, pieces, replacement,
                                 where, scope, depth + 1)
                else:
                    pieces.append(replacement)
            else:
                raise InterpolationSyntaxError(
                    option, section,
                    "'$' must be followed by '$' or '{', found: %r" % text)


# --------------------------------------------------------------------------
# parsing helpers

class _ValueLines(list):
    """Lines of a value being read; joined once the source is consumed."""


def _split_option(line, delimiters):
    """Split an option line at its first delimiter.

    Returns (name, value) or (line, None) when there is no delimiter.
    Whitespace around a delimiter is not part of name or value, and a
    whitespace delimiter yields to a visible delimiter that follows it in
    the same run of whitespace.  Runs in linear time.
    """
    visible = [d for d in delimiters if d and not d.isspace()]
    blank = [d for d in delimiters if d and d.isspace()]
    size = len(line)
    pos = 0
    while pos < size:
        if line[pos].isspace():
            end = pos
            while end < size and line[end].isspace():
                end += 1
            hit = _delimiter_at(line, end, visible)
            if hit:
                return line[:pos], line[end + len(hit):]
            run = line[pos:end]
            if end < size and any(d in run for d in blank):
                return line[:pos], line[end:]
            pos = end
            continue
        hit = _delimiter_at(line, pos, visible)
        if hit:
            return line[:pos], line[pos + len(hit):]
        pos += 1
    return line, None


def _delimiter_at(line, pos, delimiters):
    best = ''
    for d in delimiters:
        if len(d) > len(best) and line.startswith(d, pos):
            best = d
    return best


# --------------------------------------------------------------------------
# parsers

class RawConfigParser(MutableMapping):
    """Configuration parser without value interpolation."""

    # a section header line: [name]
    SECTCRE = re.compile(r"\[(?P<header>.+)\]")
    # names of converter getters: get<name>
    _GETTER = re.compile(r"^get(?P<name>.+)$")

    BOOLEAN_STATES = {'1': True, 'yes': True, 'true': True, 'on': True,
                      '0': False, 'no': False, 'false': False, 'off': False}

    _DEFAULT_INTERPOLATION = Interpolation()

    def __init__(self, defaults=None, dict_type=_default_dict,
                 allow_no_value=False, *, delimiters=('=', ':'),
                 comment_prefixes=('#', ';'), inline_comment_prefixes=None,
                 strict=True, empty_lines_in_values=True,
                 default_section=DEFAULTSECT,
                 interpolation=_UNSET, converters=_UNSET,
                 allow_unnamed_section=False):
        self._dict = dict_type
        self._sections = self._dict()
        self._defaults = self._dict()
        self._converters = ConverterMapping(self)
        self._proxies = self._dict()
        self._proxies[default_section] = SectionProxy(self, default_section)
        self._delimiters = tuple(delimiters)
        self._comment_prefixes = tuple(comment_prefixes or ())
        self._inline_comment_prefixes = tuple(inline_comment_prefixes or ())
        self._strict = strict
        self._allow_no_value = allow_no_value
        self._empty_lines_in_values = empty_lines_in_values
        self.default_section = default_section
        self._allow_unnamed_section = allow_unnamed_section
        if interpolation is _UNSET:
            interpolation = self._DEFAULT_INTERPOLATION
        if interpolation is None:
            interpolation = Interpolation()
        if not isinstance(interpolation, Interpolation):
            raise TypeError(
                f"interpolation= must be None or an instance of "
                f"Interpolation; got an object of type {type(interpolation)}")
        self._interpolation = interpolation
        if converters is not _UNSET:
            self._converters.update(converters)
        if defaults:
            self._read_defaults(defaults)

    # -- structure --------------------------------------------------------

    def defaults(self):
        return self._defaults

    def sections(self):
        """Names of the sections (the default section excluded)."""
        return list(self._sections.keys())

    def add_section(self, section):
        """Create an empty section.

        ValueError for the default section's name, DuplicateSectionError if
        the section exists.
        """
        if section == self.default_section:
            raise ValueError('Invalid section name: %r' % section)
        if section is UNNAMED_SECTION and not self._allow_unnamed_section:
            raise UnnamedSectionDisabledError
        if section in self._sections:
            raise DuplicateSectionError(section)
        self._sections[section] = self._dict()
        self._proxies[section] = SectionProxy(self, section)

    def has_section(self, section):
        """Whether the section exists (the default section does not count)."""
        return section in self._sections

    def options(self, section):
        """Option names of the section, defaults included."""
        try:
            merged = self._sections[section].copy()
        except KeyError:
            raise NoSectionError(section) from None
        merged.update(self._defaults)
        return list(merged.keys())

    # -- reading ----------------------------------------------------------

    def read(self, filenames, encoding=None):
        """Read and parse one file name or an iterable of file names.

        Files that cannot be opened are skipped silently; the list of names
        that were parsed is returned.
        """
        if isinstance(filenames, (str, bytes, os.PathLike)):
            filenames = [filenames]
        encoding = io.text_encoding(encoding)
        done = []
        for filename in filenames:
            try:
                with open(filename, encoding=encoding) as fp:
                    self._read(fp, filename)
            except OSError:
                continue
            if isinstance(filename, os.PathLike):
                filename = os.fspath(filename)
            done.append(filename)
        return done

    def read_file(self, f, source=None):
        """Parse configuration lines from an iterable of strings (such as a
        text file).  *source* names it in error messages (default: its
        ``name`` attribute, or '<???>')."""
        if source is None:
            source = getattr(f, 'name', '<???>')
        self._read(f, source)

    def read_string(self, string, source='<string>'):
        """Parse configuration text from a string."""
        self.read_file(io.StringIO(string), source)

    def read_dict(self, dictionary, source='<dict>'):
        """Load sections from a mapping of section names to mappings of
        option names to values.  Keys and values are converted to str
        (None values are kept when allowed)."""
        seen = set()
        for section, options in dictionary.items():
            if section is not UNNAMED_SECTION:
                section = str(section)
            try:
                self.add_section(section)
            except (DuplicateSectionError, ValueError):
                if self._strict and section in seen:
                    raise
            seen.add(section)
            for key, value in options.items():
                key = self.optionxform(str(key))
                if value is not None:
                    value = str(value)
                if self._strict and (section, key) in seen:
                    raise DuplicateOptionError(section, key, source)
                seen.add((section, key))
                self.set(section, key, value)

    def _read_defaults(self, defaults):
        for key, value in defaults.items():
            self._defaults[self.optionxform(key)] = value

    # -- getting ----------------------------------------------------------

    def get(self, section, option, *, raw=False, vars=None, fallback=_UNSET):
        """Value of *option* in *section*, looked up in *vars*, the section
        and the defaults (in that order) and interpolated unless *raw*.

        With *fallback* given, it is returned instead of raising
        NoSectionError / NoOptionError.
        """
        try:
            lookup = self._unify_values(section, vars)
        except NoSectionError:
            if fallback is _UNSET:
                raise
            return fallback
        option = self.optionxform(option)
        try:
            value = lookup[option]
        except KeyError:
            if fallback is _UNSET:
                raise NoOptionError(option, section)
            return fallback
        if raw or value is None:
            return value
        return self._interpolation.before_get(self, section, option, value,
                                              lookup)

    def _get(self, section, conv, option, **kwargs):
        return conv(self.get(section, option, **kwargs))

    def _get_conv(self, section, option, conv, *, raw=False, vars=None,
                  fallback=_UNSET, **kwargs):
        try:
            return self._get(section, conv, option, raw=raw, vars=vars,
                             **kwargs)
        except (NoSectionError, NoOptionError):
            if fallback is _UNSET:
                raise
            return fallback

    def getint(self, section, option, *, raw=False, vars=None,
               fallback=_UNSET, **kwargs):
        return self._get_conv(section, option, int, raw=raw, vars=vars,
                              fallback=fallback, **kwargs)

    def getfloat(self, section, option, *, raw=False, vars=None,
                 fallback=_UNSET, **kwargs):
        return self._get_conv(section, option, float, raw=raw, vars=vars,
                              fallback=fallback, **kwargs)

    def getboolean(self, section, option, *, raw=False, vars=None,
                   fallback=_UNSET, **kwargs):
        return self._get_conv(section, option, self._convert_to_boolean,
                              raw=raw, vars=vars, fallback=fallback, **kwargs)

    def items(self, section=_UNSET, raw=False, vars=None):
        """Without arguments: (name, SectionProxy) pairs of all sections,
        default section first.  With a section: its (option, value) pairs,
        defaults included and values interpolated unless *raw*."""
        if section is _UNSET:
            return super().items()
        merged = self._defaults.copy()
        try:
            merged.update(self._sections[section])
        except KeyError:
            if section != self.default_section:
                raise NoSectionError(section)
        names = list(merged.keys())
        if vars:
            for key, value in vars.items():
                merged[self.optionxform(key)] = value

        def value_of(option):
            value = merged[option]
            if raw or value is None:
                return value
            return self._interpolation.before_get(self, section, option,
                                                  value, merged)
        return [(option, value_of(option)) for option in names]

    def popitem(self):
        """Remove the first section and return (name, SectionProxy)."""
        for name in self.sections():
            proxy = self[name]
            del self[name]
            return name, proxy
        raise KeyError

    def optionxform(self, optionstr):
        """Normalise an option name (lower-cased by default)."""
        return optionstr.lower()

    def has_option(self, section, option):
        """Whether *option* exists in *section* or the defaults (an empty
        section name or the default section's name checks the defaults
        only)."""
        if not section or section == self.default_section:
            return self.optionxform(option) in self._defaults
        if section not in self._sections:
            return False
        option = self.optionxform(option)
        return option in self._sections[section] or option in self._defaults

    # -- setting ----------------------------------------------------------

    def set(self, section, option, value=None):
        """Set *option* in *section* (or in the defaults for an empty name or
        the default section)."""
        if value:
            value = self._interpolation.before_set(self, section, option,
                                                   value)
        if not section or section == self.default_section:
            target = self._defaults
        else:
            try:
                target = self._sections[section]
            except KeyError:
                raise NoSectionError(section) from None
        target[self.optionxform(option)] = value

    def remove_option(self, section, option):
        """Delete an option; returns whether it existed."""
        if not section or section == self.default_section:
            target = self._defaults
        else:
            try:
                target = self._sections[section]
            except KeyError:
                raise NoSectionError(section) from None
        option = self.optionxform(option)
        present = option in target
        if present:
            del target[option]
        return present

    def remove_section(self, section):
        """Delete a section; returns whether it existed."""
        present = section in self._sections
        if present:
            del self._sections[section]
            del self._proxies[section]
        return present

    # -- writing ----------------------------------------------------------

    def write(self, fp, space_around_delimiters=True):
        """Write the configuration in INI format to the text file *fp*.

        Multi-line values are written as indented continuation lines.
        """
        delimiter = self._delimiters[0]
        if space_around_delimiters:
            delimiter = f" {delimiter} "
        if UNNAMED_SECTION in self._sections and \
                self._sections[UNNAMED_SECTION]:
            self._write_section(fp, UNNAMED_SECTION,
                                self._sections[UNNAMED_SECTION].items(),
                                delimiter, unnamed=True)
        if self._defaults:
            self._write_section(fp, self.default_section,
                                self._defaults.items(), delimiter)
        for section in self._sections:
            if section is UNNAMED_SECTION:
                continue
            self._write_section(fp, section,
                                self._sections[section].items(), delimiter)

    def _write_section(self, fp, name, items, delimiter, unnamed=False):
        if not unnamed:
            fp.write(f"[{name}]\n")
        for key, value in items:
            self._validate_key_contents(key)
            value = self._interpolation.before_write(self, name, key, value)
            if value is not None or not self._allow_no_value:
                text = str(value).replace('\r\n', '\n').replace('\r', '\n')
                value = delimiter + text.replace('\n', '\n\t')
            else:
                value = ''
            fp.write(f"{key}{value}\n")
        fp.write("\n")

    def _validate_key_contents(self, key):
        """Refuse keys that would not read back as written."""
        if self.SECTCRE.match(key.strip()):
            raise InvalidWriteError(
                f"Cannot write key {key}; begins with section pattern")
        for delimiter in self._delimiters:
            if delimiter in key:
                raise InvalidWriteError(
                    f"Cannot write key {key}; contains delimiter "
                    f"{delimiter}")

    # -- mapping protocol -------------------------------------------------

    def __getitem__(self, key):
        if key != self.default_section and not self.has_section(key):
            raise KeyError(key)
        return self._proxies[key]

    def __setitem__(self, key, value):
        # assigning a section's own proxy to it is a no-op
        if key in self and self[key] is value:
            return
        if key == self.default_section:
            self._defaults.clear()
        elif key in self._sections:
            self._sections[key].clear()
        self.read_dict({key: value})

    def __delitem__(self, key):
        if key == self.default_section:
            raise ValueError("Cannot remove the default section.")
        if not self.has_section(key):
            raise KeyError(key)
        self.remove_section(key)

    def __contains__(self, key):
        return key == self.default_section or self.has_section(key)

    def __len__(self):
        return len(self._sections) + 1

    def __iter__(self):
        return itertools.chain((self.default_section,), self._sections.keys())

    # -- the reader -------------------------------------------------------

    def _comment_start(self, line):
        """Index where a comment starts in *line*, or None."""
        stripped = line.strip()
        if any(stripped.startswith(p) for p in self._comment_prefixes):
            return 0
        start = None
        for prefix in self._inline_comment_prefixes:
            if not prefix:
                continue
            at = line.find(prefix)
            while at != -1:
                if at == 0 or line[at - 1].isspace():
                    if start is None or at < start:
                        start = at
                    break
                at = line.find(prefix, at + 1)
        return start

    def _read(self, fp, fpname):
        """Parse the lines of *fp*; *fpname* names the source in errors.

        Values may continue on following lines that are indented deeper
        than the option's own line.  Problems that leave the parser usable
        are collected and raised together as one ParsingError at the end.
        """
        problems = []
        try:
            self._read_lines(fp, fpname, problems)
        finally:
            self._join_multiline_values()
        if problems:
            raise ParsingError(fpname, *problems)

    def _read_lines(self, fp, fpname, problems):
        seen = set()
        current = None          # option dict of the current section
        sectname = None
        optname = None
        indent_level = 0
        for lineno, line in enumerate(fp, start=1):
            comment_at = self._comment_start(line)
            text = (line if comment_at is None else line[:comment_at]).strip()
            if not text:
                if self._empty_lines_in_values:
                    # blank lines (not comments) belong to a value being
                    # continued
                    if (comment_at is None and current is not None and
                            optname and current[optname] is not None):
                        current[optname].append('')
                else:
                    indent_level = sys.maxsize
                continue
            indent = len(line) - len(line.lstrip())
            if (current is not None and optname and
                    indent > indent_level):
                if current[optname] is None:
                    raise MultilineContinuationError(fpname, lineno, line)
                current[optname].append(text)
                continue
            indent_level = indent
            header = self.SECTCRE.match(text)
            if header:
                sectname = header.group('header')
                if sectname in self._sections:
                    if self._strict and sectname in seen:
                        raise DuplicateSectionError(sectname, fpname, lineno)
                    current = self._sections[sectname]
                    seen.add(sectname)
                elif sectname == self.default_section:
                    current = self._defaults
                else:
                    current = self._dict()
                    self._sections[sectname] = current
                    self._proxies[sectname] = SectionProxy(self, sectname)
                    seen.add(sectname)
                optname = None
                continue
            if current is None:
                if not self._allow_unnamed_section:
                    raise MissingSectionHeaderError(fpname, lineno, line)
                sectname = UNNAMED_SECTION
                if UNNAMED_SECTION not in self._sections:
                    self._sections[UNNAMED_SECTION] = self._dict()
                    self._proxies[UNNAMED_SECTION] = SectionProxy(
                        self, UNNAMED_SECTION)
                current = self._sections[UNNAMED_SECTION]
                seen.add(UNNAMED_SECTION)
            name, value = _split_option(text, self._delimiters)
            name = name.rstrip()
            if not name or (value is None and not self._allow_no_value):
                problems.append((lineno, line))
                optname = None
                continue
            optname = self.optionxform(name)
            if self._strict and (sectname, optname) in seen:
                raise DuplicateOptionError(sectname, optname, fpname, lineno)
            seen.add((sectname, optname))
            if value is None:
                current[optname] = None
            else:
                current[optname] = _ValueLines([value.strip()])

    def _join_multiline_values(self):
        everything = itertools.chain(((self.default_section, self._defaults),),
                                     self._sections.items())
        for section, options in everything:
            for name, value in options.items():
                if isinstance(value, _ValueLines):
                    joined = '\n'.join(value).rstrip()
                    options[name] = self._interpolation.before_read(
                        self, section, name, joined)

    # -- helpers ----------------------------------------------------------

    def _unify_values(self, section, vars):
        """ChainMap of *vars*, the section and the defaults."""
        own = {}
        try:
            own = self._sections[section]
        except KeyError:
            if section != self.default_section:
                raise NoSectionError(section) from None
        extra = {}
        if vars:
            for key, value in vars.items():
                if value is not None:
                    value = str(value)
                extra[self.optionxform(key)] = value
        return _ChainMap(extra, own, self._defaults)

    def _convert_to_boolean(self, value):
        """Map yes/no, on/off, true/false, 1/0 (any case) to a bool."""
        try:
            return self.BOOLEAN_STATES[value.lower()]
        except KeyError:
            raise ValueError('Not a boolean: %s' % value) from None

    def _validate_value_types(self, *, section="", option="", value=""):
        """Section names, option names and values must be str (values may
        be None when allow_no_value is set)."""
        if section is not UNNAMED_SECTION and not isinstance(section, str):
            raise TypeError("section names must be strings")
        if not isinstance(option, str):
            raise TypeError("option keys must be strings")
        if not self._allow_no_value or value:
            if not isinstance(value, str):
                raise TypeError("option values must be strings")

    @property
    def converters(self):
        return self._converters


class ConfigParser(RawConfigParser):
    """Configuration parser with ``%(name)s`` interpolation by default and
    str-only values."""

    _DEFAULT_INTERPOLATION = BasicInterpolation()

    def set(self, section, option, value=None):
        """Like RawConfigParser.set(), but values must be strings."""
        self._validate_value_types(option=option, value=value)
        super().set(section, option, value)

    def add_section(self, section):
        """Like RawConfigParser.add_section(), but the name must be a
        string."""
        self._validate_value_types(section=section)
        super().add_section(section)

    def _read_defaults(self, defaults):
        # defaults are stored as given, without interpolation checks
        saved = self._interpolation
        self._interpolation = Interpolation()
        try:
            self.read_dict({self.default_section: defaults})
        finally:
            self._interpolation = saved


class SectionProxy(MutableMapping):
    """Mapping view of one section of a parser."""

    def __init__(self, parser, name):
        self._parser = parser
        self._name = name
        for conv in parser.converters:
            key = 'get' + conv
            getter = functools.partial(self.get, _impl=getattr(parser, key))
            setattr(self, key, getter)

    def __repr__(self):
        return f'<Section: {self._name}>'

    def __getitem__(self, key):
        if not self._parser.has_option(self._name, key):
            raise KeyError(key)
        return self._parser.get(self._name, key)

    def __setitem__(self, key, value):
        self._parser._validate_value_types(option=key, value=value)
        return self._parser.set(self._name, key, value)

    def __delitem__(self, key):
        if not (self._parser.has_option(self._name, key) and
                self._parser.remove_option(self._name, key)):
            raise KeyError(key)

    def __contains__(self, key):
        return self._parser.has_option(self._name, key)

    def __len__(self):
        return len(self._options())

    def __iter__(self):
        return iter(self._options())

    def _options(self):
        if self._name != self._parser.default_section:
            return self._parser.options(self._name)
        return self._parser.defaults()

    @property
    def parser(self):
        return self._parser

    @property
    def name(self):
        return self._name

    def get(self, option, fallback=None, *, raw=False, vars=None,
            _impl=None, **kwargs):
        """Value of *option* (see RawConfigParser.get()); *fallback*
        defaults to None here."""
        if not _impl:
            _impl = self._parser.get
        return _impl(self._name, option, raw=raw, vars=vars,
                     fallback=fallback, **kwargs)


class ConverterMapping(MutableMapping):
    """The ``converters`` of a parser: name -> conversion callable.

    Adding an entry ``name`` creates ``get<name>()`` methods on the parser
    and its section proxies; removing it removes them.  Getters defined by
    the parser class itself appear with the value None.
    """

    def __init__(self, parser):
        self._parser = parser
        self._data = {}
        for attr in dir(self._parser):
            found = RawConfigParser._GETTER.match(attr)
            if found and callable(getattr(self._parser, attr)):
                self._data[found.group('name')] = None

    def __getitem__(self, key):
        return self._data[key]

    def __setitem__(self, key, value):
        try:
            attr = 'get' + key
        except TypeError:
            raise ValueError(f'Incompatible key: {key} '
                             f'(type: {type(key)})') from None
        if attr == 'get':
            raise ValueError('Incompatible key: cannot use "" as a name')
        self._data[key] = value
        method = functools.partial(self._parser._get_conv, conv=value)
        method.converter = value
        setattr(self._parser, attr, method)
        for proxy in self._parser.values():
            setattr(proxy, attr, functools.partial(proxy.get, _impl=method))

    def __delitem__(self, key):
        try:
            attr = 'get' + (key or None)
        except TypeError:
            raise KeyError(key) from None
        del self._data[key]
        for holder in itertools.chain((self._parser,),
                                      self._parser.values()):
            try:
                delattr(holder, attr)
            except AttributeError:
                continue

    def __iter__(self):
        return iter(self._data)

    def __len__(self):
        return len(self._data)
