"""Parsers that build message objects from strings, bytes or files."""

__all__ = ['Parser', 'HeaderParser', 'BytesParser', 'BytesHeaderParser',
           'FeedParser', 'BytesFeedParser']

from io import StringIO, TextIOWrapper

from email.feedparser import FeedParser, BytesFeedParser
from email._policybase import compat32


class Parser:
    """Parse text messages (str or text files)."""

    def __init__(self, _class=None, *, policy=compat32):
        """*_class* is the message factory (see FeedParser); *policy*
        controls the parse."""
        self._class = _class
        self.policy = policy

    def parse(self, fp, headersonly=False):
        """Parse the text file *fp*; with *headersonly* the body is not
        parsed (it becomes the payload as is)."""
        feeder = FeedParser(self._class, policy=self.policy)
        if headersonly:
            feeder._set_headersonly()
        while True:
            chunk = fp.read(8192)
            if not chunk:
                break
            feeder.feed(chunk)
        return feeder.close()

    def parsestr(self, text, headersonly=False):
        """Parse a message from a str."""
        return self.parse(StringIO(text), headersonly=headersonly)


class HeaderParser(Parser):
    """A Parser that only parses the headers."""

    def parse(self, fp, headersonly=True):
        return Parser.parse(self, fp, True)

    def parsestr(self, text, headersonly=True):
        return Parser.parsestr(self, text, True)


class BytesParser:
    """Parse binary messages (bytes or binary files).

    Non-ASCII bytes are carried through as surrogate escapes."""

    def __init__(self, *args, **kw):
        self.parser = Parser(*args, **kw)

    def parse(self, fp, headersonly=False):
        text_fp = TextIOWrapper(fp, encoding='ascii', errors='surrogateescape')
        try:
            return self.parser.parse(text_fp, headersonly)
        finally:
            text_fp.detach()

    def parsebytes(self, text, headersonly=False):
        text = text.decode('ASCII', errors='surrogateescape')
        return self.parser.parsestr(text, headersonly)


class BytesHeaderParser(BytesParser):
    """A BytesParser that only parses the headers."""

    def parse(self, fp, headersonly=True):
        return BytesParser.parse(self, fp, headersonly=True)

    def parsebytes(self, text, headersonly=True):
        return BytesParser.parsebytes(self, text, headersonly=True)
