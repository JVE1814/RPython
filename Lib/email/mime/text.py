"""MIME text/* messages."""

__all__ = ['MIMEText']

from email.mime.nonmultipart import MIMENonMultipart


class MIMEText(MIMENonMultipart):
    """A text/* message."""

    def __init__(self, _text, _subtype='plain', _charset=None, *,
                 policy=None):
        """With no *_charset*, us-ascii is used if the text is ASCII,
        otherwise utf-8."""
        if _charset is None:
            try:
                _text.encode('us-ascii')
                _charset = 'us-ascii'
            except UnicodeEncodeError:
                _charset = 'utf-8'
        MIMENonMultipart.__init__(self, 'text', _subtype, policy=policy,
                                  **{'charset': str(_charset)})
        self.set_payload(_text, _charset)
