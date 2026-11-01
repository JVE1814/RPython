"""File-like wrappers returned by urllib.request.urlopen().

addbase wraps a file object (closing it when used as a context manager),
addclosehook runs a callback on close, addinfo adds a headers object and
addinfourl additionally the URL and status code.
"""

import tempfile

__all__ = ['addbase', 'addclosehook', 'addinfo', 'addinfourl']


class addbase(tempfile._TemporaryFileWrapper):
    """Delegate attribute access to an underlying file object."""

    def __init__(self, fp):
        super().__init__(fp, '<urllib response>', delete=False)
        self.fp = fp

    def __repr__(self):
        return (f'<{self.__class__.__name__} at {id(self)!r} '
                f'whose fp = {self.file!r}>')

    def __enter__(self):
        if self.fp.closed:
            raise ValueError('I/O operation on closed file')
        return self

    def __exit__(self, exc_type, exc, tb):
        self.close()


class addclosehook(addbase):
    """A wrapper that calls *closehook(*hookargs)* when closed."""

    def __init__(self, fp, closehook, *hookargs):
        super().__init__(fp)
        self.closehook = closehook
        self.hookargs = hookargs

    def close(self):
        try:
            hook, args = self.closehook, self.hookargs
            if hook is not None:
                self.closehook = None
                self.hookargs = None
                hook(*args)
        finally:
            super().close()


class addinfo(addbase):
    """A wrapper that also carries the response headers."""

    def __init__(self, fp, headers):
        super().__init__(fp)
        self.headers = headers

    def info(self):
        return self.headers


class addinfourl(addinfo):
    """A wrapper that also carries the URL and the status code."""

    def __init__(self, fp, headers, url, code=None):
        super().__init__(fp, headers)
        self.url = url
        self.code = code

    @property
    def status(self):
        return self.code

    def getcode(self):
        return self.code

    def geturl(self):
        return self.url
