"""XML processing.

Sub-packages: dom (Document Object Model, minidom and pulldom), parsers
(the expat parser interface), sax (the SAX 2 interfaces) and etree
(ElementTree).
"""

__all__ = ["dom", "parsers", "sax", "etree"]


# Character classes of the XML 1.0 (fifth edition) grammar, as regular
# expression bracket contents.
_NAME_START = (':A-Z_a-z\xc0-\xd6\xd8-\xf6\xf8-˿Ͱ-ͽ'
               'Ϳ-῿‌‍⁰-↏Ⰰ-⿯'
               '、-퟿豈-﷏ﷰ-�\U00010000-\U000effff')
_NAME_REST = _NAME_START + r'\-.0-9\xb7̀-ͯ‿⁀'
_CHAR = '\t\n\r\x20-퟿-�\U00010000-\U0010ffff'

_name_re = None
_text_re = None


def is_valid_name(name):
    """Return True if *name* matches the XML ``Name`` production."""
    global _name_re
    if _name_re is None:
        import re
        _name_re = re.compile('[%s][%s]*' % (_NAME_START, _NAME_REST))
    return _name_re.fullmatch(name) is not None


def is_valid_text(text):
    """Return True if every character of *text* may appear in an XML
    document (the ``Char`` production)."""
    global _text_re
    if _text_re is None:
        import re
        _text_re = re.compile('[%s]*' % _CHAR)
    return _text_re.fullmatch(text) is not None
