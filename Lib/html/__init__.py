"""General functions for HTML manipulation: escaping and unescaping text."""

import re as _re
from html.entities import html5 as _html5


__all__ = ['escape', 'unescape']


def escape(s, quote=True):
    """Replace the characters &, < and > in *s* by HTML-safe sequences.

    When *quote* is true (the default), the quote characters " and ' are
    translated as well, so the result can be used inside attribute values.
    """
    s = s.replace("&", "&amp;")
    s = s.replace("<", "&lt;").replace(">", "&gt;")
    if quote:
        s = s.replace('"', "&quot;").replace("'", "&#x27;")
    return s


# Numeric references to these code points are replaced by the Windows-1252
# interpretation of the byte (as browsers do), see the HTML5 tokenizer.
_C1_REPLACEMENTS = {
    0x00: '�', 0x0d: '\r',
    0x80: '€', 0x81: '\x81', 0x82: '‚', 0x83: 'ƒ',
    0x84: '„', 0x85: '…', 0x86: '†', 0x87: '‡',
    0x88: 'ˆ', 0x89: '‰', 0x8a: 'Š', 0x8b: '‹',
    0x8c: 'Œ', 0x8d: '\x8d', 0x8e: 'Ž', 0x8f: '\x8f',
    0x90: '\x90', 0x91: '‘', 0x92: '’', 0x93: '“',
    0x94: '”', 0x95: '•', 0x96: '–', 0x97: '—',
    0x98: '˜', 0x99: '™', 0x9a: 'š', 0x9b: '›',
    0x9c: 'œ', 0x9d: '\x9d', 0x9e: 'ž', 0x9f: 'Ÿ',
}


def _is_dropped_codepoint(cp):
    # Control characters and permanent noncharacters are removed.
    if 0x01 <= cp <= 0x08 or cp == 0x0b or 0x0e <= cp <= 0x1f:
        return True
    if 0x7f <= cp <= 0x9f:
        return True
    if 0xfdd0 <= cp <= 0xfdef:
        return True
    return (cp & 0xfffe) == 0xfffe


def _numeric_ref(digits, base):
    cp = int(digits, base)
    special = _C1_REPLACEMENTS.get(cp)
    if special is not None:
        return special
    if cp > 0x10ffff or 0xd800 <= cp <= 0xdfff:
        return '�'
    if _is_dropped_codepoint(cp):
        return ''
    return chr(cp)


def _named_ref(name):
    value = _html5.get(name)
    if value is not None:
        return value, len(name)
    # Look for the longest legacy reference (one without ';') that is a
    # prefix of *name*.
    for end in range(min(len(name) - 1, 32), 1, -1):
        value = _html5.get(name[:end])
        if value is not None:
            return value, end
    return None, 0


_REF_RE = _re.compile(r'&(#[0-9]+;?|#[xX][0-9a-fA-F]+;?|[^\t\n\f <&#;]{1,32};?)')


def _replace_ref(m):
    body = m.group(1)
    if body[0] == '#':
        if body[1] in 'xX':
            return _numeric_ref(body[2:].rstrip(';'), 16)
        return _numeric_ref(body[1:].rstrip(';'), 10)
    value, used = _named_ref(body)
    if value is None:
        return '&' + body
    return value + body[used:]


def unescape(s):
    """Convert all named and numeric character references in *s* to the
    corresponding Unicode characters, following the HTML5 rules (including
    the handling of references without a terminating semicolon).
    """
    if '&' not in s:
        return s
    return _REF_RE.sub(_replace_ref, s)


def _unescape_attr(s):
    # Like unescape(), but a named reference without ';' is left alone when
    # it is immediately followed by an alphanumeric character or '=' (the
    # HTML5 rule for character references inside attribute values).
    if '&' not in s:
        return s

    def repl(m):
        body = m.group(1)
        if body[0] == '#':
            return _replace_ref(m)
        value, used = _named_ref(body)
        if value is None:
            return '&' + body
        if not body[used - 1] == ';':
            rest = body[used:]
            if rest and (rest[0].isascii() and rest[0].isalnum() or rest[0] == '='):
                return '&' + body
        return value + body[used:]

    return _REF_RE.sub(repl, s)
