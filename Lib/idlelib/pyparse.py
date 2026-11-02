"""Light-weight analysis of Python source for smart indentation.

Parser looks only at the end of a code fragment: it finds where the last
statement begins, whether that statement continues on the next line (open
bracket, backslash or unterminated triple-quoted string) and the indentation
the next line should get.  It is used by the editor's newline-and-indent
command and by HyperParser (expression and bracket lookup).

Strings and comments are recognised but their content is ignored, so code
is processed quickly even when it contains long strings.
"""
import re

# Kinds of continuation of the last line.
(C_NONE,                # the next line starts a new statement
 C_BACKSLASH,           # the last line ends with a backslash
 C_STRING_FIRST_LINE,   # inside a triple-quoted string that began on it
 C_STRING_NEXT_LINES,   # inside a triple-quoted string begun earlier
 C_BRACKET,             # inside open brackets
) = range(5)

# Lines starting with these words begin a statement; they are good places
# to start parsing.  ('else' only as a clause; words that may begin a
# continuation line -- 'if', 'for', 'yield', 'from', ... -- are left out.)
_synchre = re.compile(r"""
    ^ [ \t]*
    (?: (?: while | def | return | assert | break | class | continue
          | elif | try | except | raise | import | with | del | global
          | nonlocal | pass | finally
        ) \b
      | else [ \t]* :
    )
""", re.VERBOSE | re.MULTILINE).search

# A line that is blank or a comment starting in column 0 without a space
# after '#' (usually commented-out code); such lines do not count as the
# last statement.
_junkre = re.compile(r"[ \t]*(?:\#\S.*)?\n").match

# A string literal starting at the match position.  Triple-quoted strings
# may span lines and may be unterminated; others end at a newline.
_match_stringre = re.compile(r"""
    \"\"\" (?: [^"\\] | \\. | "(?!"") )* (?: \"\"\" )?
  | '''    (?: [^'\\] | \\. | '(?!'') )* (?: ''' )?
  | " (?: [^"\\\n] | \\. )* "?
  | ' (?: [^'\\\n] | \\. )* '?
""", re.VERBOSE | re.DOTALL).match

# The first item after an open bracket (skipping blanks; not a comment).
_itemre = re.compile(r"[ \t]*[^\s#\\]").match

# Statements after which the next line is dedented.
_closere = re.compile(r"""
    \s*
    (?: return | break | continue | raise | pass )
    \b
""", re.VERBOSE).match

# A run of characters that are not brackets, quotes, comments or
# backslashes.
_chew_ordinaryre = re.compile(r"""
    [^[\](){}#'"\\]+
""", re.VERBOSE).match


class ParseMap(dict):
    """A str.translate() table mapping every character not listed to 'x'."""

    def __missing__(self, key):
        return 120  # ord('x')


# Squeeze code for the line/continuation scan: brackets become '(' and ')',
# quotes, backslashes, newlines and '#' are kept, anything else is 'x'.
trans = ParseMap.fromkeys(range(128), 120)
trans.update((ord(c), ord('(')) for c in "({[")
trans.update((ord(c), ord(')')) for c in ")}]")
trans.update((ord(c), ord(c)) for c in "\"'\\\n#")


