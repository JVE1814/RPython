"""Exceptions raised by urllib.request.

URLError is the base class (a subclass of OSError).  HTTPError is raised
for HTTP error statuses; it doubles as a response object, so the body of
the error page can be read from it.
"""

import io
import urllib.response

__all__ = ['URLError', 'HTTPError', 'ContentTooShortError']


class URLError(OSError):
    """Something went wrong while opening a URL; *reason* says what."""

    def __init__(self, reason, filename=None):
        self.args = (reason,)
        self.reason = reason
        if filename is not None:
            self.filename = filename

    def __str__(self):
        return f'<urlopen error {self.reason}>'


class HTTPError(URLError, urllib.response.addinfourl):
    """The server answered with an HTTP error status."""

    __super_init = urllib.response.addinfourl.__init__

    def __init__(self, url, code, msg, hdrs, fp):
        self.code = code
        self.msg = msg
        self.hdrs = hdrs
        self.fp = fp
        self.filename = url
        if fp is None:
            fp = io.BytesIO()
        self.__super_init(fp, hdrs, url, code)

    def __str__(self):
        return f'HTTP Error {self.code}: {self.msg}'

    def __repr__(self):
        return f'<HTTPError {self.code}: {self.msg!r}>'

    @property
    def reason(self):
        return self.msg

    @property
    def headers(self):
        return self.hdrs

    @headers.setter
    def headers(self, value):
        self.hdrs = value


class ContentTooShortError(URLError):
    """Fewer bytes were received than the Content-Length announced."""

    def __init__(self, message, content):
        URLError.__init__(self, message)
        self.content = content
