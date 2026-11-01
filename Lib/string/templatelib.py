"""Support for template string literals (t-strings, PEP 750)."""

t = t"{0}"
Template = type(t)
Interpolation = type(t.interpolations[0])
del t

__all__ = ["Interpolation", "Template", "convert"]


def convert(obj, /, conversion):
    """Apply the conversion ('a', 'r', 's' or None) of an interpolation."""
    if conversion is None:
        return obj
    if conversion == 'r':
        return repr(obj)
    if conversion == 's':
        return str(obj)
    if conversion == 'a':
        return ascii(obj)
    raise ValueError(f'invalid conversion specifier: {conversion}')


def _template_unpickle(*args):
    if len(args) != 2:
        raise ValueError('Template expects tuple of length 2 to unpickle')
    strings, interpolations = args
    parts = []
    for i, s in enumerate(strings):
        parts.append(s)
        if i < len(interpolations):
            parts.append(interpolations[i])
    return Template(*parts)
