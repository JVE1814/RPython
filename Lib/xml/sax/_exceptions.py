"""Exceptions of the SAX interfaces."""


class SAXException(Exception):
    """Base class for SAX errors and warnings; may wrap another
    exception."""

    def __init__(self, msg, exception=None):
        self._msg = msg
        self._exception = exception
        Exception.__init__(self, msg)

    def getMessage(self):
        return self._msg

    def getException(self):
        return self._exception

    def __str__(self):
        return self._msg

    def __getitem__(self, ix):
        # guard against code expecting a tuple
        raise AttributeError("__getitem__")


class SAXParseException(SAXException):
    """A parse error, with the location reported by the parser's
    locator."""

    def __init__(self, msg, exception, locator):
        SAXException.__init__(self, msg, exception)
        self._locator = locator
        self._systemId = self._locator.getSystemId()
        self._colnum = self._locator.getColumnNumber()
        self._linenum = self._locator.getLineNumber()

    def getColumnNumber(self):
        return self._colnum

    def getLineNumber(self):
        return self._linenum

    def getPublicId(self):
        return self._locator.getPublicId()

    def getSystemId(self):
        return self._systemId

    def __str__(self):
        sysid = self.getSystemId()
        if sysid is None:
            sysid = "<unknown>"
        linenum = self.getLineNumber()
        if linenum is None:
            linenum = "?"
        colnum = self.getColumnNumber()
        if colnum is None:
            colnum = "?"
        return "%s:%s:%s: %s" % (sysid, linenum, colnum, self._msg)


class SAXNotRecognizedException(SAXException):
    """An unknown feature or property was requested."""


class SAXNotSupportedException(SAXException):
    """A known feature or property cannot be used."""


class SAXReaderNotAvailable(SAXNotSupportedException):
    """No parser driver could be loaded."""
