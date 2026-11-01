"""A mutable list of response headers with a mapping-like interface.

Lookups are case-insensitive; the original order and spelling of the
headers is kept.  str() gives the header block as sent on the wire.
"""

import re

__all__ = ['Headers']

# A parameter value needs quoting if it contains separators (RFC 2045).
_needs_quoting = re.compile(r'[ \(\)<>@,;:\\"/\[\]\?=]')

_C0_EXCEPT_TAB = frozenset(chr(c) for c in range(0x20) if c != 9) | {'\x7f'}
_C0 = _C0_EXCEPT_TAB | {'\t'}


def _formatparam(param, value=None, quote=1):
    """'param="value"' (quoted when needed), or just 'param'."""
    if value is not None and len(value) > 0:
        if quote or _needs_quoting.search(value):
            value = value.replace('\\', '\\\\').replace('"', r'\"')
            return f'{param}="{value}"'
        return f'{param}={value}'
    return param


class Headers:
    """Manage a list of (name, value) response header pairs."""

    def __init__(self, headers=None):
        headers = headers if headers is not None else []
        if type(headers) is not list:
            raise TypeError('Headers must be a list of name/value tuples')
        self._headers = headers
        for name, value in headers:
            self._convert_string_type(name, name=True)
            self._convert_string_type(value, name=False)

    def _convert_string_type(self, value, *, name):
        """Check that *value* is a str without forbidden control
        characters and return it."""
        if type(value) is not str:
            raise AssertionError(
                f'Header names/values must be of type str (got {value!r})')
        forbidden = _C0 if name else _C0_EXCEPT_TAB
        if not forbidden.isdisjoint(value):
            what = 'name' if name else 'value'
            raise ValueError(f'Control characters not allowed in header '
                             f'{what}: {value!r}')
        return value

    def __len__(self):
        return len(self._headers)

    def __setitem__(self, name, val):
        """Replace all headers called *name* by one with value *val*."""
        del self[name]
        self._headers.append((self._convert_string_type(name, name=True),
                              self._convert_string_type(val, name=False)))

    def __delitem__(self, name):
        """Remove all headers called *name* (no error if there is none)."""
        name = self._convert_string_type(name.lower(), name=True)
        self._headers[:] = [kv for kv in self._headers
                            if kv[0].lower() != name]

    def __getitem__(self, name):
        """The first value of header *name*, or None."""
        return self.get(name)

    def __contains__(self, name):
        return self.get(name) is not None

    def get_all(self, name):
        """All values of header *name* in order (possibly an empty list)."""
        name = self._convert_string_type(name.lower(), name=True)
        return [value for key, value in self._headers
                if key.lower() == name]

    def get(self, name, default=None):
        name = self._convert_string_type(name.lower(), name=True)
        for key, value in self._headers:
            if key.lower() == name:
                return value
        return default

    def keys(self):
        return [key for key, value in self._headers]

    def values(self):
        return [value for key, value in self._headers]

    def items(self):
        """A copy of the (name, value) pairs."""
        return self._headers[:]

    def __repr__(self):
        return f'{self.__class__.__name__}({self._headers!r})'

    def __str__(self):
        """The header block, terminated by an empty line."""
        lines = [f'{key}: {value}' for key, value in self._headers]
        return '\r\n'.join(lines + ['', ''])

    def __bytes__(self):
        return str(self).encode('iso-8859-1')

    def setdefault(self, name, value):
        """Return the value of *name*, adding it with *value* first if it
        is missing."""
        result = self.get(name)
        if result is None:
            self._headers.append((self._convert_string_type(name, name=True),
                                  self._convert_string_type(value,
                                                            name=False)))
            return value
        return result

    def add_header(self, _name, _value, **_params):
        """Add a header with optional MIME parameters.

        Keyword arguments become parameters (underscores in their names
        turn into dashes); a None value adds a bare parameter name.
        """
        parts = []
        if _value is not None:
            parts.append(self._convert_string_type(_value, name=False))
        for key, value in _params.items():
            key = self._convert_string_type(key.replace('_', '-'), name=True)
            if value is None:
                parts.append(key)
            else:
                value = self._convert_string_type(value, name=False)
                parts.append(_formatparam(key, value))
        self._headers.append((self._convert_string_type(_name, name=True),
                              '; '.join(parts)))
