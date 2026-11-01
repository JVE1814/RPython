"""MIME multipart messages."""

__all__ = ['MIMEMultipart']

from email.mime.base import MIMEBase


class MIMEMultipart(MIMEBase):
    """A multipart/* message (multipart/mixed by default)."""

    def __init__(self, _subtype='mixed', boundary=None, _subparts=None,
                 *, policy=None, **_params):
        """*boundary* (default: chosen when generating), *_subparts* (an
        iterable of initial parts) and further Content-Type parameters."""
        MIMEBase.__init__(self, 'multipart', _subtype, policy=policy,
                          **_params)
        # an empty list, so that is_multipart() is true even without parts
        self._payload = []
        if _subparts:
            for part in _subparts:
                self.attach(part)
        if boundary:
            self.set_boundary(boundary)
