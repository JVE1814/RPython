"""The function-style resource API (read_text, open_binary, path, ...),
expressed in terms of files()."""

import warnings

from ._common import as_file, files
from .abc import TraversalError

_NO_ENCODING = object()


def _encoding_for(path_names, encoding):
    if encoding is not _NO_ENCODING:
        return encoding
    if len(path_names) > 1:
        raise TypeError(
            "'encoding' argument required with multiple path names",
        )
    return 'utf-8'


def _resource(anchor, path_names):
    if anchor is None:
        raise TypeError("anchor must be module or string, got None")
    return files(anchor).joinpath(*path_names)


def open_binary(anchor, *path_names):
    """Open the named resource for binary reading."""
    return _resource(anchor, path_names).open('rb')


def open_text(anchor, *path_names, encoding=_NO_ENCODING, errors='strict'):
    """Open the named resource for text reading."""
    encoding = _encoding_for(path_names, encoding)
    return _resource(anchor, path_names).open('r', encoding=encoding, errors=errors)


def read_binary(anchor, *path_names):
    """Return the bytes content of the named resource."""
    return _resource(anchor, path_names).read_bytes()


def read_text(anchor, *path_names, encoding=_NO_ENCODING, errors='strict'):
    """Return the text content of the named resource."""
    encoding = _encoding_for(path_names, encoding)
    return _resource(anchor, path_names).read_text(encoding=encoding, errors=errors)


def path(anchor, *path_names):
    """Context manager providing a file system path to the named resource."""
    return as_file(_resource(anchor, path_names))


def is_resource(anchor, *path_names):
    """Return True if the named resource exists and is a file."""
    try:
        target = _resource(anchor, path_names)
    except TraversalError:
        return False
    return target.is_file()


def contents(anchor, *path_names):
    """Return an iterable of the entry names of the named directory.

    Deprecated: use ``files(anchor).iterdir()``.
    """
    warnings.warn(
        "importlib.resources.contents is deprecated. "
        "Use files(anchor).iterdir() instead.",
        DeprecationWarning,
        stacklevel=1,
    )
    entries = _resource(anchor, path_names).iterdir()
    return (entry.name for entry in entries)
