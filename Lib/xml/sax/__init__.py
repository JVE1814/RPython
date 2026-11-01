"""SAX 2: the Simple API for XML.

make_parser() returns a parser driver (by default the expat based one);
parse() and parseString() are shortcuts for parsing with a handler.
"""

from .xmlreader import InputSource
from .handler import ContentHandler, ErrorHandler
from ._exceptions import (SAXException, SAXNotRecognizedException,
                          SAXParseException, SAXNotSupportedException,
                          SAXReaderNotAvailable)

__all__ = [
    "ContentHandler",
    "ErrorHandler",
    "InputSource",
    "SAXException",
    "SAXNotRecognizedException",
    "SAXNotSupportedException",
    "SAXParseException",
    "SAXReaderNotAvailable",
    "default_parser_list",
    "make_parser",
    "parse",
    "parseString",
]


def parse(source, handler, errorHandler=ErrorHandler()):
    """Parse *source* (file, name or InputSource) with *handler*."""
    parser = make_parser()
    parser.setContentHandler(handler)
    parser.setErrorHandler(errorHandler)
    parser.parse(source)


def parseString(string, handler, errorHandler=ErrorHandler()):
    """Parse a document given as str or bytes."""
    import io
    if errorHandler is None:
        errorHandler = ErrorHandler()
    parser = make_parser()
    parser.setContentHandler(handler)
    parser.setErrorHandler(errorHandler)
    source = InputSource()
    if isinstance(string, str):
        source.setCharacterStream(io.StringIO(string))
    else:
        source.setByteStream(io.BytesIO(string))
    parser.parse(source)


default_parser_list = ["xml.sax.expatreader"]

import os as _os
import sys as _sys
if not _sys.flags.ignore_environment and "PY_SAX_PARSER" in _os.environ:
    default_parser_list = _os.environ["PY_SAX_PARSER"].split(",")
del _os, _sys


def make_parser(parser_list=()):
    """Create a SAX parser, trying the modules in *parser_list* and then
    default_parser_list."""
    for parser_name in list(parser_list) + default_parser_list:
        try:
            return _create_parser(parser_name)
        except ImportError:
            import sys
            if parser_name in sys.modules:
                # the module exists but failed to import properly
                raise
        except SAXReaderNotAvailable:
            pass
    raise SAXReaderNotAvailable("No parsers found", None)


def _create_parser(parser_name):
    module = __import__(parser_name, {}, {}, ['create_parser'])
    return module.create_parser()
