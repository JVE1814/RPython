"""A tolerant, incremental HTML tokenizer.

HTMLParser follows the tokenization rules of the HTML5 specification (it
does not build a tree): every recognised piece of markup results in a call
to one of the handle_*() methods, which subclasses override.
"""

import re
import _markupbase

from html import unescape as _unescape, _unescape_attr


__all__ = ['HTMLParser']


_WS = '\t\n\f\r '
_ws_re = re.compile(r'[\t\n\f\r ]*')
_tagname_rest_re = re.compile(r'[^\t\n\f\r />]*')
_attrname_rest_re = re.compile(r'[^\t\n\f\r />=]*')
_unquoted_value_re = re.compile(r'[^\t\n\f\r >]*')
_comment_close_re = re.compile(r'--!?>')
_text_special_re = re.compile(r'[&<]')
_charref_re = re.compile(r'&#(?:[0-9]+|[xX][0-9a-fA-F]+)')
_entityref_re = re.compile(r'&([a-zA-Z][a-zA-Z0-9]*)')
# text after the last '&' that could still turn into a longer reference
_partial_ref_re = re.compile(r'&(?:#[xX]?[0-9a-fA-F]*|[a-zA-Z0-9]*)\Z')

_ASCII_LOWER = str.maketrans('ABCDEFGHIJKLMNOPQRSTUVWXYZ',
                             'abcdefghijklmnopqrstuvwxyz')


def _lower(s):
    return s.translate(_ASCII_LOWER)


def _is_ascii_letter(c):
    return ('a' <= c <= 'z') or ('A' <= c <= 'Z')


# Return value of the token scanners when the input ends inside a token.
_INCOMPLETE = -1


