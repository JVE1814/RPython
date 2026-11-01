"""Shared base class for markup parsers (used by html.parser).

ParserBase keeps track of the position (line number and column) in the
input and knows how to skip over SGML-style declarations such as
``<!DOCTYPE ...>`` and marked sections ``<![...]]>``.
"""

import re

_name_re = re.compile(r'[a-zA-Z][-_.a-zA-Z0-9]*\s*')
_decl_string_re = re.compile(r'(\'[^\']*\'|"[^"]*")\s*')
_comment_end_re = re.compile(r'--\s*>')
_marked_end = {
    'temp': ']]>', 'cdata': ']]>', 'ignore': ']]>', 'include': ']]>',
    'rcdata': ']]>', 'if': ']>', 'else': ']>', 'endif': ']>',
}


class ParserBase:
    """Base class for incremental markup parsers.

    Subclasses keep the unparsed input in ``self.rawdata``.
    """

    def __init__(self):
        if type(self) is ParserBase:
            raise RuntimeError(
                "_markupbase.ParserBase must be subclassed")

    def reset(self):
        self.lineno = 1
        self.offset = 0

    def getpos(self):
        """Return the current (line number, column offset) pair."""
        return self.lineno, self.offset

    def updatepos(self, i, j):
        """Advance the position over rawdata[i:j] and return j."""
        if i >= j:
            return j
        raw = self.rawdata
        newlines = raw.count('\n', i, j)
        if newlines:
            self.lineno += newlines
            self.offset = j - (raw.rindex('\n', i, j) + 1)
        else:
            self.offset += j - i
        return j

    _decl_otherchars = ''

    def parse_declaration(self, i):
        """Parse a '<!...>' declaration starting at *i*.

        Returns the index just past the declaration, or -1 when the input
        ends before the declaration is complete.
        """
        raw = self.rawdata
        j = i + 2
        if raw[i:j] != '<!':
            raise AssertionError('unexpected call to parse_declaration')
        if raw[j:j + 1] == '>':
            return j + 1                 # '<!>' is an empty declaration
        if raw[j:j + 1] in ('-', ''):
            return -1                    # possibly an incomplete comment
        if raw.startswith('--', j):
            return self.parse_comment(i)
        if raw[j] == '[':
            return self.parse_marked_section(i)
        name, j = self._scan_name(j, i)
        if j < 0:
            return j
        if name == 'doctype':
            self._decl_otherchars = ''
        n = len(raw)
        while j < n:
            c = raw[j]
            if c == '>':
                data = raw[i + 2:j]
                if name == 'doctype':
                    self.handle_decl(data)
                else:
                    self.unknown_decl(data)
                return j + 1
            if c in '"\'':
                m = _decl_string_re.match(raw, j)
                if not m:
                    return -1
                j = m.end()
            elif c.isalpha():
                _, j = self._scan_name(j, i)
                if j < 0:
                    return j
            elif c == '[' and name == 'doctype':
                end = raw.find(']', j)
                if end < 0:
                    return -1
                j = end + 1
            else:
                j += 1
        return -1

    def parse_marked_section(self, i, report=1):
        """Parse a marked section '<![keyword[ ... ]]>' starting at *i*."""
        raw = self.rawdata
        if raw[i:i + 3] != '<![':
            raise AssertionError('unexpected call to parse_marked_section')
        name, j = self._scan_name(i + 3, i)
        if j < 0:
            return j
        terminator = _marked_end.get(name, ']]>')
        end = raw.find(terminator, i + 3)
        if end < 0:
            return -1
        if report:
            self.unknown_decl(raw[i + 3:end])
        return end + len(terminator)

    def parse_comment(self, i, report=1):
        """Parse an SGML comment '<!-- ... -->' starting at *i*."""
        raw = self.rawdata
        if raw[i:i + 4] != '<!--':
            raise AssertionError('unexpected call to parse_comment')
        m = _comment_end_re.search(raw, i + 4)
        if not m:
            return -1
        if report:
            self.handle_comment(raw[i + 4:m.start()])
        return m.end()

    def _scan_name(self, i, declstartpos):
        raw = self.rawdata
        if i == len(raw):
            return None, -1
        m = _name_re.match(raw, i)
        if m:
            name = m.group().strip()
            if i + len(m.group()) == len(raw):
                return None, -1          # the name might continue
            return name.lower(), m.end()
        self.updatepos(declstartpos, i)
        raise AssertionError('expected name token at %r'
                             % raw[declstartpos:declstartpos + 20])

    def unknown_decl(self, data):
        """Called for declarations the parser does not understand."""
        pass
