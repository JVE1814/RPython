"""MIME message/* messages (a message containing a message)."""

__all__ = ['MIMEMessage']

from email import message
from email.mime.nonmultipart import MIMENonMultipart


class MIMEMessage(MIMENonMultipart):
    """A message/rfc822 (or other message/*) part wrapping *_msg*."""

    def __init__(self, _msg, _subtype='rfc822', *, policy=None):
        MIMENonMultipart.__init__(self, 'message', _subtype, policy=policy)
        if not isinstance(_msg, message.Message):
            raise TypeError('Argument is not an instance of Message')
        # bypass the non-multipart attach() restriction
        message.Message.attach(self, _msg)
        self.set_default_type('message/rfc822')