class HTMLParser(_markupbase.ParserBase):
    """Find tags and other markup and call handler functions.

    Usage::

        p = HTMLParserSubclass()
        p.feed(data)
        ...
        p.close()

    Start tags are reported through handle_starttag() (or
    handle_startendtag() for ``<tag/>``), end tags through handle_endtag().
    Tag and attribute names are converted to lower case.  If
    *convert_charrefs* is true (the default), character references are
    replaced in text, otherwise handle_charref()/handle_entityref() are
    called.  Attribute values always have their references replaced.
    """

    # Elements whose content is raw text, ended only by the matching end tag.
    CDATA_CONTENT_ELEMENTS = ("script", "style", "xmp", "iframe",
                              "noembed", "noframes")
    # Elements whose content is text with character references.
    RCDATA_CONTENT_ELEMENTS = ("textarea", "title")

    def __init__(self, *, convert_charrefs=True, scripting=False):
        super().__init__()
        self.convert_charrefs = convert_charrefs
        self.scripting = scripting
        self.reset()

    def reset(self):
        """Discard all buffered data and return to the initial state."""
        self.rawdata = ''
        self._chunks = []
        self._waiting = False
        self.lasttag = '???'
        self.cdata_elem = None
        self._escapable = False
        self._end_re = None
        self._support_cdata = True
        self.__starttag_text = None
        super().reset()

    # -- public interface ---------------------------------------------------

    def feed(self, data):
        """Feed some text to the parser; complete tokens are handled now,
        an incomplete trailing token is kept until more data arrives."""
        if self._waiting and '>' not in data:
            # The pending token can only be completed by a '>'.
            self._chunks.append(data)
            return
        if self._chunks:
            self._chunks.append(data)
            data = ''.join(self._chunks)
            self._chunks = []
        self.rawdata = self.rawdata + data
        self.goahead(False)

    def close(self):
        """Handle all remaining buffered data as if the input ended here."""
        if self._chunks:
            self.rawdata = self.rawdata + ''.join(self._chunks)
            self._chunks = []
        self.goahead(True)

    def get_starttag_text(self):
        """Return the source text of the most recent start tag."""
        return self.__starttag_text

    def set_cdata_mode(self, elem, *, escapable=False):
        self.cdata_elem = _lower(elem)
        self._escapable = escapable
        if self.cdata_elem == 'plaintext':
            self._end_re = None
        else:
            self._end_re = re.compile(
                r'</%s(?=[\t\n\f\r />])' % re.escape(self.cdata_elem),
                re.IGNORECASE | re.ASCII)

    def clear_cdata_mode(self):
        self.cdata_elem = None
        self._escapable = False
        self._end_re = None

    def _set_support_cdata(self, flag=True):
        """Enable or disable recognition of <![CDATA[...]]> sections.

        CDATA sections exist only in foreign content (SVG, MathML); a tree
        builder that tracks the current namespace toggles this."""
        self._support_cdata = flag

    # -- main loop ------------------------------------------------------------

    def goahead(self, end):
        raw = self.rawdata
        n = len(raw)
        i = 0
        self._waiting = False
        while i < n:
            if self.cdata_elem is not None:
                j = self._parse_cdata_content(i, end)
                if j == _INCOMPLETE:
                    break
                i = j
                continue
            if self.convert_charrefs:
                j = raw.find('<', i)
                if j < 0:
                    j = n
                    if not end:
                        amp = raw.rfind('&', max(i, n - 40))
                        if amp >= 0 and _partial_ref_re.match(raw, amp):
                            j = amp
                if i < j:
                    self.handle_data(_unescape(raw[i:j]))
                    i = self.updatepos(i, j)
                if j == n or raw[j] != '<':
                    break
            else:
                m = _text_special_re.search(raw, i)
                j = m.start() if m else n
                if i < j:
                    self.handle_data(raw[i:j])
                    i = self.updatepos(i, j)
                if i == n:
                    break
                if raw[i] == '&':
                    k = self._parse_reference(i, end)
                    if k == _INCOMPLETE:
                        break
                    i = self.updatepos(i, k)
                    continue
            # raw[i] == '<'
            k = self._parse_markup(i, end)
            if k == _INCOMPLETE:
                break
            i = self.updatepos(i, k)
        if end and i < n:
            # Only possible for constructs that are dropped at EOF.
            i = self.updatepos(i, n)
        self.rawdata = raw[i:]

    # -- text -------------------------------------------------------------------

    def _parse_reference(self, i, end):
        """Handle a '&' in text when references are not converted."""
        raw = self.rawdata
        n = len(raw)
        m = _charref_re.match(raw, i)
        if m:
            k = m.end()
            if k == n and not end:
                return _INCOMPLETE
            self.handle_charref(raw[i + 2:k])
            if k < n and raw[k] == ';':
                k += 1
            return k
        m = _entityref_re.match(raw, i)
        if m:
            k = m.end()
            if k == n and not end:
                return _INCOMPLETE
            self.handle_entityref(m.group(1))
            if k < n and raw[k] == ';':
                k += 1
            return k
        # Not a reference (yet?)
        rest = raw[i:i + 3]
        if not end and _partial_ref_re.match(rest) and i + len(rest) == n:
            return _INCOMPLETE
        if raw.startswith('&#', i):
            k = i + 2
            if raw[k:k + 1] in ('x', 'X'):
                k += 1
            self.handle_data(raw[i:k])
            return k
        self.handle_data('&')
        return i + 1

    def _handle_text_with_refs(self, text):
        # Text of an RCDATA element when references are reported separately.
        pos = 0
        start = 0
        n = len(text)
        while pos < n:
            amp = text.find('&', pos)
            if amp < 0:
                break
            m = _charref_re.match(text, amp)
            if m:
                if start < amp:
                    self.handle_data(text[start:amp])
                self.handle_charref(text[amp + 2:m.end()])
                pos = m.end()
                if text[pos:pos + 1] == ';':
                    pos += 1
                start = pos
                continue
            m = _entityref_re.match(text, amp)
            if m:
                if start < amp:
                    self.handle_data(text[start:amp])
                self.handle_entityref(m.group(1))
                pos = m.end()
                if text[pos:pos + 1] == ';':
                    pos += 1
                start = pos
                continue
            pos = amp + 1
        if start < n:
            self.handle_data(text[start:])

    # -- raw text / RCDATA content ----------------------------------------

    def _parse_cdata_content(self, i, end):
        raw = self.rawdata
        n = len(raw)
        if self._end_re is None:                 # <plaintext>
            if not end:
                return _INCOMPLETE
            self._emit_cdata(raw[i:n])
            return self.updatepos(i, n)
        search_from = i
        while True:
            m = self._end_re.search(raw, search_from)
            if m is None:
                if not end:
                    self._waiting = True
                    return _INCOMPLETE
                self._emit_cdata(raw[i:n])
                return self.updatepos(i, n)
            j = m.start()
            result = self._scan_tag_body(m.end(), None)
            if result == _INCOMPLETE:
                if not end:
                    self._waiting = True
                    return _INCOMPLETE
                # EOF inside the end tag: the tag is dropped.
                self._emit_cdata(raw[i:j])
                return self.updatepos(i, n)
            self._emit_cdata(raw[i:j])
            self.updatepos(i, j)
            elem = self.cdata_elem
            self.clear_cdata_mode()
            self.handle_endtag(elem)
            return self.updatepos(j, result[0])

    def _emit_cdata(self, text):
        if not text:
            return
        if self._escapable:
            if self.convert_charrefs:
                self.handle_data(_unescape(text))
            else:
                self._handle_text_with_refs(text)
        else:
            self.handle_data(text)

    # -- markup -------------------------------------------------------------

    def _parse_markup(self, i, end):
        """Handle markup starting with '<' at *i*; return the index after it,
        or _INCOMPLETE if more input is needed."""
        raw = self.rawdata
        n = len(raw)
        if i + 1 >= n:
            if not end:
                return _INCOMPLETE
            self.handle_data('<')
            return n
        c = raw[i + 1]
        if _is_ascii_letter(c):
            return self._parse_starttag(i, end)
        if c == '/':
            return self._parse_endtag(i, end)
        if c == '!':
            return self._parse_markup_declaration(i, end)
        if c == '?':
            return self._parse_pi(i, end)
        self.handle_data('<')
        return i + 1

    def _scan_tag_body(self, k, attrs):
        """Scan attributes from *k* to the closing '>' of a tag.

        Appends (name, value) pairs to *attrs* (unless it is None).  Returns
        (index after '>', self_closing) or _INCOMPLETE."""
        raw = self.rawdata
        n = len(raw)
        while True:
            k = _ws_re.match(raw, k).end()
            if k >= n:
                return _INCOMPLETE
            c = raw[k]
            if c == '>':
                return k + 1, False
            if c == '/':
                if k + 1 >= n:
                    return _INCOMPLETE
                if raw[k + 1] == '>':
                    return k + 2, True
                k += 1
                continue
            # attribute name (the first character may be '=')
            name_end = _attrname_rest_re.match(raw, k + 1).end()
            name = raw[k:name_end]
            k = _ws_re.match(raw, name_end).end()
            if k >= n:
                return _INCOMPLETE
            value = None
            if raw[k] == '=':
                k = _ws_re.match(raw, k + 1).end()
                if k >= n:
                    return _INCOMPLETE
                q = raw[k]
                if q == '"' or q == "'":
                    close = raw.find(q, k + 1)
                    if close < 0:
                        return _INCOMPLETE
                    value = raw[k + 1:close]
                    k = close + 1
                elif q == '>':
                    value = ''
                else:
                    v_end = _unquoted_value_re.match(raw, k).end()
                    value = raw[k:v_end]
                    k = v_end
                value = _unescape_attr(value)
            if attrs is not None:
                attrs.append((_lower(name), value))

    def _parse_starttag(self, i, end):
        raw = self.rawdata
        name_end = _tagname_rest_re.match(raw, i + 2).end()
        attrs = []
        result = self._scan_tag_body(name_end, attrs)
        if result == _INCOMPLETE:
            if end:
                return len(raw)          # EOF in a tag: dropped
            self._waiting = True
            return _INCOMPLETE
        k, self_closing = result
        tag = _lower(raw[i + 1:name_end])
        self.__starttag_text = raw[i:k]
        self.lasttag = tag
        if self_closing:
            self.handle_startendtag(tag, attrs)
            return k
        self.handle_starttag(tag, attrs)
        if tag in self.CDATA_CONTENT_ELEMENTS or tag == 'plaintext':
            self.set_cdata_mode(tag)
        elif tag in self.RCDATA_CONTENT_ELEMENTS:
            self.set_cdata_mode(tag, escapable=True)
        elif tag == 'noscript' and self.scripting:
            self.set_cdata_mode(tag)
        return k

    def _parse_endtag(self, i, end):
        raw = self.rawdata
        n = len(raw)
        j = i + 2
        if j >= n:
            if not end:
                return _INCOMPLETE
            self.handle_data('</')
            return n
        c = raw[j]
        if c == '>':
            return j + 1                 # '</>' is ignored
        if not _is_ascii_letter(c):
            return self._parse_bogus_comment(i, j, end)
        name_end = _tagname_rest_re.match(raw, j).end()
        result = self._scan_tag_body(name_end, None)
        if result == _INCOMPLETE:
            if end:
                return n
            self._waiting = True
            return _INCOMPLETE
        tag = _lower(raw[j:name_end])
        self.handle_endtag(tag)
        return result[0]

    def _parse_bogus_comment(self, i, start, end):
        raw = self.rawdata
        k = raw.find('>', start)
        if k < 0:
            if not end:
                self._waiting = True
                return _INCOMPLETE
            self.handle_comment(raw[start:])
            return len(raw)
        self.handle_comment(raw[start:k])
        return k + 1

    def _parse_pi(self, i, end):
        raw = self.rawdata
        k = raw.find('>', i + 2)
        if k < 0:
            if not end:
                self._waiting = True
                return _INCOMPLETE
            self.handle_pi(raw[i + 2:])
            return len(raw)
        self.handle_pi(raw[i + 2:k])
        return k + 1

    def _parse_markup_declaration(self, i, end):
        raw = self.rawdata
        n = len(raw)
        j = i + 2
        rest = raw[j:j + 9]
        # Decide which construct this is; wait if the input is too short.
        if rest.startswith('--'):
            return self._parse_comment(i, end)
        if _lower(rest[:7]) == 'doctype':
            k = raw.find('>', j)
            if k < 0:
                if not end:
                    self._waiting = True
                    return _INCOMPLETE
                self.handle_decl(raw[j:])
                return n
            self.handle_decl(raw[j:k])
            return k + 1
        if self._support_cdata and rest.startswith('[CDATA['):
            k = raw.find(']]>', j + 7)
            if k < 0:
                if not end:
                    self._waiting = True
                    return _INCOMPLETE
                self.unknown_decl(raw[j + 1:])
                return n
            self.unknown_decl(raw[j + 1:k])
            return k + 3
        if not end and len(rest) < 9 and (
                '--'.startswith(rest)
                or 'doctype'.startswith(_lower(rest))
                or (self._support_cdata and '[CDATA['.startswith(rest))):
            return _INCOMPLETE
        return self._parse_bogus_comment(i, j, end)

    def _parse_comment(self, i, end):
        raw = self.rawdata
        n = len(raw)
        j = i + 4
        # abruptly closed empty comments: <!--> and <!--->
        if raw.startswith('>', j):
            self.handle_comment('')
            return j + 1
        if raw.startswith('->', j):
            self.handle_comment('')
            return j + 2
        m = _comment_close_re.search(raw, j)
        if m is None:
            if not end:
                self._waiting = True
                return _INCOMPLETE
            text = raw[j:]
            for tail in ('--!', '--', '-'):
                if text.endswith(tail):
                    text = text[:-len(tail)]
                    break
            self.handle_comment(text)
            return n
        self.handle_comment(raw[j:m.start()])
        return m.end()

    # -- overridable handlers -------------------------------------------------

    def handle_startendtag(self, tag, attrs):
        """Called for an XHTML-style empty tag '<tag ... />'."""
        self.handle_starttag(tag, attrs)
        self.handle_endtag(tag)

    def handle_starttag(self, tag, attrs):
        pass

    def handle_endtag(self, tag):
        pass

    def handle_charref(self, name):
        pass

    def handle_entityref(self, name):
        pass

    def handle_data(self, data):
        pass

    def handle_comment(self, data):
        pass

    def handle_decl(self, decl):
        pass

    def handle_pi(self, data):
        pass

    def unknown_decl(self, data):
        pass
