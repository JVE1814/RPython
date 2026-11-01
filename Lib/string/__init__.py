"""Common string constants and classes.

Constants: whitespace, ascii_lowercase, ascii_uppercase, ascii_letters,
digits, hexdigits, octdigits, punctuation, printable.

Classes: Formatter (customizable str.format) and Template ($-substitution).
Function: capwords.
"""

__all__ = ["ascii_letters", "ascii_lowercase", "ascii_uppercase", "capwords",
           "digits", "hexdigits", "octdigits", "printable", "punctuation",
           "whitespace", "Formatter", "Template"]

import _string

# Character classes (ASCII only)
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
    """Capitalize each word of s.

    s is split with s.split(sep), every word is capitalized and the words
    are joined with sep (a single space when sep is None, which also
    collapses runs of whitespace and strips the ends).
    """
    return (sep or ' ').join(map(str.capitalize, s.split(sep)))


####################################################################
# $-substitution templates

_sentinel_dict = {}


class _TemplatePattern:
    """Marker for 'the default pattern, built from delimiter/idpattern on
    first use' (so that importing string does not import re)."""


class _MergedMapping:
    """Look keys up in `primary` first, then in `fallback`."""

    def __init__(self, primary, fallback):
        self.primary = primary
        self.fallback = fallback

    def __getitem__(self, key):
        try:
            return self.primary[key]
        except KeyError:
            return self.fallback[key]


class Template:
    """A string with $-substitution placeholders ($name, ${name}, $$)."""

    delimiter = '$'
    # ASCII-only identifiers; the pattern is case insensitive by default.
    idpattern = r'(?a:[_a-z][_a-z0-9]*)'
    braceidpattern = None
    # None means re.IGNORECASE.
    flags = None
    pattern = _TemplatePattern

    def __init_subclass__(cls):
        super().__init_subclass__()
        cls._compile_pattern()

    @classmethod
    def _compile_pattern(cls):
        import re
        pattern = cls.pattern
        if pattern is not _TemplatePattern and not isinstance(pattern, str):
            # Already a compiled pattern object.
            return pattern
        if pattern is _TemplatePattern:
            delim = re.escape(cls.delimiter)
            ident = cls.idpattern
            bident = cls.braceidpattern or cls.idpattern
            pattern = (
                f'{delim}(?:\n'
                f'  (?P<escaped>{delim})   |  # doubled delimiter\n'
                f'  (?P<named>{ident})      |  # delimiter + identifier\n'
                f'  {{(?P<braced>{bident})}} |  # delimiter + {{identifier}}\n'
                f'  (?P<invalid>)             # anything else is an error\n'
                f')\n'
            )
        flags = cls.flags
        if flags is None:
            flags = re.IGNORECASE
        compiled = re.compile(pattern, flags | re.VERBOSE)
        cls.pattern = compiled
        return compiled

    @classmethod
    def _get_pattern(cls):
        pattern = cls.pattern
        if pattern is _TemplatePattern or isinstance(pattern, str):
            pattern = cls._compile_pattern()
        return pattern

    def __init__(self, template):
        self.template = template

    def _invalid(self, mo):
        i = mo.start('invalid')
        lines = self.template[:i].splitlines(keepends=True)
        if not lines:
            colno = 1
            lineno = 1
        else:
            colno = i - len(''.join(lines[:-1]))
            lineno = len(lines)
        raise ValueError('Invalid placeholder in string: line %d, col %d' %
                         (lineno, colno))

    def substitute(self, mapping=_sentinel_dict, /, **kws):
        if mapping is _sentinel_dict:
            mapping = kws
        elif kws:
            mapping = _MergedMapping(kws, mapping)

        def convert(mo):
            named = mo.group('named') or mo.group('braced')
            if named is not None:
                return str(mapping[named])
            if mo.group('escaped') is not None:
                return self.delimiter
            if mo.group('invalid') is not None:
                self._invalid(mo)
            raise ValueError('Unrecognized named group in pattern',
                             self.pattern)
        return self._get_pattern().sub(convert, self.template)

    def safe_substitute(self, mapping=_sentinel_dict, /, **kws):
        if mapping is _sentinel_dict:
            mapping = kws
        elif kws:
            mapping = _MergedMapping(kws, mapping)

        def convert(mo):
            named = mo.group('named') or mo.group('braced')
            if named is not None:
                try:
                    return str(mapping[named])
                except KeyError:
                    return mo.group()
            if mo.group('escaped') is not None:
                return self.delimiter
            if mo.group('invalid') is not None:
                return mo.group()
            raise ValueError('Unrecognized named group in pattern',
                             self.pattern)
        return self._get_pattern().sub(convert, self.template)

    def is_valid(self):
        for mo in self._get_pattern().finditer(self.template):
            if mo.group('invalid') is not None:
                return False
            if (mo.group('named') is None
                    and mo.group('braced') is None
                    and mo.group('escaped') is None):
                # The pattern matched but none of the known groups did.
                raise ValueError('Unrecognized named group in pattern',
                                 self.pattern)
        return True

    def get_identifiers(self):
        ids = []
        for mo in self._get_pattern().finditer(self.template):
            named = mo.group('named') or mo.group('braced')
            if named is not None and named not in ids:
                ids.append(named)
            elif (named is None
                    and mo.group('invalid') is None
                    and mo.group('escaped') is None):
                raise ValueError('Unrecognized named group in pattern',
                                 self.pattern)
        return ids


