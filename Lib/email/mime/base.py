"""Base class for the MIME message classes."""

__all__ = ['MIMEBase']

import email.policy

from email import message


class MIMEBase(message.Message):
    """A message with Content-Type and MIME-Version headers."""

    def __init__(self, _maintype, _subtype, *, policy=None, **_params):
        """Content-Type becomes _maintype/_subtype with the keyword
        arguments as parameters."""
        if policy is None:
            policy = email.policy.compat32
        message.Message.__init__(self, policy=policy)
        ctype = '%s/%s' % (_maintype, _subtype)
        self.add_header('Content-Type', ctype, **_params)
        self['MIME-Version'] = '1.0'
