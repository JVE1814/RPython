"""Parse dylib path names of the form Location/Name.Version_Suffix.dylib."""

import re

__all__ = ["dylib_info"]

_DYLIB = re.compile(r"""
    (?P<location>^.*)(?:^|/)
    (?P<name>
        (?P<shortname>\w+?)
        (?:\.(?P<version>[^._]+))?
        (?:_(?P<suffix>[^._]+))?
        \.dylib$
    )
""", re.VERBOSE)


def dylib_info(filename):
    """Split a dylib path into its parts.

    Returns a dict with the keys location, name, shortname, version and
    suffix (version/suffix may be None), or None if ``filename`` is not a
    dylib path.
    """
    m = _DYLIB.match(filename)
    return m.groupdict() if m else None