########################################################################
# The Formatter class: str.format() as overridable methods.

class Formatter:
    def format(self, format_string, /, *args, **kwargs):
        return self.vformat(format_string, args, kwargs)

    def vformat(self, format_string, args, kwargs):
        used_args = set()
        result, _ = self._vformat(format_string, args, kwargs, used_args, 2)
        self.check_unused_args(used_args, args, kwargs)
        return result

    def _vformat(self, format_string, args, kwargs, used_args, recursion_depth,
                 auto_arg_index=0):
        if recursion_depth < 0:
            raise ValueError('Max string recursion exceeded')
        result = []
        for literal_text, field_name, format_spec, conversion in \
                self.parse(format_string):

            if literal_text:
                result.append(literal_text)

            if field_name is not None:
                # Automatic ('{}') and manual ('{0}') numbering cannot be
                # mixed; auto_arg_index is False once manual numbering is used.
                if field_name == '':
                    if auto_arg_index is False:
                        raise ValueError('cannot switch from manual field '
                                         'specification to automatic field '
                                         'numbering')
                    field_name = str(auto_arg_index)
                    auto_arg_index += 1
                elif field_name.isdigit():
                    if auto_arg_index:
                        raise ValueError('cannot switch from automatic field '
                                         'numbering to manual field '
                                         'specification')
                    auto_arg_index = False

                obj, arg_used = self.get_field(field_name, args, kwargs)
                used_args.add(arg_used)
                obj = self.convert_field(obj, conversion)

                # The format spec may itself contain replacement fields.
                format_spec, auto_arg_index = self._vformat(
                    format_spec, args, kwargs,
                    used_args, recursion_depth - 1,
                    auto_arg_index=auto_arg_index)

                result.append(self.format_field(obj, format_spec))

        return ''.join(result), auto_arg_index

    def get_value(self, key, args, kwargs):
        if isinstance(key, int):
            return args[key]
        return kwargs[key]

    def check_unused_args(self, used_args, args, kwargs):
        pass

    def format_field(self, value, format_spec):
        return format(value, format_spec)

    def convert_field(self, value, conversion):
        if conversion is None:
            return value
        if conversion == 's':
            return str(value)
        if conversion == 'r':
            return repr(value)
        if conversion == 'a':
            return ascii(value)
        raise ValueError("Unknown conversion specifier {0!s}".format(conversion))

    def parse(self, format_string):
        """Return an iterable of (literal_text, field_name, format_spec,
        conversion) tuples; field_name is None after the last field."""
        return _string.formatter_parser(format_string)

    def get_field(self, field_name, args, kwargs):
        """Resolve field_name (e.g. '0.attr[key]') to an object; return
        (object, first_key) where first_key is recorded as used."""
        first, rest = _string.formatter_field_name_split(field_name)
        obj = self.get_value(first, args, kwargs)
        for is_attr, i in rest:
            if is_attr:
                obj = getattr(obj, i)
            else:
                obj = obj[i]
        return obj, first
