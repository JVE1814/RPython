"""Parse trees of regular expressions, for code that inspects patterns.

The matcher of this implementation compiles patterns inside `_sre` and never
uses these trees. `re._parser.parse()` builds them for third-party code that
walks a pattern's structure (hypothesis' from_regex, regex string
generators): a `SubPattern` is a list of `(code, argument)` items, with the
codes of `re._constants`:

    LITERAL c / NOT_LITERAL c        one character (code point or byte value)
    ANY None                         '.'
    AT AT_*                          an anchor: ^ $ \\A \\Z \\b \\B
    IN [items]                       a character class; items are
                                     (NEGATE, None) first if negated,
                                     (LITERAL, c), (RANGE, (lo, hi)),
                                     (CATEGORY, CATEGORY_*)
    BRANCH (None, [SubPattern...])   alternatives
    SUBPATTERN (group, add_flags, del_flags, SubPattern)
                                     a group; group is None if not capturing
    MAX_REPEAT / MIN_REPEAT / POSSESSIVE_REPEAT (min, max, SubPattern)
                                     greedy / lazy / possessive repetition,
                                     max is MAXREPEAT when unbounded
    GROUPREF group                   a back reference
    GROUPREF_EXISTS (group, yes, no) a conditional; no may be None
    ASSERT / ASSERT_NOT (direction, SubPattern)
                                     lookahead (1) or lookbehind (-1)
    ATOMIC_GROUP SubPattern          (?>...)

The patterns are expected to be valid (they were compiled before); errors
are reported with re.error all the same.
"""

from . import _constants as C
from ._constants import MAXREPEAT, PatternError

_FLAG_LETTERS = {
    "a": C.SRE_FLAG_ASCII, "i": C.SRE_FLAG_IGNORECASE, "L": C.SRE_FLAG_LOCALE,
    "m": C.SRE_FLAG_MULTILINE, "s": C.SRE_FLAG_DOTALL, "x": C.SRE_FLAG_VERBOSE,
    "u": C.SRE_FLAG_UNICODE,
}
_SIMPLE_ESCAPES = {"a": 7, "b": 8, "f": 12, "n": 10, "r": 13, "t": 9, "v": 11, "\\": 92}
_CATEGORIES = {
    "d": C.CATEGORY_DIGIT, "D": C.CATEGORY_NOT_DIGIT,
    "s": C.CATEGORY_SPACE, "S": C.CATEGORY_NOT_SPACE,
    "w": C.CATEGORY_WORD, "W": C.CATEGORY_NOT_WORD,
}
_ANCHORS = {
    "A": C.AT_BEGINNING_STRING, "Z": C.AT_END_STRING, "z": C.AT_END_STRING,
    "b": C.AT_BOUNDARY, "B": C.AT_NON_BOUNDARY,
}
_HEX = "0123456789abcdefABCDEF"
_UNBOUNDED = MAXREPEAT - 1


class State:
    """What one parse learns about the whole pattern."""

    def __init__(self, flags=0):
        self.flags = flags
        self.groupdict = {}
        self.ngroups = 0

    @property
    def groups(self):
        # group numbers run from 1; the count includes the implicit group 0
        return self.ngroups + 1

    def opengroup(self, name=None):
        self.ngroups += 1
        if name is not None:
            self.groupdict[name] = self.ngroups
        return self.ngroups


class SubPattern(list):
    """A sequence of (code, argument) items (see the module docstring)."""

    def __init__(self, state, data=None):
        super().__init__(data or ())
        self.state = state

    @property
    def data(self):
        return self

    def getwidth(self):
        """(shortest, longest) number of characters this sequence can match."""
        lo = hi = 0
        for code, av in self:
            a, b = _item_width(code, av)
            lo += a
            hi += b
        return min(lo, _UNBOUNDED), min(hi, _UNBOUNDED)

    def __repr__(self):
        return "SubPattern(%s)" % list.__repr__(self)


