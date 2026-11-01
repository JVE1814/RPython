"""An incremental ("feed") parser for email messages.

Data can be pushed in arbitrary pieces with feed(); close() finishes the
parse and returns the root message object.  The parser never raises on
malformed input; problems are recorded as defects on the message objects.
"""

__all__ = ['FeedParser', 'BytesFeedParser']

import re
from collections import deque

from email import errors
from email._policybase import compat32

# Line ending patterns (only CR and LF end lines in email).
NLCRE = re.compile(r'\r\n|\r|\n')
NLCRE_bol = re.compile(r'(\r\n|\r|\n)')
NLCRE_eol = re.compile(r'(\r\n|\r|\n)\Z')
NLCRE_crack = re.compile(r'(\r\n|\r|\n)')
# A header line: a field name and colon, an envelope 'From ' line, or a
# continuation line.
headerRE = re.compile(r'^(From |[\041-\071\073-\176]*:|[\t ])')
EMPTYSTRING = ''
NL = '\n'
# What may follow a boundary marker on its line.
boundaryendRE = re.compile(
    r'(?P<end>--)?(?P<ws>[ \t]*)(?P<linesep>\r\n|\r|\n)?$')

_LINE_RE = re.compile(r'[^\r\n]*(?:\r\n|\r|\n)')


class _NeedMoreData:
    def __repr__(self):
        return 'NeedMoreData'


NeedMoreData = _NeedMoreData()


class BufferedSubFile:
    """A line buffer fed with arbitrary chunks of text.

    readline() returns complete lines (with their line ending), '' at the
    end of the data (or at a "false EOF" announced by a pushed matcher), or
    NeedMoreData when no complete line is available yet.
    """

    def __init__(self):
        self._partial = []          # pieces of an incomplete last line
        self._lines = deque()       # complete lines
        self._eofstack = []         # predicates recognising a false EOF
        self._closed = False

    def push_eof_matcher(self, pred):
        self._eofstack.append(pred)

    def pop_eof_matcher(self):
        return self._eofstack.pop()

    def close(self):
        rest = EMPTYSTRING.join(self._partial)
        self._partial = []
        if rest:
            self.pushlines(self._split(rest, final=True))
        self._closed = True

    def readline(self):
        if not self._lines:
            if self._closed:
                return ''
            return NeedMoreData
        line = self._lines.popleft()
        # Outer boundaries are recognised at any nesting depth; check the
        # innermost first.
        for matcher in reversed(self._eofstack):
            if matcher(line):
                self._lines.appendleft(line)
                return ''
        return line

    def unreadline(self, line):
        assert line is not NeedMoreData
        self._lines.appendleft(line)

    @staticmethod
    def _split(text, final):
        pieces = NLCRE_crack.split(text)
        # pieces alternate: text, line ending, text, ...
        lines = [pieces[i] + pieces[i + 1] for i in range(0, len(pieces) - 1, 2)]
        rest = pieces[-1]
        if final:
            if rest:
                lines.append(rest)
            return lines
        # A line ending in a lone '\r' may be the first half of '\r\n'.
        if not rest and lines and lines[-1].endswith('\r'):
            rest = lines.pop()
        return lines, rest

    def push(self, data):
        """Add a chunk of text."""
        if '\n' not in data and '\r' not in data:
            self._partial.append(data)
            return
        self._partial.append(data)
        text = EMPTYSTRING.join(self._partial)
        lines, rest = self._split(text, final=False)
        self._partial = [rest] if rest else []
        self.pushlines(lines)

    def pushlines(self, lines):
        self._lines.extend(lines)

    def __iter__(self):
        return self

    def __next__(self):
        line = self.readline()
        if line == '':
            raise StopIteration
        return line


