"""Iterators over the parts of a message tree."""

__all__ = [
    'body_line_iterator',
    'typed_subpart_iterator',
    'walk',
]

import sys
from io import StringIO


def walk(self):
    """Yield the message and all of its subparts, depth first."""
    yield self
    if self.is_multipart():
        for subpart in self.get_payload():
            yield from subpart.walk()


def body_line_iterator(msg, decode=False):
    """Yield the lines of all string payloads in the message tree."""
    for part in msg.walk():
        payload = part.get_payload(decode=decode)
        if isinstance(payload, str):
            yield from StringIO(payload)


def typed_subpart_iterator(msg, maintype='text', subtype=None):
    """Yield the parts of the tree with the given MIME main type (and
    subtype, if given)."""
    for part in msg.walk():
        if part.get_content_maintype() != maintype:
            continue
        if subtype is None or part.get_content_subtype() == subtype:
            yield part


def _structure(msg, fp=None, level=0, include_default=False):
    """Print the content types of the message tree (for debugging)."""
    if fp is None:
        fp = sys.stdout
    indent = ' ' * (level * 4)
    line = indent + msg.get_content_type()
    if include_default:
        line += ' [%s]' % msg.get_default_type()
    print(line, file=fp)
    if msg.is_multipart():
        for part in msg.get_payload():
            _structure(part, fp, level + 1, include_default)
