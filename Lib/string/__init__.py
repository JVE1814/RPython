"""String constants, capwords(), and the Template and Formatter classes.

Constants (ASCII only): whitespace, ascii_lowercase, ascii_uppercase,
ascii_letters, digits, hexdigits, octdigits, punctuation, printable.

Template performs $-substitution ($name, ${name}, $$ for a literal $);
Formatter exposes the machinery of str.format() as overridable methods.
"""

__all__ = ["ascii_letters", "ascii_lowercase", "ascii_uppercase", "capwords",
           "digits", "hexdigits", "octdigits", "printable", "punctuation",
           "whitespace", "Formatter", "Template"]

import _string

whitespace = ' \t\n\r\v\f'
ascii_lowercase = 'abcdefghijklmnopqrstuvwxyz'
ascii_uppercase = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ'
ascii_letters = ascii_lowercase + ascii_uppercase
digits = '0123456789'
hexdigits = digits + 'abcdef' + 'ABCDEF'
octdigits = '01234567'
punctuation = r"""!"#$%&'()*+,-./:;<=>?@[\]^_`{|}~"""
printable = digits + ascii_letters + punctuation + whitespace


def capwords(s, sep=None):
    """Split *s* with s.split(sep), capitalize each piece and join them with
    *sep* (one space when sep is None, so that runs of whitespace collapse
    and leading/trailing whitespace disappears)."""
    return (sep or ' ').join(word.capitalize() for word in s.split(sep))


# --------------------------------------------------------------------------
# Template

# Default for the positional mapping argument of the substitute methods.
_NO_MAPPING = {}


class _KeywordsFirst:
    """Mapping view: the keyword arguments shadow the positional mapping."""

    __slots__ = ('keywords', 'mapping')

    def __init__(self, keywords, mapping):
        self.keywords = keywords
        self.mapping = mapping

    def __getitem__(self, key):
        if key in self.keywords:
            return self.keywords[key]
        return self.mapping[key]


def _lookup_source(mapping, keywords):
    if mapping is _NO_MAPPING:
        return keywords
    if keywords:
        return _KeywordsFirst(keywords, mapping)
    return mapping


def _default_pattern_source(cls):
    import re
    start = re.escape(cls.delimiter)
    plain = cls.idpattern
    braced = cls.braceidpattern or cls.idpattern
    # verbose regex: the four alternatives after a delimiter
    return (f'{start} (?:\n'
            f'    (?P<escaped>{start})\n'          # the delimiter twice
            f'  | (?P<named>{plain})\n'            # $identifier
            f'  | {{ (?P<braced>{braced}) }}\n'    # ${identifier}
            f'  | (?P<invalid>)\n'                 # a delimiter alone
            f')')


def _compile_for(cls, source):
    import re
    if isinstance(source, re.Pattern):
        return source
    flags = re.IGNORECASE if cls.flags is None else cls.flags
    return re.compile(source, flags | re.VERBOSE)


class _PatternOnDemand:
    """Template.pattern until first use: compiling it at import time would
    mean importing re.  Subclasses get their pattern when created."""

    def __get__(self, instance, owner):
        compiled = _compile_for(owner, _default_pattern_source(owner))
        owner.pattern = compiled
        return compiled


# the on-demand marker itself (a subclass can be re-armed with it)
_TemplatePattern = _PatternOnDemand()


class Template:
    """A string containing $-placeholders, see substitute()."""

    delimiter = '$'
    # identifiers are ASCII; matching ignores case unless flags says otherwise
    idpattern = r'(?a:[_a-z][_a-z0-9]*)'
    braceidpattern = None
    flags = None          # None stands for re.IGNORECASE
    pattern = _TemplatePattern

    def __init_subclass__(cls):
        super().__init_subclass__()
        if 'pattern' in cls.__dict__:
            source = cls.__dict__['pattern']
        else:
            source = _default_pattern_source(cls)
        cls.pattern = _compile_for(cls, source)

    def __init__(self, template):
        self.template = template

    def _placeholder_error(self, match):
        # The 'invalid' group is empty and normally sits right after the
        # delimiter, so the number of characters before it on its line is
        # the delimiter's 1-based column.
        index = match.start('invalid')
        # The sentinel makes the last row the one holding the placeholder.
        rows = (self.template[:index] + '#').splitlines()
        column = max(1, len(rows[-1]) - 1)
        raise ValueError(f'Invalid placeholder in string: '
                         f'line {len(rows)}, col {column}')

    def _unknown_match(self):
        return ValueError('the Template pattern matched without setting any '
                          'of the escaped/named/braced/invalid groups',
                          self.pattern)

    def substitute(self, mapping=_NO_MAPPING, /, **kws):
        """Return the template with every placeholder replaced by the value
        from *mapping* and/or the keyword arguments (keywords win).

        A missing key raises KeyError, a malformed placeholder ValueError.
        """
        source = _lookup_source(mapping, kws)

        def replace(match):
            name = match['named'] or match['braced']
            if name is not None:
                return str(source[name])
            if match['escaped'] is not None:
                return self.delimiter
            if match['invalid'] is not None:
                self._placeholder_error(match)
            raise self._unknown_match()

        return self.pattern.sub(replace, self.template)

    def safe_substitute(self, mapping=_NO_MAPPING, /, **kws):
        """Like substitute(), but placeholders that cannot be filled (missing
        key or malformed) are left in the result unchanged."""
        source = _lookup_source(mapping, kws)

        def replace(match):
            name = match['named'] or match['braced']
            if name is not None:
                try:
                    return str(source[name])
                except KeyError:
                    return match[0]
            if match['escaped'] is not None:
                return self.delimiter
            if match['invalid'] is not None:
                return match[0]
            raise self._unknown_match()

        return self.pattern.sub(replace, self.template)

    def is_valid(self):
        """True unless the template contains a malformed placeholder."""
        for match in self.pattern.finditer(self.template):
            if match['invalid'] is not None:
                return False
            if (match['named'] is None and match['braced'] is None
                    and match['escaped'] is None):
                raise self._unknown_match()
        return True

    def get_identifiers(self):
        """The placeholder names used in the template, in order of first
        appearance, without duplicates."""
        seen = {}
        for match in self.pattern.finditer(self.template):
            name = match['named'] or match['braced']
            if name is not None:
                seen.setdefault(name, None)
            elif match['invalid'] is None and match['escaped'] is None:
                raise self._unknown_match()
        return list(seen)