class FeedParser:
    """Incremental message parser."""

    def __init__(self, _factory=None, *, policy=compat32):
        """*_factory* is called (with policy=..., if it accepts it) to make
        each new message object; the default is the policy's
        message_factory, or email.message.Message."""
        self.policy = policy
        self._old_style_factory = False
        if _factory is None:
            if policy.message_factory is None:
                from email.message import Message
                self._factory = Message
            else:
                self._factory = policy.message_factory
        else:
            self._factory = _factory
            try:
                _factory(policy=self.policy)
            except TypeError:
                self._old_style_factory = True
        self._input = BufferedSubFile()
        self._msgstack = []
        self._parse = self._parsegen().__next__
        self._cur = None
        self._last = None
        self._headersonly = False

    def _set_headersonly(self):
        self._headersonly = True

    def feed(self, data):
        """Push more data into the parser."""
        self._input.push(data)
        self._call_parse()

    def _call_parse(self):
        try:
            self._parse()
        except StopIteration:
            pass

    def close(self):
        """Finish parsing and return the root message."""
        self._input.close()
        self._call_parse()
        root = self._pop_message()
        assert not self._msgstack
        if (root.get_content_maintype() == 'multipart'
                and not root.is_multipart() and not self._headersonly):
            self.policy.handle_defect(
                root, errors.MultipartInvariantViolationDefect())
        return root

    # -- message stack ---------------------------------------------------------

    def _new_message(self):
        if self._old_style_factory:
            msg = self._factory()
        else:
            msg = self._factory(policy=self.policy)
        if self._cur and self._cur.get_content_type() == 'multipart/digest':
            msg.set_default_type('message/rfc822')
        if self._msgstack:
            self._msgstack[-1].attach(msg)
        self._msgstack.append(msg)
        self._cur = msg
        self._last = msg

    def _pop_message(self):
        msg = self._msgstack.pop()
        self._cur = self._msgstack[-1] if self._msgstack else None
        return msg

    # -- the parser proper -------------------------------------------------------
    #
    # _parsegen() and its helpers are generators that yield NeedMoreData
    # whenever the input runs dry; feed() resumes them.

    def _lines_until_eof(self):
        # Yields NeedMoreData while waiting; the collected lines are the
        # return value of the generator.
        lines = []
        for line in self._input:
            if line is NeedMoreData:
                yield NeedMoreData
                continue
            lines.append(line)
        return lines

    def _next_line(self):
        while True:
            line = self._input.readline()
            if line is NeedMoreData:
                yield NeedMoreData
                continue
            return line

    def _parse_subpart(self):
        # Parse one nested message completely (until its EOF).
        for item in self._parsegen():
            if item is NeedMoreData:
                yield NeedMoreData
                continue
            break

    def _parsegen(self):
        self._new_message()
        header_lines = []
        while True:
            line = yield from self._next_line()
            if line == '':
                break
            if not headerRE.match(line):
                if not NLCRE.match(line):
                    self.policy.handle_defect(
                        self._cur, errors.MissingHeaderBodySeparatorDefect())
                    self._input.unreadline(line)
                break
            header_lines.append(line)
        self._parse_headers(header_lines)

        if self._headersonly:
            lines = yield from self._lines_until_eof()
            self._cur.set_payload(EMPTYSTRING.join(lines))
            return
        ctype = self._cur.get_content_type()
        if ctype == 'message/delivery-status':
            yield from self._parse_delivery_status()
            return
        maintype = self._cur.get_content_maintype()
        if maintype == 'message':
            yield from self._parse_subpart()
            self._pop_message()
            return
        if maintype == 'multipart':
            yield from self._parse_multipart()
            return
        lines = yield from self._lines_until_eof()
        self._cur.set_payload(EMPTYSTRING.join(lines))

    def _parse_delivery_status(self):
        # The body is a sequence of header blocks separated by blank lines;
        # each block becomes a subpart.
        while True:
            self._input.push_eof_matcher(NLCRE.match)
            yield from self._parse_subpart()
            self._pop_message()
            self._input.pop_eof_matcher()
            # consume the separating blank line, then peek at the next one
            yield from self._next_line()
            line = yield from self._next_line()
            if line == '':
                break
            self._input.unreadline(line)

    def _parse_multipart(self):
        cur = self._cur
        boundary = cur.get_boundary()
        if boundary is None:
            self.policy.handle_defect(cur, errors.NoBoundaryInMultipartDefect())
            lines = yield from self._lines_until_eof()
            cur.set_payload(EMPTYSTRING.join(lines))
            return
        cte = str(cur.get('content-transfer-encoding', '8bit')).lower()
        if cte not in ('7bit', '8bit', 'binary'):
            self.policy.handle_defect(
                cur, errors.InvalidMultipartContentTransferEncodingDefect())

        marker = '--' + boundary

        def is_boundary(line):
            if not line.startswith(marker):
                return None
            return boundaryendRE.match(line, len(marker))

        in_preamble = True
        preamble = []
        closed = False
        close_linesep = False
        while True:
            line = yield from self._next_line()
            if line == '':
                break
            m = is_boundary(line)
            if not m:
                # only the preamble can contain non-boundary lines here
                preamble.append(line)
                continue
            if m.group('end'):
                closed = True
                close_linesep = m.group('linesep')
                break
            if in_preamble:
                if preamble:
                    # the line break before a boundary belongs to it
                    last = preamble[-1]
                    eol = NLCRE_eol.search(last)
                    if eol:
                        preamble[-1] = last[:-len(eol.group(0))]
                    cur.preamble = EMPTYSTRING.join(preamble)
                in_preamble = False
                self._input.unreadline(line)
                continue
            # Skip repeated boundary lines.
            while True:
                line = yield from self._next_line()
                if not is_boundary(line):
                    self._input.unreadline(line)
                    break
            self._input.push_eof_matcher(is_boundary)
            yield from self._parse_subpart()
            self._strip_last_newline(self._last)
            self._input.pop_eof_matcher()
            self._pop_message()
            self._last = self._cur

        if in_preamble:
            self.policy.handle_defect(cur, errors.StartBoundaryNotFoundDefect())
            cur.set_payload(EMPTYSTRING.join(preamble))
            yield from self._lines_until_eof()
            cur.epilogue = EMPTYSTRING
            return
        if not closed:
            self.policy.handle_defect(cur, errors.CloseBoundaryNotFoundDefect())
            return
        epilogue = [''] if close_linesep else []
        epilogue += yield from self._lines_until_eof()
        if epilogue:
            first = epilogue[0]
            m = NLCRE_bol.match(first)
            if m:
                epilogue[0] = first[len(m.group(0)):]
        cur.epilogue = EMPTYSTRING.join(epilogue)

    @staticmethod
    def _strip_last_newline(part):
        # The line break before a boundary belongs to the boundary, not to
        # the payload (or epilogue) of the part before it.
        if part.get_content_maintype() == 'multipart':
            epilogue = part.epilogue
            if epilogue == '':
                part.epilogue = None
            elif epilogue is not None:
                m = NLCRE_eol.search(epilogue)
                if m:
                    part.epilogue = epilogue[:-len(m.group(0))]
        else:
            payload = part._payload
            if isinstance(payload, str):
                m = NLCRE_eol.search(payload)
                if m:
                    part._payload = payload[:-len(m.group(0))]

    def _parse_headers(self, lines):
        name = ''
        value_lines = []

        def flush():
            if name:
                self._cur.set_raw(*self.policy.header_source_parse(value_lines))

        for index, line in enumerate(lines):
            if line[0] in ' \t':
                if not name:
                    self.policy.handle_defect(
                        self._cur, errors.FirstHeaderLineIsContinuationDefect(line))
                    continue
                value_lines.append(line)
                continue
            flush()
            name, value_lines = '', []
            if line.startswith('From '):
                if index == 0:
                    eol = NLCRE_eol.search(line)
                    if eol:
                        line = line[:-len(eol.group(0))]
                    self._cur.set_unixfrom(line)
                    continue
                if index == len(lines) - 1:
                    # probably the first line of the body
                    self._input.unreadline(line)
                    return
                self.policy.handle_defect(
                    self._cur, errors.MisplacedEnvelopeHeaderDefect(line))
                continue
            colon = line.find(':')
            if colon == 0:
                self.policy.handle_defect(
                    self._cur, errors.InvalidHeaderDefect("Missing header name."))
                continue
            assert colon > 0, "_parse_headers fed line with no : and no leading WS"
            name = line[:colon]
            value_lines = [line]
        flush()


class BytesFeedParser(FeedParser):
    """Like FeedParser, but feed() takes bytes."""

    def feed(self, data):
        super().feed(data.decode('ascii', 'surrogateescape'))
