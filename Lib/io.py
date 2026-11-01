"""The io module provides the Python interfaces to stream handling.

The builtin open function is defined in this module.  The concrete
classes (FileIO, BytesIO, StringIO, BufferedReader, BufferedWriter,
BufferedRandom, BufferedRWPair, TextIOWrapper) are implemented in the
_io accelerator module; this module adds the abstract base classes
IOBase, RawIOBase, BufferedIOBase and TextIOBase and registers the
concrete classes with them.

DEFAULT_BUFFER_SIZE is the default buffer size used by the buffered
classes; open() uses the file's blksize when it can be determined.
"""

__author__ = "rpython"

__all__ = ["BlockingIOError", "open", "open_code", "IOBase", "RawIOBase",
           "FileIO", "BytesIO", "StringIO", "BufferedIOBase",
           "BufferedReader", "BufferedWriter", "BufferedRWPair",
           "BufferedRandom", "TextIOBase", "TextIOWrapper",
           "UnsupportedOperation", "SEEK_SET", "SEEK_CUR", "SEEK_END",
           "DEFAULT_BUFFER_SIZE", "text_encoding", "IncrementalNewlineDecoder",
           "Reader", "Writer"]


import _io
import abc

from _io import (DEFAULT_BUFFER_SIZE, BlockingIOError, UnsupportedOperation,
                 open, open_code, FileIO, BytesIO, StringIO, BufferedReader,
                 BufferedWriter, BufferedRWPair, BufferedRandom,
                 IncrementalNewlineDecoder, text_encoding, TextIOWrapper)

# UnsupportedOperation is created by _io with __module__ already set to "io".

SEEK_SET = 0
SEEK_CUR = 1
SEEK_END = 2


class IOBase(_io._IOBase, metaclass=abc.ABCMeta):
    __doc__ = _io._IOBase.__doc__

class RawIOBase(_io._RawIOBase, IOBase):
    __doc__ = _io._RawIOBase.__doc__

class BufferedIOBase(_io._BufferedIOBase, IOBase):
    __doc__ = _io._BufferedIOBase.__doc__

class TextIOBase(_io._TextIOBase, IOBase):
    __doc__ = _io._TextIOBase.__doc__


RawIOBase.register(FileIO)

for _cls in (BytesIO, BufferedReader, BufferedWriter, BufferedRandom,
             BufferedRWPair):
    BufferedIOBase.register(_cls)

for _cls in (StringIO, TextIOWrapper):
    TextIOBase.register(_cls)
del _cls

try:
    from _io import _WindowsConsoleIO
except ImportError:
    pass
else:
    RawIOBase.register(_WindowsConsoleIO)


from _collections_abc import _check_methods

GenericAlias = type(list[int])


class Reader(metaclass=abc.ABCMeta):
    """Protocol for simple I/O reader instances.

    This protocol only supports blocking I/O.
    """

    __slots__ = ()

    @abc.abstractmethod
    def read(self, size=..., /):
        """Read data from the input stream and return it.

        If *size* is specified, at most *size* items (bytes/characters) will be
        read.
        """

    @classmethod
    def __subclasshook__(cls, C):
        if cls is Reader:
            return _check_methods(C, "read")
        return NotImplemented

    __class_getitem__ = classmethod(GenericAlias)


class Writer(metaclass=abc.ABCMeta):
    """Protocol for simple I/O writer instances.

    This protocol only supports blocking I/O.
    """

    __slots__ = ()

    @abc.abstractmethod
    def write(self, data, /):
        """Write *data* to the output stream and return the number of items written."""

    @classmethod
    def __subclasshook__(cls, C):
        if cls is Writer:
            return _check_methods(C, "write")
        return NotImplemented

    __class_getitem__ = classmethod(GenericAlias)
