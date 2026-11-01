"""Base class for MIME messages that are not multipart."""

__all__ = ['MIMENonMultipart']

from email import errors
from email.mime.base import MIMEBase


class MIMENonMultipart(MIMEBase):
    """A MIME message whose payload cannot have subparts."""

    def attach(self, payload):
        raise errors.MultipartConversionError(
            'Cannot attach additional subparts to non-multipart/*')