def _item_width(code, av):
    if code in (C.LITERAL, C.NOT_LITERAL, C.IN, C.ANY):
        return 1, 1
    if code in (C.AT, C.ASSERT, C.ASSERT_NOT):
        return 0, 0
    if code == C.SUBPATTERN:
        return av[3].getwidth()
    if code == C.ATOMIC_GROUP:
        return av.getwidth()
    if code == C.BRANCH:
        widths = [b.getwidth() for b in av[1]]
        return min(w[0] for w in widths), max(w[1] for w in widths)
    if code in (C.MAX_REPEAT, C.MIN_REPEAT, C.POSSESSIVE_REPEAT):
        a, b = av[2].getwidth()
        top = _UNBOUNDED if av[1] == MAXREPEAT else b * av[1]
        return a * av[0], min(top, _UNBOUNDED)
    if code == C.GROUPREF_EXISTS:
        yes = av[1].getwidth()
        no = av[2].getwidth() if av[2] is not None else (0, 0)
        return min(yes[0], no[0]), max(yes[1], no[1])
    return 0, _UNBOUNDED  # GROUPREF: unknown


class _Reader:
    def __init__(self, text, state, is_bytes):
        self.s = text
        self.i = 0
        self.state = state
        self.is_bytes = is_bytes
        self.verbose = bool(state.flags & C.SRE_FLAG_VERBOSE)
        self.depth = 0

    # -- helpers ----------------------------------------------------------

    def error(self, msg, at=None):
        return PatternError(msg, self.s, self.i if at is None else at)

    def peek(self):
        return self.s[self.i] if self.i < len(self.s) else None

    def take(self):
        ch = self.peek()
        if ch is not None:
            self.i += 1
        return ch

    def accept(self, text):
        if self.s.startswith(text, self.i):
            self.i += len(text)
            return True
        return False

    def skip_verbose(self):
        if not self.verbose:
            return
        while self.i < len(self.s):
            ch = self.s[self.i]
            if ch in " \t\n\r\v\f":
                self.i += 1
            elif ch == "#":
                end = self.s.find("\n", self.i)
                self.i = len(self.s) if end < 0 else end + 1
            else:
                return

    def read_until(self, stop):
        end = self.s.find(stop, self.i)
        if end < 0:
            raise self.error("missing %s" % stop)
        text = self.s[self.i:end]
        self.i = end + 1
        return text

    # -- structure --------------------------------------------------------

    def alternation(self):
        branches = [self.sequence()]
        while self.accept("|"):
            branches.append(self.sequence())
        if len(branches) == 1:
            return branches[0]
        return SubPattern(self.state, [(C.BRANCH, (None, branches))])

    def sequence(self):
        items = SubPattern(self.state)
        while True:
            self.skip_verbose()
            ch = self.peek()
            if ch is None or ch == "|" or (ch == ")" and self.depth):
                return items
            if ch == ")":
                raise self.error("unbalanced parenthesis")
            start = self.i
            item = self.atom()
            if item is None:  # a comment or a global flag group
                continue
            self.skip_verbose()
            item = self.repeat(item, start)
            items.append(item)

    def repeat(self, item, start):
        at = self.i
        ch = self.peek()
        if ch == "*":
            lo, hi = 0, MAXREPEAT
        elif ch == "+":
            lo, hi = 1, MAXREPEAT
        elif ch == "?":
            lo, hi = 0, 1
        elif ch == "{":
            bounds = self.counted()
            if bounds is None:
                return item
            lo, hi = bounds
            self.i -= 1  # counted() consumed the closing brace
        else:
            return item
        self.i += 1
        if item[0] == C.AT:
            raise self.error("nothing to repeat", start)
        if self.accept("?"):
            code = C.MIN_REPEAT
        elif self.accept("+"):
            code = C.POSSESSIVE_REPEAT
        else:
            code = C.MAX_REPEAT
        if self.peek() is not None and self.peek() in "*+?" and not self.verbose:
            raise self.error("multiple repeat", at)
        return (code, (lo, hi, SubPattern(self.state, [item])))

    def counted(self):
        """{m}, {m,}, {,n}, {m,n} at the reader (else None: a literal brace)."""
        end = self.s.find("}", self.i)
        if end < 0:
            return None
        body = self.s[self.i + 1:end]
        lo_text, comma, hi_text = body.partition(",")
        if not (lo_text.isdigit() or lo_text == "") or not (hi_text.isdigit() or hi_text == ""):
            return None
        if not comma and not lo_text:
            return None
        if not lo_text.isascii() or not hi_text.isascii():
            return None
        lo = int(lo_text) if lo_text else 0
        hi = int(hi_text) if hi_text else (MAXREPEAT if comma else lo)
        if hi < lo:
            raise self.error("min repeat greater than max repeat", self.i + 1)
        self.i = end + 1
        return lo, hi

    def atom(self):
        ch = self.take()
        if ch == ".":
            return (C.ANY, None)
        if ch == "^":
            return (C.AT, C.AT_BEGINNING)
        if ch == "$":
            return (C.AT, C.AT_END)
        if ch == "[":
            return self.char_class()
        if ch == "(":
            return self.group()
        if ch == "\\":
            return self.escape()
        if ch in "*+?":
            raise self.error("nothing to repeat", self.i - 1)
        if ch == "{" and self.counted_at(self.i - 1):
            raise self.error("nothing to repeat", self.i - 1)
        return (C.LITERAL, ord(ch))

    def counted_at(self, pos):
        save = self.i
        self.i = pos
        try:
            return self.counted() is not None
        finally:
            self.i = save

    def group(self):
        if not self.accept("?"):
            gid = self.state.opengroup()
            return (C.SUBPATTERN, (gid, 0, 0, self.nested()))
        ch = self.take()
        if ch == ":":
            return (C.SUBPATTERN, (None, 0, 0, self.nested()))
        if ch == "P":
            if self.accept("<"):
                name = self.read_until(">")
                gid = self.state.opengroup(name)
                return (C.SUBPATTERN, (gid, 0, 0, self.nested()))
            if self.accept("="):
                name = self.read_until(")")
                if name not in self.state.groupdict:
                    raise self.error("unknown group name %r" % name)
                return (C.GROUPREF, self.state.groupdict[name])
            raise self.error("unknown extension ?P")
        if ch == "#":
            self.read_until(")")
            return None
        if ch in "=!":
            sub = self.nested()
            return (C.ASSERT if ch == "=" else C.ASSERT_NOT, (1, sub))
        if ch == "<":
            kind = self.take()
            if kind not in ("=", "!"):
                raise self.error("unknown extension ?<")
            sub = self.nested()
            return (C.ASSERT if kind == "=" else C.ASSERT_NOT, (-1, sub))
        if ch == ">":
            return (C.ATOMIC_GROUP, self.nested())
        if ch == "(":
            ref = self.read_until(")")
            gid = int(ref) if ref.isdigit() else self.state.groupdict.get(ref)
            if gid is None:
                raise self.error("unknown group name %r" % ref)
            self.depth += 1
            yes = self.sequence()
            no = self.sequence() if self.accept("|") else None
            if self.peek() == "|":
                raise self.error("conditional backref with more than two branches")
            self.close_group()
            return (C.GROUPREF_EXISTS, (gid, yes, no))
        if ch is not None and (ch in _FLAG_LETTERS or ch == "-"):
            return self.flag_group(ch)
        raise self.error("unknown extension ?" + (ch or ""))

    def flag_group(self, ch):
        add = remove = 0
        target = "add"
        while True:
            if ch == "-":
                target = "remove"
            elif ch in _FLAG_LETTERS:
                if target == "add":
                    add |= _FLAG_LETTERS[ch]
                else:
                    remove |= _FLAG_LETTERS[ch]
            elif ch == ")":
                # flags for the whole pattern
                self.state.flags |= add
                if add & C.SRE_FLAG_VERBOSE:
                    self.verbose = True
                return None
            elif ch == ":":
                saved = self.verbose
                if add & C.SRE_FLAG_VERBOSE:
                    self.verbose = True
                elif remove & C.SRE_FLAG_VERBOSE:
                    self.verbose = False
                sub = self.nested()
                self.verbose = saved
                return (C.SUBPATTERN, (None, add, remove, sub))
            else:
                raise self.error("unknown flag")
            ch = self.take()
            if ch is None:
                raise self.error("missing -, : or )")

    def nested(self):
        self.depth += 1
        sub = self.alternation()
        self.close_group()
        return sub

    def close_group(self):
        if not self.accept(")"):
            raise self.error("missing ), unterminated subpattern")
        self.depth -= 1

    # -- escapes and classes ------------------------------------------------

    def escape(self):
        ch = self.take()
        if ch is None:
            raise self.error("bad escape (end of pattern)")
        if ch in _ANCHORS:
            return (C.AT, _ANCHORS[ch])
        if ch in _CATEGORIES:
            return (C.IN, [(C.CATEGORY, _CATEGORIES[ch])])
        if ch == "0":
            return (C.LITERAL, self.octal("0"))
        if ch.isdigit() and ch.isascii():
            # a back reference, or three octal digits
            digits = ch
            if self.peek() is not None and self.peek() in "0123456789":
                digits += self.take()
                if (digits[0] in "01234567" and digits[1] in "01234567"
                        and self.peek() is not None and self.peek() in "01234567"):
                    value = int(digits + self.take(), 8)
                    if value > 0o377:
                        raise self.error("octal escape value out of range")
                    return (C.LITERAL, value)
            gid = int(digits)
            if gid > self.state.ngroups:
                raise self.error("invalid group reference %d" % gid)
            return (C.GROUPREF, gid)
        return (C.LITERAL, self.char_escape(ch))

    def octal(self, first):
        digits = first
        while len(digits) < 3 and self.peek() is not None and self.peek() in "01234567":
            digits += self.take()
        return int(digits, 8)

    def char_escape(self, ch):
        """Code of the escaped character `ch` (after the backslash)."""
        if ch in _SIMPLE_ESCAPES:
            return _SIMPLE_ESCAPES[ch]
        if ch == "x":
            return self.hex_digits(2, "x")
        if ch in "uU" and not self.is_bytes:
            return self.hex_digits(4 if ch == "u" else 8, ch)
        if ch == "N" and not self.is_bytes:
            if not self.accept("{"):
                raise self.error("missing {")
            name = self.read_until("}")
            import unicodedata
            try:
                return ord(unicodedata.lookup(name))
            except KeyError:
                raise self.error("undefined character name %r" % name) from None
        if ch.isascii() and ch.isalnum():
            raise self.error("bad escape \\%s" % ch, self.i - 2)
        return ord(ch)

    def hex_digits(self, n, letter):
        text = self.s[self.i:self.i + n]
        if len(text) != n or any(c not in _HEX for c in text):
            raise self.error("incomplete escape \\%s%s" % (letter, text), self.i - 2)
        self.i += n
        value = int(text, 16)
        if value > 0x10FFFF:
            raise self.error("bad escape \\%s%s" % (letter, text), self.i - n - 2)
        return value

    def class_member(self):
        """One member of a class: ('char', code) or ('category', code)."""
        ch = self.take()
        if ch is None:
            raise self.error("unterminated character set")
        if ch != "\\":
            return "char", ord(ch)
        ch = self.take()
        if ch is None:
            raise self.error("bad escape (end of pattern)")
        if ch in _CATEGORIES:
            return "category", _CATEGORIES[ch]
        if ch == "b":
            return "char", 8
        if ch in "01234567":
            return "char", self.octal(ch)
        return "char", self.char_escape(ch)

    def char_class(self):
        start = self.i - 1
        negate = self.accept("^")
        items = []
        first = True
        while True:
            ch = self.peek()
            if ch is None:
                raise self.error("unterminated character set", start)
            if ch == "]" and not first:
                self.i += 1
                break
            first = False
            kind, lo = self.class_member()
            if kind == "category":
                items.append((C.CATEGORY, lo))
                continue
            if self.peek() == "-" and self.s[self.i + 1:self.i + 2] not in ("]", ""):
                self.i += 1
                kind2, hi = self.class_member()
                if kind2 == "category":
                    raise self.error("bad character range", start)
                if hi < lo:
                    raise self.error("bad character range", start)
                items.append((C.RANGE, (lo, hi)) if hi != lo else (C.LITERAL, lo))
            else:
                items.append((C.LITERAL, lo))
        if len(items) == 1 and items[0][0] == C.LITERAL:
            return (C.NOT_LITERAL if negate else C.LITERAL, items[0][1])
        if negate:
            items.insert(0, (C.NEGATE, None))
        return (C.IN, items)


def parse(pattern, flags=0, state=None):
    """The parse tree (a SubPattern) of `pattern` (str or bytes)."""
    if isinstance(pattern, (bytes, bytearray)):
        text, is_bytes = bytes(pattern).decode("latin-1"), True
    elif isinstance(pattern, str):
        text, is_bytes = pattern, False
    else:
        raise TypeError("first argument must be string or compiled pattern")
    if state is None:
        state = State()
    state.flags |= flags
    reader = _Reader(text, state, is_bytes)
    tree = reader.alternation()
    if reader.peek() is not None:
        raise reader.error("unbalanced parenthesis")
    if not is_bytes and not state.flags & (C.SRE_FLAG_ASCII | C.SRE_FLAG_LOCALE):
        state.flags |= C.SRE_FLAG_UNICODE
    return tree