class Parser:
    """Analyse a code fragment (see the module docstring)."""

    def __init__(self, indentwidth, tabwidth):
        self.indentwidth = indentwidth
        self.tabwidth = tabwidth

    def set_code(self, s):
        """Set the code to analyse; it must be empty or end with '\\n'."""
        assert len(s) == 0 or s[-1] == '\n'
        self.code = s
        self.study_level = 0

    def find_good_parse_start(self, is_char_in_string):
        """Return the index of a statement start close to the end of the
        code, or None when every candidate is in a string.

        *is_char_in_string(index)* tells whether a character is inside a
        string (from the colorizer); candidates inside strings are skipped.
        """
        code = self.code
        pos = None
        # Look back from the end for a line that opens a block, a few times.
        limit = len(code)
        for _ in range(5):
            colon = code.rfind(":\n", 0, limit)
            if colon < 0:
                break
            linestart = code.rfind('\n', 0, colon) + 1
            m = _synchre(code, linestart, limit)
            if m and not is_char_in_string(m.start()):
                pos = m.start()
                break
            limit = linestart
        if pos is None:
            # Give it one more try from the start of the code.
            m = _synchre(code)
            if m and not is_char_in_string(m.start()):
                pos = m.start()
            return pos
        # Move forward to the last statement start that is not in a string.
        m = _synchre(code, pos + 1)
        while m:
            start, end = m.span()
            if not is_char_in_string(start):
                pos = start
            m = _synchre(code, end)
        return pos

    def set_lo(self, lo):
        """Drop the code before index *lo* (which must start a line)."""
        assert lo == 0 or self.code[lo - 1] == '\n'
        if lo > 0:
            self.code = self.code[lo:]

    # -- level 1: lines and continuation

    def _study1(self):
        """Compute goodlines (numbers of the lines on which a statement
        may start, followed by the line count) and continuation."""
        if self.study_level >= 1:
            return
        self.study_level = 1

        code = self.code.translate(trans)
        n = len(code)
        goodlines = [0]
        depth = 0           # bracket nesting
        lno = 0             # current line number
        escaped_eol = False  # did the last line end with a backslash?
        continuation = C_NONE
        i = 0
        while i < n:
            ch = code[i]
            if ch == 'x':
                i += 1
                continue
            if ch == '\n':
                lno += 1
                escaped_eol = False
                if depth == 0:
                    goodlines.append(lno)
                i += 1
            elif ch == '(':
                depth += 1
                i += 1
            elif ch == ')':
                if depth:
                    depth -= 1
                i += 1
            elif ch == '#':
                nl = code.find('\n', i)
                i = n if nl < 0 else nl
            elif ch == '\\':
                if code[i + 1:i + 2] == '\n':
                    lno += 1
                    escaped_eol = True
                i += 2
            else:  # a quote
                quote = code[i:i + 3] if code[i:i + 3] == ch * 3 else ch
                start_line = lno
                i += len(quote)
                closed = False
                while i < n:
                    c = code[i]
                    if c == '\\':
                        if code[i + 1:i + 2] == '\n':
                            lno += 1
                        i += 2
                    elif c == '\n':
                        lno += 1
                        i += 1
                        if len(quote) == 1:
                            # an unterminated one-line string ends here
                            if depth == 0:
                                goodlines.append(lno)
                            closed = True
                            break
                    elif code.startswith(quote, i):
                        i += len(quote)
                        closed = True
                        break
                    else:
                        i += 1
                if not closed:
                    # inside a triple-quoted string at the end of the code
                    if start_line == lno - 1:
                        continuation = C_STRING_FIRST_LINE
                    else:
                        continuation = C_STRING_NEXT_LINES
                    break
        else:
            if depth:
                continuation = C_BRACKET
            elif escaped_eol:
                continuation = C_BACKSLASH
        if goodlines[-1] != lno:
            goodlines.append(lno)
        self.goodlines = goodlines
        self.continuation = continuation

    def get_continuation_type(self):
        """Return how the last line continues (one of the C_ constants)."""
        self._study1()
        return self.continuation

    # -- level 2: the last statement

    def _study2(self):
        """Analyse the last statement: its extent, last interesting
        character, last open bracket and bracketing levels."""
        if self.study_level >= 2:
            return
        self._study1()
        self.study_level = 2

        code = self.code
        # Offsets of the line starts (the code ends with a newline, so the
        # last entry is the length of the code).
        line_offsets = [0]
        line_offsets.extend(i + 1 for i, c in enumerate(code) if c == '\n')
        goodlines = self.goodlines

        start = end = 0
        k = len(goodlines) - 1
        while k > 0:
            start = line_offsets[goodlines[k - 1]]
            end = line_offsets[goodlines[k]]
            if not _junkre(code, start):
                break
            k -= 1
        else:
            start = end = 0
        self.stmt_start, self.stmt_end = start, end

        lastch = ""
        stack = []                  # positions of the open brackets
        bracketing = [(start, 0)]   # (position, nesting level from there)
        p = start
        while p < end:
            m = _chew_ordinaryre(code, p, end)
            if m:
                run = code[p:m.end()].rstrip(" \t\n")
                if run:
                    lastch = run[-1]
                p = m.end()
                if p >= end:
                    break
            ch = code[p]
            if ch in "([{":
                stack.append(p)
                bracketing.append((p, len(stack)))
                lastch = ch
                p += 1
            elif ch in ")]}":
                if stack:
                    stack.pop()
                lastch = ch
                p += 1
                bracketing.append((p, len(stack)))
            elif ch in "\"'":
                bracketing.append((p, len(stack) + 1))
                lastch = ch
                p = _match_stringre(code, p, end).end()
                bracketing.append((p, len(stack)))
            elif ch == '#':
                bracketing.append((p, len(stack) + 1))
                p = code.find('\n', p, end) + 1
                assert p > 0
                bracketing.append((p, len(stack)))
            else:  # backslash
                p += 1
                assert p < end
                if code[p] != '\n':
                    # not valid Python, but remember it
                    lastch = ch + code[p]
                p += 1

        self.lastch = lastch
        self.lastopenbracketpos = stack[-1] if stack else None
        self.stmt_bracketing = tuple(bracketing)

    def compute_bracket_indent(self):
        """Return the indentation (in columns) of a line continuing an
        open bracket: aligned with the first item after the bracket, or
        one level deeper than the bracket's line if nothing follows."""
        self._study2()
        assert self.continuation == C_BRACKET
        code = self.code
        n = len(code)
        bracket = self.lastopenbracketpos
        line_start = code.rfind('\n', 0, bracket) + 1
        j = bracket + 1
        i = line_start
        while j < n:
            m = _itemre(code, j)
            if m:
                return len(code[i:m.end() - 1].expandtabs(self.tabwidth))
            # nothing on this line: continue with the next one
            i = j = code.find('\n', j) + 1
            if j == 0:
                break
        # nothing follows the bracket: indent its line one more level
        j = line_start
        while code[j] in " \t":
            j += 1
        return (len(code[line_start:j].expandtabs(self.tabwidth)) +
                self.indentwidth)

    def get_num_lines_in_stmt(self):
        """Return the number of lines of the last statement."""
        self._study1()
        goodlines = self.goodlines
        return goodlines[-1] - goodlines[-2]

    def compute_backslash_indent(self):
        """Return the indentation of a line continuing a backslash line.

        After an assignment the line aligns with the value; otherwise with
        what follows the first word of the statement.
        """
        self._study2()
        assert self.continuation == C_BACKSLASH
        code = self.code
        i = self.stmt_start
        while code[i] in " \t":
            i += 1
        first_nonblank = i
        line_end = code.find('\n', i) + 1

        # Look for a top-level '=' (not part of ==, <=, >=, !=).
        depth = 0
        found = False
        while i < line_end:
            ch = code[i]
            if ch in "([{":
                depth += 1
                i += 1
            elif ch in ")]}":
                if depth:
                    depth -= 1
                i += 1
            elif ch in "\"'":
                i = _match_stringre(code, i, line_end).end()
            elif ch == '#':
                break
            elif (depth == 0 and ch == '=' and
                  (i == 0 or code[i - 1] not in "=<>!") and
                  code[i + 1] != '='):
                found = True
                break
            else:
                i += 1
        if found:
            i += 1  # past the '='
            # Only a backslash after the '=': align as without it.
            found = re.match(r"\s*\\", code[i:line_end]) is None
        if not found:
            i = first_nonblank
            while code[i] not in " \t\n":
                i += 1
        return len(code[self.stmt_start:i].expandtabs(self.tabwidth)) + 1

    def get_base_indent_string(self):
        """Return the leading whitespace of the last statement."""
        self._study2()
        i, n = self.stmt_start, self.stmt_end
        j = i
        code = self.code
        while j < n and code[j] in " \t":
            j += 1
        return code[i:j]

    def is_block_opener(self):
        """Return True if the last statement ends with ':'."""
        self._study2()
        return self.lastch == ':'

    def is_block_closer(self):
        """Return True if the last statement is return, break, continue,
        raise or pass."""
        self._study2()
        return _closere(self.code, self.stmt_start) is not None

    def get_last_stmt_bracketing(self):
        """Return the bracketing of the last statement: a tuple of
        (index, level) pairs, each saying the nesting level (brackets,
        strings and comments count as a level) from that index on."""
        self._study2()
        return self.stmt_bracketing


if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_pyparse', verbosity=2)
