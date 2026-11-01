"""Policy framework of the email package, and the backward compatible
Compat32 policy.

Policies are immutable objects; clone() (or adding two policies) makes a
new one with some attributes changed.
"""

import abc
from email import header
from email import charset as _charset
from email.utils import _has_surrogates

__all__ = [
    'Policy',
    'Compat32',
    'compat32',
]


class _PolicyBase:
    """Immutable attribute container.

    Attributes are declared at class level with their defaults; instances
    only store the attributes that differ.
    """

    def __init__(self, **kw):
        for name, value in kw.items():
            if not hasattr(self, name):
                raise TypeError(
                    "{!r} is an invalid keyword argument for {}".format(
                        name, self.__class__.__name__))
            object.__setattr__(self, name, value)

    def __repr__(self):
        settings = ', '.join('{}={!r}'.format(name, value)
                             for name, value in self.__dict__.items())
        return "{}({})".format(self.__class__.__name__, settings)

    def clone(self, **kw):
        """Return a copy of this policy with the given attributes changed."""
        cls = self.__class__
        new = cls.__new__(cls)
        for name, value in self.__dict__.items():
            object.__setattr__(new, name, value)
        for name, value in kw.items():
            if not hasattr(self, name):
                raise TypeError(
                    "{!r} is an invalid keyword argument for {}".format(
                        name, cls.__name__))
            object.__setattr__(new, name, value)
        return new

    def __setattr__(self, name, value):
        if hasattr(self, name):
            message = "{!r} object attribute {!r} is read-only"
        else:
            message = "{!r} object has no attribute {!r}"
        raise AttributeError(message.format(self.__class__.__name__, name))

    def __add__(self, other):
        """The settings of *other* override those of self."""
        return self.clone(**other.__dict__)


class Policy(_PolicyBase, metaclass=abc.ABCMeta):
    """Controls how messages are parsed, represented and serialized.

    raise_on_defect -- raise defects instead of recording them
    linesep -- line separator used when serializing
    cte_type -- '7bit' or '8bit': what the transport can carry
    max_line_length -- folding limit for headers (0/None: no limit)
    mangle_from_ -- escape body lines starting with 'From '
    message_factory -- class used for new messages (None: Message)
    verify_generated_headers -- refuse to emit headers that could be
                                misparsed
    """

    raise_on_defect = False
    linesep = '\n'
    cte_type = '8bit'
    max_line_length = 78
    mangle_from_ = False
    message_factory = None
    verify_generated_headers = True

    def handle_defect(self, obj, defect):
        """Raise *defect* if raise_on_defect is set, else register it."""
        if self.raise_on_defect:
            raise defect
        self.register_defect(obj, defect)

    def register_defect(self, obj, defect):
        """Record *defect* on *obj* (appends to obj.defects)."""
        obj.defects.append(defect)

    def header_max_count(self, name):
        """How many headers called *name* a message may have (None: any)."""
        return None

    @abc.abstractmethod
    def header_source_parse(self, sourcelines):
        """Turn the raw source lines of a header into (name, value)."""
        raise NotImplementedError

    @abc.abstractmethod
    def header_store_parse(self, name, value):
        """Turn an application supplied (name, value) into what is stored."""
        raise NotImplementedError

    @abc.abstractmethod
    def header_fetch_parse(self, name, value):
        """Turn a stored value into what the application gets."""
        raise NotImplementedError

    @abc.abstractmethod
    def fold(self, name, value):
        """Serialize a header as text, folded, with the final linesep."""
        raise NotImplementedError

    @abc.abstractmethod
    def fold_binary(self, name, value):
        """Like fold(), but return bytes."""
        raise NotImplementedError


class Compat32(Policy):
    """The behaviour of the email package in Python 3.2."""

    mangle_from_ = True

    def _sanitize_header(self, name, value):
        # Values with surrogate escapes become Header objects so that the
        # undecodable bytes survive.
        if not isinstance(value, str):
            return value
        if _has_surrogates(value):
            return header.Header(value, charset=_charset.UNKNOWN8BIT,
                                 header_name=name)
        return value

    def header_source_parse(self, sourcelines):
        """Name up to the colon; the value with leading blanks removed and
        the line ending of the last line stripped."""
        name, value = sourcelines[0].split(':', 1)
        value = value.lstrip(' \t') + ''.join(sourcelines[1:])
        return (name, value.rstrip('\r\n'))

    def header_store_parse(self, name, value):
        return (name, value)

    def header_fetch_parse(self, name, value):
        return self._sanitize_header(name, value)

    def fold(self, name, value):
        return self._fold(name, value, sanitize=True)

    def fold_binary(self, name, value):
        folded = self._fold(name, value, sanitize=self.cte_type == '7bit')
        return folded.encode('ascii', 'surrogateescape')

    def _fold(self, name, value, sanitize):
        pieces = ['%s: ' % name]
        h = value
        if isinstance(value, str):
            if not _has_surrogates(value):
                h = header.Header(value, header_name=name)
            elif sanitize:
                h = header.Header(value, charset=_charset.UNKNOWN8BIT,
                                  header_name=name)
            else:
                # raw 8bit text of unknown encoding is passed through
                pieces.append(value)
                h = None
        if h is not None:
            limit = self.max_line_length if self.max_line_length is not None else 0
            pieces.append(h.encode(linesep=self.linesep, maxlinelen=limit))
        pieces.append(self.linesep)
        return ''.join(pieces)


compat32 = Compat32()
