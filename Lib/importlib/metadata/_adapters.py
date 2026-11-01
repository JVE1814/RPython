"""The PackageMetadata implementation: core metadata (METADATA/PKG-INFO)
parsed into an email.message.Message subclass."""

import email.message
import re
import textwrap

from ._text import FoldedCase

# Fields that may legitimately occur several times (core metadata spec);
# ``json`` reports them as lists.
_MULTI_FIELDS = frozenset(
    FoldedCase(name)
    for name in (
        'Classifier',
        'Obsoletes-Dist',
        'Platform',
        'Project-URL',
        'Provides-Dist',
        'Provides-Extra',
        'Requires-Dist',
        'Requires-External',
        'Supported-Platform',
        'Dynamic',
        'License-File',
        'Import-Name',
        'Import-Namespace',
    )
)

_FIELD_RE = re.compile(r'([^\s:][^:]*?)\s*:[ \t]?(.*)\Z', re.DOTALL)


def _parse(text):
    """Split *text* into ([(field, value), ...], body).

    Header lines are 'Name: value'; lines starting with whitespace continue
    the previous value (their common indentation is removed, the line
    breaks kept).  The first empty line ends the header block.
    """
    lines = text.splitlines()
    fields = []
    index = 0
    while index < len(lines):
        line = lines[index]
        if not line.strip():
            index += 1
            break
        if line[:1] in (' ', '\t') and fields:
            fields[-1][1].append(line)
            index += 1
            continue
        match = _FIELD_RE.match(line)
        if match is None:
            # Not a header: treat the rest as the body.
            break
        fields.append((match.group(1), [match.group(2)]))
        index += 1
    body = '\n'.join(lines[index:])
    if body and text.endswith(('\n', '\r')):
        body += '\n'
    headers = []
    for name, parts in fields:
        first, rest = parts[0], parts[1:]
        if rest:
            value = first + '\n' + textwrap.dedent('\n'.join(rest))
        else:
            value = first
        headers.append((name, value.rstrip() if rest else value))
    return headers, body


class Message(email.message.Message):
    """Distribution metadata.

    Behaves like email.message.Message (case-insensitive field access,
    ``get_all``), except that indexing a missing field raises KeyError and
    a free-form body is exposed as the ``Description`` field.
    """

    @classmethod
    def from_text(cls, text):
        message = cls()
        headers, body = _parse(text)
        for name, value in headers:
            message[name] = value
        if body.strip():
            message['Description'] = body
        return message

    def __getitem__(self, item):
        value = super().__getitem__(item)
        if value is None:
            raise KeyError(item)
        return value

    def __iter__(self):
        return iter(self.keys())

    @property
    def json(self):
        """The metadata as a JSON-compatible dict, keys normalised to
        lower case with underscores; repeatable fields map to lists and
        'keywords' to a list of words."""
        result = {}
        for raw_key in self.keys():
            key = FoldedCase(raw_key)
            json_key = key.lower().replace('-', '_')
            if json_key in result:
                continue
            if key in _MULTI_FIELDS:
                value = self.get_all(key)
            else:
                value = self[key]
            if key == 'Keywords':
                value = [word for word in re.split(r'[\s,]+', value) if word]
            result[json_key] = value
        return result