# --------------------------------------------------------------------------
# Formatter

class Formatter:
    """str.format() split into methods that subclasses can override:
    parse() finds the fields, get_field()/get_value() look the arguments
    up, convert_field() applies !r/!s/!a, format_field() calls format(),
    check_unused_args() may reject unused arguments."""

    def format(self, format_string, /, *args, **kwargs):
        return self.vformat(format_string, args, kwargs)

    def vformat(self, format_string, args, kwargs):
        used = set()
        text, _ = self._vformat(format_string, args, kwargs, used, 2)
        self.check_unused_args(used, args, kwargs)
        return text

    def _vformat(self, format_string, args, kwargs, used_args, recursion_depth,
                 auto_arg_index=0):
        # Returns (text, numbering state).  The state is the next automatic
        # field number, or None once explicit numbers were used; the two
        # styles must not be mixed.  Format specs are expanded recursively,
        # at most *recursion_depth* levels deep.
        if recursion_depth < 0:
            raise ValueError('format specifications nest too deeply '
                             '(recursion limit reached)')
        pieces = []
        for literal, field, spec, conversion in self.parse(format_string):
            if literal:
                pieces.append(literal)
            if field is None:
                continue
            field, auto_arg_index = _number_field(field, auto_arg_index)
            value, key = self.get_field(field, args, kwargs)
            used_args.add(key)
            value = self.convert_field(value, conversion)
            spec, auto_arg_index = self._vformat(
                spec, args, kwargs, used_args, recursion_depth - 1,
                auto_arg_index)
            pieces.append(self.format_field(value, spec))
        return ''.join(pieces), auto_arg_index

    def get_value(self, key, args, kwargs):
        """Positional argument *key* (an int) or keyword argument *key*."""
        if isinstance(key, int):
            return args[key]
        return kwargs[key]

    def check_unused_args(self, used_args, args, kwargs):
        """Hook called after formatting; by default it accepts anything."""

    def format_field(self, value, format_spec):
        return format(value, format_spec)

    def convert_field(self, value, conversion):
        if conversion is None:
            return value
        converter = _CONVERSIONS.get(conversion)
        if converter is None:
            raise ValueError(f'Unknown conversion specifier {conversion!s}')
        return converter(value)

    def parse(self, format_string):
        """Iterate over (literal_text, field_name, format_spec, conversion);
        field_name is None for trailing text without a field."""
        return _string.formatter_parser(format_string)

    def get_field(self, field_name, args, kwargs):
        """Resolve a field such as '0.attr[key]'; return (object, key of the
        argument it started from)."""
        first, accessors = _string.formatter_field_name_split(field_name)
        value = self.get_value(first, args, kwargs)
        for is_attribute, name in accessors:
            value = getattr(value, name) if is_attribute else value[name]
        return value, first


_CONVERSIONS = {'s': str, 'r': repr, 'a': ascii}


def _number_field(field, state):
    """Give an automatically numbered field ('{}', '{.x}', '{[0]}') its
    number; return (field, new numbering state)."""
    argument = field
    for stop in '.[':
        argument = argument.partition(stop)[0]
    if argument == '':
        if state is None:
            raise ValueError('cannot switch from manual field specification '
                             'to automatic field numbering')
        return f'{state}{field}', state + 1
    if argument.isdigit():
        if state:
            raise ValueError('cannot switch from automatic field numbering '
                             'to manual field specification')
        return field, None
    return field, state
