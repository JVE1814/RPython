"""Parse TOML (Tom's Obvious Minimal Language) documents.

    load(fp, /, *, parse_float=float)   -- parse a binary file object
    loads(s, /, *, parse_float=float)   -- parse a str

Both return a dict.  TOMLDecodeError is raised for invalid documents.
"""

__all__ = ("loads", "load", "TOMLDecodeError")

from ._parser import TOMLDecodeError, load, loads

# Make the error class look like it belongs to the public package.
TOMLDecodeError.__module__ = __name__
