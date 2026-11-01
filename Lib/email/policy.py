"""The standard policies of the email package (besides compat32)."""

import re
import sys
from email._policybase import Policy, Compat32, compat32
from email.utils import _has_surrogates
from email.headerregistry import HeaderRegistry as HeaderRegistry
from email.contentmanager import raw_data_manager
from email.message import EmailMessage

__all__ = [
    'Compat32',
    'compat32',
    'Policy',
    'EmailPolicy',
    'default',
    'strict',
    'SMTP',
    'HTTP',
]

linesep_splitter = re.compile(r'\n|\r')


class EmailPolicy(Policy):
    """The policy implementing current email standards.

    utf8 -- serialize headers as utf-8 instead of RFC 2047 encoded words
    refold_source -- 'none', 'long' (refold source headers with lines longer
                     than max_line_length) or 'all'
    header_factory -- callable (name, value) -> header object
    content_manager -- used by get_content() and set_content()
    """

    message_factory = EmailMessage
    utf8 = False
    refold_source = 'long'
    header_factory = HeaderRegistry()
    content_manager = raw_data_manager

    def __init__(self, **kw):
        # New instances (unlike clones) get their own header registry.
        if 'header_factory' not in kw:
            object.__setattr__(self, 'header_factory', HeaderRegistry())
        super().__init__(**kw)

    def header_max_count(self, name):
        """The max_count of the header class registered for *name*."""
        return self.header_factory[name].max_count

    def header_source_parse(self, sourcelines):
        """The name up to the colon; the value with leading whitespace and
        the line ending of the last line removed (folding is kept)."""
        name, value = sourcelines[0].split(':', 1)
        value = value.lstrip(' \t') + ''.join(sourcelines[1:])
        return (name, value.rstrip('\r\n'))

    def header_store_parse(self, name, value):
        """Header objects with a matching name are stored as is; strings
        (which must not contain line breaks) go through the factory."""
        if hasattr(value, 'name') and value.name.lower() == name.lower():
            return (name, value)
        if isinstance(value, str) and len(value.splitlines()) > 1:
            raise ValueError("Header values may not contain linefeed "
                             "or carriage return characters")
        return (name, self.header_factory(name, value))

    def header_fetch_parse(self, name, value):
        """Return a header object (made from the unfolded source value if
        needed)."""
        if hasattr(value, 'name'):
            return value
        value = ''.join(linesep_splitter.split(value))
        return self.header_factory(name, value)

    def fold(self, name, value):
        return self._fold(name, value, refold_binary=True)

    def fold_binary(self, name, value):
        folded = self._fold(name, value,
                            refold_binary=self.cte_type == '7bit')
        charset = 'utf8' if self.utf8 else 'ascii'
        return folded.encode(charset, 'surrogateescape')

    def _fold(self, name, value, refold_binary=False):
        if hasattr(value, 'name'):
            return value.fold(policy=self)
        maxlen = self.max_line_length if self.max_line_length else sys.maxsize
        lines = value.splitlines()
        refold = (self.refold_source == 'all'
                  or (self.refold_source == 'long'
                      and ((lines and len(lines[0]) + len(name) + 2 > maxlen)
                           or any(len(x) > maxlen for x in lines[1:]))))
        if refold or (refold_binary and _has_surrogates(value)):
            return self.header_factory(name, ''.join(lines)).fold(policy=self)
        return name + ': ' + self.linesep.join(lines) + self.linesep


default = EmailPolicy()
# the default policy uses the class-level header factory
del default.header_factory
strict = default.clone(raise_on_defect=True)
SMTP = default.clone(linesep='\r\n')
HTTP = default.clone(linesep='\r\n', max_line_length=None)
SMTPUTF8 = SMTP.clone(utf8=True)
