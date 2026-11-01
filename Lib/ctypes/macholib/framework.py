"""Parse framework paths of the form
Location/Name.framework/Versions/Version/Name_Suffix."""

import re

__all__ = ["framework_info"]

_FRAMEWORK = re.compile(r"""
    (?P<location>^.*)(?:^|/)
    (?P<name>
        (?P<shortname>\w+).framework/
        (?:Versions/(?P<version>[^/]+)/)?
        (?P=shortname)
        (?:_(?P<suffix>[^_]+))?
    )$
""", re.VERBOSE)


def framework_info(filename):
    """Split a framework path into its parts (or return None)."""
    m = _FRAMEWORK.match(filename)
    return m.groupdict() if m else None
