"""Inspect the code around a position of an editor or shell window.

HyperParser analyses the statement containing an index with pyparse and
answers: is the index in a string or in code, which brackets surround it,
and which expression (e.g. 'os.path.join' or 'f(x).attr') ends there.  It
is used for calltips, completions and paren matching.
"""
from keyword import iskeyword
import string

from idlelib import pyparse

# All ASCII characters that can appear in an identifier, and the subset
# that can start one.
_ASCII_ID_CHARS = frozenset(string.ascii_letters + string.digits + "_")
_ASCII_ID_FIRST_CHARS = frozenset(string.ascii_letters + "_")

# Lookup tables for the ASCII part of the identifier checks.
_IS_ASCII_ID_CHAR = [(chr(x) in _ASCII_ID_CHARS) for x in range(128)]
_IS_ASCII_ID_FIRST_CHAR = [(chr(x) in _ASCII_ID_FIRST_CHARS) for x in range(128)]


class HyperParser:

    # Characters skipped between the parts of an expression.
    _whitespace_chars = " \t\n\\"

    def __init__(self, editwin, index):
        """Analyse the statement containing *index* of editwin.text."""
        self.editwin = editwin
        self.text = text = editwin.text

        parser = pyparse.Parser(editwin.indentwidth, editwin.tabwidth)
        lno = int(float(text.index(index)))
        stopatindex = "%d.end" % lno

        if not editwin.is_shell:
            # Find a place to start parsing, looking back further when no
            # statement start is found nearby.
            for context in editwin.num_context_lines:
                startat = max(lno - context, 1)
                startatindex = repr(startat) + ".0"
                # pyparse wants a final newline; the space makes the index
                # at the end of the line count like the character before.
                parser.set_code(text.get(startatindex, stopatindex) + ' \n')
                bod = parser.find_good_parse_start(
                    editwin._build_char_in_string_func(startatindex))
                if bod is not None or startat == 1:
                    break
            parser.set_lo(bod or 0)
        else:
            # In the shell, the code starts after the last output.
            r = text.tag_prevrange("console", index)
            startatindex = r[1] if r else "1.0"
            parser.set_code(text.get(startatindex, stopatindex) + ' \n')
            parser.set_lo(0)

        # The analysed code without the added space and newline.
        self.rawtext = parser.code[:-2]
        # rawtext ends at this index of the text widget.
        self.stopatindex = stopatindex
        self.bracketing = parser.get_last_stmt_bracketing()
        # Entries of bracketing that open a level (a bracket, string or
        # comment start at that position of rawtext).
        self.isopener = [i > 0 and self.bracketing[i][1] > self.bracketing[i - 1][1]
                         for i in range(len(self.bracketing))]

        self.set_index(index)

    def set_index(self, index):
        """Move the analysed position to *index* (within the statement)."""
        indexinrawtext = (len(self.rawtext) -
                          len(self.text.get(index, self.stopatindex)))
        if indexinrawtext < 0:
            raise ValueError("Index %s precedes the analyzed statement"
                             % index)
        self.indexinrawtext = indexinrawtext
        # The last bracketing entry at or before the index ...
        self.indexbracket = 0
        while (self.indexbracket < len(self.bracketing) - 1 and
               self.bracketing[self.indexbracket + 1][0] < self.indexinrawtext):
            self.indexbracket += 1
        # ... where an entry that closes a level at the index applies.
        if (self.indexbracket < len(self.bracketing) - 1 and
                self.bracketing[self.indexbracket + 1][0] == self.indexinrawtext and
                not self.isopener[self.indexbracket + 1]):
            self.indexbracket += 1

    def _opener_char(self, i):
        return self.rawtext[self.bracketing[i][0]]

    def is_in_string(self):
        """Is the index inside a string literal?"""
        return (self.isopener[self.indexbracket] and
                self._opener_char(self.indexbracket) in ('"', "'"))

    def is_in_code(self):
        """Is the index in code (not in a string or comment)?"""
        return (not self.isopener[self.indexbracket] or
                self._opener_char(self.indexbracket) not in ('#', '"', "'"))

    def get_surrounding_brackets(self, openers='([{', mustclose=False):
        """Return (open index, close index) of the innermost brackets
        (of the kinds in *openers*) around the index, or None.

        The close index is the end of the statement when the bracket is not
        closed, unless *mustclose*, which then gives None.
        """
        bracketing = self.bracketing
        level = bracketing[self.indexbracket][1]
        before = self.indexbracket
        while (not self.isopener[before] or
               self._opener_char(before) not in openers or
               bracketing[before][1] > level):
            before -= 1
            if before < 0:
                return None
            level = min(level, bracketing[before][1])
        after = self.indexbracket + 1
        while after < len(bracketing) and bracketing[after][1] >= level:
            after += 1

        beforeindex = self.text.index("%s-%dc" % (
            self.stopatindex, len(self.rawtext) - bracketing[before][0]))
        if after >= len(bracketing) or bracketing[after][0] > len(self.rawtext):
            if mustclose:
                return None
            afterindex = self.stopatindex
        else:
            # bracketing[after] is just past the closing bracket
            afterindex = self.text.index("%s-%dc" % (
                self.stopatindex, len(self.rawtext) - (bracketing[after][0] - 1)))
        return beforeindex, afterindex

    @classmethod
    def _eat_identifier(cls, str, limit, pos):
        """Return the length of the identifier ending at *pos* (looking no
        further back than *limit*), or 0.

        Keywords other than True, False and None are not identifiers.
        """
        # Collect the characters that may be part of an identifier.
        i = pos
        while i > limit:
            ch = str[i - 1]
            if ord(ch) < 128:
                if not _IS_ASCII_ID_CHAR[ord(ch)]:
                    break
            elif not ('a' + ch).isidentifier():
                break
            i -= 1
        if i == pos:
            return 0
        word = str[i:pos]
        first = word[0]
        if ord(first) < 128:
            if not _IS_ASCII_ID_FIRST_CHAR[ord(first)]:
                return 0
        elif not word.isidentifier():
            return 0
        if not word.isidentifier():
            return 0
        if iskeyword(word) and word not in ('True', 'False', 'None'):
            return 0
        return pos - i

    def get_expression(self):
        """Return the expression ending at the index, such as 'a.b.c',
        'f(x).y' or "r'...'" ('' if there is none).

        Raise ValueError if the index is not in code.
        """
        if not self.is_in_code():
            raise ValueError("get_expression should only be called "
                             "if index is inside a code.")
        raw = self.rawtext
        bracketing = self.bracketing
        bi = self.indexbracket          # bracketing entry we are in
        limit = bracketing[bi][0]       # start of that entry
        pos = self.indexinrawtext
        start = pos                     # start of the expression found
        want_name = True                # False: an identifier was just read

        while True:
            # Skip blanks and comments (and, after a name, one dot).
            while True:
                if pos > limit and raw[pos - 1] in self._whitespace_chars:
                    pos -= 1
                elif not want_name and pos > limit and raw[pos - 1] == '.':
                    pos -= 1
                    want_name = True
                elif (pos == limit and bi > 0 and
                      raw[bracketing[bi - 1][0]] == '#'):
                    # a comment ends here: continue before it
                    bi -= 2
                    limit = bracketing[bi][0]
                    pos = bracketing[bi + 1][0]
                else:
                    break
            if not want_name:
                break  # no dot before the name: the expression starts there
            length = self._eat_identifier(raw, limit, pos)
            if length:
                pos -= length
                start = pos
                want_name = False
                continue
            if pos != limit:
                break  # an operator or other character
            # A bracket or string may end here: step back over it.
            level = bracketing[bi][1]
            while bi > 0 and bracketing[bi - 1][1] > level:
                bi -= 1
            if bracketing[bi][0] == limit:
                break  # nothing closes here
            pos = bracketing[bi][0]
            bi -= 1
            limit = bracketing[bi][0]
            start = pos
            opener = raw[pos]
            if opener in "([":
                # a call or subscript: the callee comes before it
                continue
            if opener in "'\"":
                # include a string prefix such as r or b
                while pos > 0 and raw[pos - 1] in "rRbBuUfFtT":
                    pos -= 1
                start = pos
            break
        return raw[start:self.indexinrawtext]


if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_hyperparser', verbosity=2)
