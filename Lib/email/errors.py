"""Exception classes and the defect classes used by the email package.

Errors are raised; defects are recorded on the objects they concern (for
example ``msg.defects``) when the parser can recover from a problem.
"""


class MessageError(Exception):
    """Base class for errors raised by the email package."""


class MessageParseError(MessageError):
    """A message could not be parsed."""


class HeaderParseError(MessageParseError):
    """A header could not be parsed."""


class BoundaryError(MessageParseError):
    """A multipart boundary could not be found (no longer raised)."""


class MultipartConversionError(MessageError, TypeError):
    """A message could not be turned into (or attached to) a multipart."""


class CharsetError(MessageError):
    """An unusable character set was given."""


class HeaderWriteError(MessageError):
    """A header could not be serialized safely."""


# -- defects ----------------------------------------------------------------

class MessageDefect(ValueError):
    """Base class for problems found while parsing a message."""

    def __init__(self, line=None):
        if line is not None:
            super().__init__(line)
        self.line = line


class NoBoundaryInMultipartDefect(MessageDefect):
    """A multipart message has no boundary parameter."""


class StartBoundaryNotFoundDefect(MessageDefect):
    """The start boundary of a multipart body was never found."""


class CloseBoundaryNotFoundDefect(MessageDefect):
    """A start boundary was found but the closing one was not."""


class FirstHeaderLineIsContinuationDefect(MessageDefect):
    """The first header line was indented (a continuation line)."""


class MisplacedEnvelopeHeaderDefect(MessageDefect):
    """A 'From ' line appeared inside the headers."""


class MissingHeaderBodySeparatorDefect(MessageDefect):
    """A line that is not a header was found before the blank line."""


MalformedHeaderDefect = MissingHeaderBodySeparatorDefect


class MultipartInvariantViolationDefect(MessageDefect):
    """A message claimed to be multipart but was not."""


class InvalidMultipartContentTransferEncodingDefect(MessageDefect):
    """A multipart part had an encoding other than 7bit, 8bit or binary."""


class UndecodableBytesDefect(MessageDefect):
    """Bytes in a header could not be decoded with the declared charset."""


class InvalidBase64PaddingDefect(MessageDefect):
    """base64 data had missing padding characters."""


class InvalidBase64CharactersDefect(MessageDefect):
    """base64 data contained characters outside the base64 alphabet."""


class InvalidBase64LengthDefect(MessageDefect):
    """base64 data had a length that cannot be decoded."""


class HeaderDefect(MessageDefect):
    """Base class for problems in a header value."""

    def __init__(self, *args, **kw):
        super().__init__(*args, **kw)


class InvalidHeaderDefect(HeaderDefect):
    """A header value is invalid."""


class HeaderMissingRequiredValue(HeaderDefect):
    """A header that needs a value has none."""


class NonPrintableDefect(HeaderDefect):
    """A header contains ASCII control characters."""

    def __init__(self, non_printables):
        super().__init__(non_printables)
        self.non_printables = non_printables

    def __str__(self):
        return ("the following ASCII non-printables found in header: "
                "{}".format(self.non_printables))


class ObsoleteHeaderDefect(HeaderDefect):
    """A header uses syntax that RFC 5322 only allows as obsolete."""


class NonASCIILocalPartDefect(HeaderDefect):
    """A local part contains non-ASCII text (only allowed with SMTPUTF8)."""


class InvalidDateDefect(HeaderDefect):
    """A date header could not be parsed."""
