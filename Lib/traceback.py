"""Extract, format and print stack traces and exceptions.

The functions mirror what the interpreter prints for an uncaught
exception: tracebacks with source lines and (where column information is
available) markers under the failing expression, chained exceptions,
exception groups, notes and "Did you mean ...?" suggestions.
"""

import collections.abc
import itertools
import linecache
import sys
import textwrap
from contextlib import suppress

__all__ = ['extract_stack', 'extract_tb', 'format_exception',
           'format_exception_only', 'format_list', 'format_stack',
           'format_tb', 'print_exc', 'format_exc', 'print_exception',
           'print_last', 'print_stack', 'print_tb', 'clear_frames',
           'FrameSummary', 'StackSummary', 'TracebackException',
           'walk_stack', 'walk_tb', 'print_list']


def _theme(colorize):
    # _colorize is only imported when something is actually formatted
    import _colorize
    if colorize:
        return _colorize.get_theme(force_color=True).traceback
    return _colorize.get_theme(force_no_color=True).traceback


# ---------------------------------------------------------------------------
# Formatting and printing lists of frames
# ---------------------------------------------------------------------------

def print_list(extracted_list, file=None):
    """Print the list of (filename, lineno, name, line) tuples or
    FrameSummary objects, as returned by extract_tb()/extract_stack()."""
    if file is None:
        file = sys.stderr
    for item in StackSummary.from_list(extracted_list).format():
        print(item, file=file, end='')


def format_list(extracted_list):
    """Format a list of frames (as from extract_tb()/extract_stack()) into
    a list of strings ready for printing, one per frame (with its source
    line, if known)."""
    return StackSummary.from_list(extracted_list).format()


# ---------------------------------------------------------------------------
# Tracebacks
# ---------------------------------------------------------------------------

def print_tb(tb, limit=None, file=None):
    """Print up to *limit* entries of traceback *tb* (all if None; the
    last abs(limit) if negative) to *file* (default sys.stderr)."""
    print_list(extract_tb(tb, limit=limit), file=file)


def format_tb(tb, limit=None):
    """A shorthand for 'format_list(extract_tb(tb, limit))'."""
    return extract_tb(tb, limit=limit).format()


def extract_tb(tb, limit=None):
    """Return a StackSummary of the frames of traceback *tb* (outermost
    first), limited like print_tb()."""
    return StackSummary._extract_from_extended_frame_gen(
        _walk_tb_with_full_positions(tb), limit=limit)


# ---------------------------------------------------------------------------
# Exceptions
# ---------------------------------------------------------------------------

_cause_message = (
    '\nThe above exception was the direct cause '
    'of the following exception:\n\n')

_context_message = (
    '\nDuring handling of the above exception, '
    'another exception occurred:\n\n')


class _Sentinel:
    def __repr__(self):
        return '<implicit>'


_sentinel = _Sentinel()


def _parse_value_tb(exc, value, tb):
    if (value is _sentinel) != (tb is _sentinel):
        raise ValueError('Both or neither of value and tb must be given')
    if value is tb is _sentinel:
        if exc is None:
            return None, None
        if isinstance(exc, BaseException):
            return exc, exc.__traceback__
        raise TypeError(f'Exception expected for value, '
                        f'{type(exc).__qualname__} found')
    return value, tb


def print_exception(exc, /, value=_sentinel, tb=_sentinel, limit=None,
                    file=None, chain=True, **kwargs):
    """Print an exception (with traceback, chained exceptions and notes)
    to *file* (default sys.stderr).

    The exception may be given alone or, in the legacy form, as
    (type, value, traceback).  *limit* restricts the traceback entries as
    for print_tb(); with *chain* false, __cause__/__context__ are ignored.
    """
    colorize = kwargs.get('colorize', False)
    value, tb = _parse_value_tb(exc, value, tb)
    te = TracebackException(type(value), value, tb, limit=limit,
                            compact=True)
    te.print(file=file, chain=chain, colorize=colorize)


# Marks the limit used when the interpreter itself prints an exception.
BUILTIN_EXCEPTION_LIMIT = object()


def _print_exception_bltin(exc, file=None, /):
    if file is None:
        file = sys.stderr if sys.stderr is not None else sys.__stderr__
    import _colorize
    colorize = _colorize.can_colorize(file=file)
    return print_exception(exc, limit=BUILTIN_EXCEPTION_LIMIT, file=file,
                           colorize=colorize)


def format_exception(exc, /, value=_sentinel, tb=_sentinel, limit=None,
                     chain=True, **kwargs):
    """Like print_exception(), but return a list of strings (each ending
    in a newline; some contain internal newlines) instead of printing."""
    colorize = kwargs.get('colorize', False)
    value, tb = _parse_value_tb(exc, value, tb)
    te = TracebackException(type(value), value, tb, limit=limit,
                            compact=True)
    return list(te.format(chain=chain, colorize=colorize))


def format_exception_only(exc, /, value=_sentinel, *, show_group=False,
                          **kwargs):
    """Format the exception part of a traceback: usually a single line
    "ExceptionType: message", more for SyntaxErrors and notes.  With
    *show_group*, the nested exceptions of a group are listed too."""
    colorize = kwargs.get('colorize', False)
    if value is _sentinel:
        value = exc
    te = TracebackException(type(value), value, None, compact=True)
    return list(te.format_exception_only(show_group=show_group,
                                         colorize=colorize))


def _format_final_exc_line(etype, value, *, insert_final_newline=True,
                           colorize=False):
    valuestr = _safe_string(value, 'exception')
    end = '\n' if insert_final_newline else ''
    theme = _theme(colorize)
    if value is None or not valuestr:
        return f'{theme.type}{etype}{theme.reset}{end}'
    return (f'{theme.type}{etype}{theme.reset}: '
            f'{theme.message}{valuestr}{theme.reset}{end}')


def _safe_string(value, what, func=str):
    try:
        return func(value)
    except:
        return f'<{what} {func.__name__}() failed>'


# -- the current exception -----------------------------------------------------

def print_exc(limit=None, file=None, chain=True):
    """Shorthand for print_exception(sys.exception(), ...)."""
    print_exception(sys.exception(), limit=limit, file=file, chain=chain)


def format_exc(limit=None, chain=True):
    """Like print_exc() but return a string."""
    return ''.join(format_exception(sys.exception(), limit=limit,
                                    chain=chain))


def print_last(limit=None, file=None, chain=True):
    """Print the last uncaught exception (sys.last_exc, or the legacy
    sys.last_type/last_value/last_traceback)."""
    if not hasattr(sys, 'last_exc') and not hasattr(sys, 'last_type'):
        raise ValueError('no last exception')
    if hasattr(sys, 'last_exc'):
        print_exception(sys.last_exc, limit=limit, file=file, chain=chain)
    else:
        print_exception(sys.last_type, sys.last_value, sys.last_traceback,
                        limit=limit, file=file, chain=chain)


# ---------------------------------------------------------------------------
# Stacks
# ---------------------------------------------------------------------------

def print_stack(f=None, limit=None, file=None):
    """Print a stack trace from frame *f* (default: the caller's frame)."""
    if f is None:
        f = sys._getframe().f_back
    print_list(extract_stack(f, limit=limit), file=file)


def format_stack(f=None, limit=None):
    """A shorthand for 'format_list(extract_stack(f, limit))'."""
    if f is None:
        f = sys._getframe().f_back
    return format_list(extract_stack(f, limit=limit))


def extract_stack(f=None, limit=None):
    """Return a StackSummary of the stack ending at frame *f* (default:
    the caller's frame), oldest frame first."""
    if f is None:
        f = sys._getframe().f_back
    stack = StackSummary.extract(walk_stack(f), limit=limit)
    stack.reverse()
    return stack


def clear_frames(tb):
    """Clear the local variables of all the frames of a traceback."""
    while tb is not None:
        try:
            tb.tb_frame.clear()
        except RuntimeError:
            # the frame is still executing
            pass
        tb = tb.tb_next


def walk_stack(f):
    """Yield (frame, lineno) going outwards from frame *f* (default: the
    caller's frame)."""
    if f is None:
        # resolved now, not when the generator first runs
        f = sys._getframe().f_back
    return _frames_outwards(f)


def _frames_outwards(frame):
    while frame is not None:
        yield frame, frame.f_lineno
        frame = frame.f_back


def walk_tb(tb):
    """Yield (frame, lineno) for each entry of a traceback, following
    tb_next."""
    while tb is not None:
        yield tb.tb_frame, tb.tb_lineno
        tb = tb.tb_next


def _walk_tb_with_full_positions(tb):
    # (frame, (lineno, end_lineno, colno, end_colno)) per entry
    while tb is not None:
        positions = _get_code_position(tb.tb_frame.f_code, tb.tb_lasti)
        if positions[0] is None:
            yield tb.tb_frame, (tb.tb_lineno,) + positions[1:]
        else:
            yield tb.tb_frame, positions
        tb = tb.tb_next


def _get_code_position(code, instruction_index):
    if instruction_index < 0:
        return (None, None, None, None)
    # one co_positions() entry per 2-byte code unit
    positions = code.co_positions()
    return next(itertools.islice(positions, instruction_index // 2, None))


# Identical consecutive frames beyond this many are summarised.
_RECURSIVE_CUTOFF = 3


# ---------------------------------------------------------------------------
# FrameSummary
# ---------------------------------------------------------------------------

class FrameSummary:
    """Information about a single frame of a traceback or stack.

    Attributes: filename, lineno, end_lineno, colno, end_colno, name,
    line (the first source line, stripped; loaded lazily) and locals (a
    dict of reprs, or None).  For compatibility a FrameSummary also
    behaves like the 4-tuple (filename, lineno, name, line).
    """

    __slots__ = ('filename', 'lineno', 'end_lineno', 'colno', 'end_colno',
                 'name', '_lines', '_lines_dedented', 'locals', '_code')

    def __init__(self, filename, lineno, name, *, lookup_line=True,
                 locals=None, line=None, end_lineno=None, colno=None,
                 end_colno=None, **kwargs):
        """*lookup_line* loads the source line now (not later); *locals*
        is a mapping whose values are stored as their repr(); *line* gives
        the source line directly."""
        self.filename = filename
        self.lineno = lineno
        self.end_lineno = lineno if end_lineno is None else end_lineno
        self.colno = colno
        self.end_colno = end_colno
        self.name = name
        self._code = kwargs.get('_code')
        self._lines = line
        self._lines_dedented = None
        if lookup_line:
            self.line
        if locals:
            self.locals = {key: _safe_string(value, 'local', func=repr)
                           for key, value in locals.items()}
        else:
            self.locals = None

    def __eq__(self, other):
        if isinstance(other, FrameSummary):
            return (self.filename == other.filename and
                    self.lineno == other.lineno and
                    self.name == other.name and
                    self.locals == other.locals)
        if isinstance(other, tuple):
            return (self.filename, self.lineno, self.name, self.line) == other
        return NotImplemented

    def __getitem__(self, pos):
        return (self.filename, self.lineno, self.name, self.line)[pos]

    def __iter__(self):
        return iter([self.filename, self.lineno, self.name, self.line])

    def __repr__(self):
        return '<FrameSummary file {filename}, line {lineno} in {name}>'.format(
            filename=self.filename, lineno=self.lineno, name=self.name)

    def __len__(self):
        return 4

    def _set_lines(self):
        if self._lines or self.lineno is None or self.end_lineno is None:
            return
        collected = []
        for lineno in range(self.lineno, self.end_lineno + 1):
            # a missing line and an empty line both come back empty
            text = linecache.getline(self.filename, lineno).rstrip()
            if (not text and self._code is not None
                    and self.filename.startswith('<')):
                text = linecache._getline_from_code(self._code,
                                                    lineno).rstrip()
            collected.append(text)
        self._lines = '\n'.join(collected) + '\n'

    @property
    def _original_lines(self):
        # the source lines exactly as in the file
        self._set_lines()
        return self._lines

    @property
    def _dedented_lines(self):
        self._set_lines()
        if self._lines_dedented is None and self._lines is not None:
            self._lines_dedented = textwrap.dedent(self._lines)
        return self._lines_dedented

    @property
    def line(self):
        self._set_lines()
        if self._lines is None:
            return None
        return self._lines.partition('\n')[0].strip()


# ---------------------------------------------------------------------------
# Markers under the failing expression
# ---------------------------------------------------------------------------

def _byte_offset_to_character_offset(text, offset):
    encoded = text.encode('utf-8')
    return len(encoded[:offset].decode('utf-8', errors='replace'))


_Anchors = collections.namedtuple(
    '_Anchors',
    ['left_end_lineno', 'left_end_offset',
     'right_start_lineno', 'right_start_offset',
     'primary_char', 'secondary_char'],
    defaults=['~', '^'])


class _SegmentCursor:
    """Walks over the characters of a (multi-line) source segment, skipping
    line continuations, comments and line ends."""

    def __init__(self, lines):
        self.lines = lines

    def settle(self, lineno, col):
        # move past line ends (and empty lines) to a real character
        while lineno < len(self.lines) and col >= len(self.lines[lineno]):
            lineno += 1
            col = 0
        if lineno >= len(self.lines):
            raise IndexError('ran past the end of the segment')
        return lineno, col

    def step(self, lineno, col):
        return self.settle(lineno, col + 1)

    def next_line(self, lineno, col):
        return self.settle(lineno + 1, 0)

    def scan(self, lineno, col, accept):
        """First position at or after (lineno, col) holding a character
        accepted by *accept*, ignoring backslashes and comments."""
        while True:
            ch = self.lines[lineno][col]
            if ch in '\\#':
                lineno, col = self.next_line(lineno, col)
            elif accept(ch):
                return lineno, col
            else:
                lineno, col = self.step(lineno, col)


def _extract_caret_anchors_from_line_segment(segment):
    """Where to draw the secondary markers inside *segment* (the source of
    the failing instruction): the operator of a binary operation, or the
    brackets of a subscript or call.  None if not applicable."""
    import ast

    try:
        # parenthesised so that multi-line expressions parse
        tree = ast.parse(f'(\n{segment}\n)')
    except SyntaxError:
        return None
    if len(tree.body) != 1:
        return None
    statement = tree.body[0]
    if not isinstance(statement, ast.Expr):
        return None
    expr = statement.value

    lines = segment.splitlines()
    cursor = _SegmentCursor(lines)

    def char_col(lineno, byte_offset):
        return _byte_offset_to_character_offset(lines[lineno], byte_offset)

    def end_of(node, settle=True):
        # the parse added one line before the segment: subtract 2 from the
        # 1-based line numbers
        lineno = node.end_lineno - 2
        col = char_col(lineno, node.end_col_offset)
        return cursor.settle(lineno, col) if settle else (lineno, col)

    if isinstance(expr, ast.BinOp):
        lineno, col = end_of(expr.left)
        lineno, col = cursor.scan(lineno, col,
                                  lambda ch: not ch.isspace() and ch != ')')
        # the operator has one or two characters
        stop = col + 1
        text = lines[lineno]
        right_line = expr.right.lineno - 2
        if stop < len(text):
            before_right = (right_line > lineno or
                            stop < char_col(right_line,
                                            expr.right.col_offset))
            if (before_right and not text[stop].isspace()
                    and text[stop] not in '\\#'):
                stop += 1
        return _Anchors(lineno, col, lineno, stop)

    if isinstance(expr, ast.Subscript):
        left = cursor.scan(*end_of(expr.value), lambda ch: ch == '[')
        right = end_of(expr, settle=False)
        return _Anchors(left[0], left[1], right[0], right[1])

    if isinstance(expr, ast.Call):
        left = cursor.scan(*end_of(expr.func), lambda ch: ch == '(')
        right = end_of(expr, settle=False)
        return _Anchors(left[0], left[1], right[0], right[1])

    return None


def _display_width(line, offset=None):
    """Terminal columns taken by line[:offset]: East Asian wide characters
    take two, control characters (shown as ^X) take two and ANSI escape
    sequences take none."""
    text = line if offset is None else line[:offset]
    if text.isascii() and text.isprintable():
        return len(text)
    return sum(width for _, width in _char_widths(text))


def _char_widths(text):
    """Yield (character, display width) for each character of *text*."""
    if text.isascii() and text.isprintable():
        for ch in text:
            yield ch, 1
        return
    import unicodedata
    n = len(text)
    i = 0
    while i < n:
        ch = text[i]
        if ch == '\x1b' and i + 1 < n and text[i + 1] == '[':
            # a CSI escape sequence occupies no columns
            j = i + 2
            while j < n and not ('@' <= text[j] <= '~'):
                j += 1
            for k in range(i, min(j + 1, n)):
                yield text[k], 0
            i = j + 1
            continue
        code = ord(ch)
        if code < 32 or code == 127:
            # shown as ^X
            yield ch, 2
        elif unicodedata.combining(ch):
            yield ch, 0
        elif unicodedata.east_asian_width(ch) in ('W', 'F'):
            yield ch, 2
        else:
            yield ch, 1
        i += 1


def _color_runs(text, kinds, colors, reset):
    """Wrap the runs of *text* whose kind (per character) has a colour."""
    pieces = []
    current = None
    for ch, kind in zip(text, kinds):
        color = colors.get(kind)
        if color != current:
            if current:
                pieces.append(reset)
            if color:
                pieces.append(color)
            current = color
        pieces.append(ch)
    if current:
        pieces.append(reset)
    pieces.append(text[len(kinds):])
    return ''.join(pieces)


class _MarkedSource:
    """The source lines of a frame with the marker lines under them."""

    def __init__(self, frame_summary):
        fs = frame_summary
        original = fs._original_lines.splitlines()
        count = fs.end_lineno - fs.lineno + 1
        self.lines = fs._dedented_lines.splitlines()[:count]
        first, last = original[0], original[count - 1]
        start = _byte_offset_to_character_offset(first, fs.colno)
        end = _byte_offset_to_character_offset(last, fs.end_colno)
        shift = len(first) - len(self.lines[0])
        self.start = max(0, start - shift)
        self.end = max(0, end - shift)

        joined = '\n'.join(self.lines)
        segment = joined[self.start:len(joined) - (len(self.lines[-1])
                                                   - self.end)]
        self.anchors = None
        with suppress(Exception):
            self.anchors = _extract_caret_anchors_from_line_segment(segment)

    def marker_kinds(self, index, show):
        """Marker character for every display column of line *index*:
        ' ' (none), primary or secondary character."""
        line = self.lines[index]
        last = len(self.lines) - 1
        anchors = self.anchors
        if index == last:
            columns = _display_width(line, self.end)
        else:
            columns = _display_width(line)
        lead = len(line) - len(line.lstrip())
        start_col = _display_width(self.lines[0], self.start)
        primary = secondary = '^'
        if anchors:
            primary, secondary = anchors.primary_char, anchors.secondary_char
            left_line, right_line = anchors.left_end_lineno, anchors.right_start_lineno
            left_off = anchors.left_end_offset
            right_off = anchors.right_start_offset
            # anchor offsets on the first line are relative to the segment
            if left_line == 0:
                left_off += self.start
            if right_line == 0:
                right_off += self.start
            left_col = _display_width(self.lines[left_line], left_off)
            right_col = _display_width(self.lines[right_line], right_off)
        kinds = []
        for col in range(columns):
            if col < lead or (index == 0 and col < start_col):
                kinds.append(' ')
                continue
            if anchors:
                after_left = (index > left_line or
                              (index == left_line and col >= left_col))
                before_right = (index < right_line or
                                (index == right_line and col < right_col))
                if after_left and before_right:
                    kinds.append(secondary)
                    continue
            kinds.append(primary)
        return kinds

    def significant(self):
        wanted = {0, len(self.lines) - 1}
        if self.anchors:
            for lineno in (self.anchors.left_end_lineno,
                           self.anchors.right_start_lineno):
                wanted.update(range(lineno - 1, lineno + 2))
        return sorted(n for n in wanted if 0 <= n < len(self.lines))

    def render(self, summary, colorize, theme):
        show = summary._should_show_carets(self.start, self.end, self.lines,
                                           self.anchors)
        output = []

        def emit(index):
            line = self.lines[index]
            if not show:
                output.append(line + '\n')
                return
            kinds = self.marker_kinds(index, show)
            markers = ''.join(kinds)
            if colorize:
                colors = self._colors(theme)
                char_kinds = self._char_kinds(line, kinds)
                line = _color_runs(line, char_kinds, colors, theme.reset)
                markers = _color_runs(markers, kinds, colors, theme.reset)
            output.append(line + '\n')
            output.append(markers + '\n')

        chosen = self.significant()
        previous = None
        for index in chosen:
            if previous is not None:
                gap = index - previous
                if gap == 2:
                    emit(index - 1)
                elif gap > 2:
                    output.append(f'...<{gap - 1} lines>...\n')
            emit(index)
            previous = index
        return textwrap.indent(textwrap.dedent(''.join(output)), '    ',
                               lambda line: True)

    def _colors(self, theme):
        if self.anchors:
            return {self.anchors.primary_char: theme.error_range,
                    self.anchors.secondary_char: theme.error_highlight}
        return {'^': theme.error_highlight}

    @staticmethod
    def _char_kinds(line, column_kinds):
        # map display columns back to characters
        kinds = []
        col = 0
        for ch, width in _char_widths(line):
            if width == 0 and kinds:
                # combining marks etc. belong to the preceding character
                kinds.append(kinds[-1])
                continue
            if col >= len(column_kinds):
                break
            kinds.append(column_kinds[col])
            col += width
        return kinds


# ---------------------------------------------------------------------------
# StackSummary
# ---------------------------------------------------------------------------

class StackSummary(list):
    """The frames of a traceback or stack (a list of FrameSummary)."""

    @classmethod
    def extract(klass, frame_gen, *, limit=None, lookup_lines=True,
                capture_locals=False):
        """Create a StackSummary from an iterable of (frame, lineno) pairs
        (as from walk_stack() or walk_tb()).

        *limit* keeps the first entries (or the last abs(limit) if
        negative); *lookup_lines* loads source lines now; *capture_locals*
        records the frames' local variables."""
        def with_positions():
            for f, lineno in frame_gen:
                yield f, (lineno, None, None, None)

        return klass._extract_from_extended_frame_gen(
            with_positions(), limit=limit, lookup_lines=lookup_lines,
            capture_locals=capture_locals)

    @classmethod
    def _extract_from_extended_frame_gen(klass, frame_gen, *, limit=None,
                                         lookup_lines=True,
                                         capture_locals=False):
        if limit is None or limit is BUILTIN_EXCEPTION_LIMIT:
            limit = getattr(sys, 'tracebacklimit', None)
            if limit is not None and limit < 0:
                limit = 0
        if limit is not None:
            if limit >= 0:
                frame_gen = itertools.islice(frame_gen, limit)
            else:
                frame_gen = collections.deque(frame_gen, maxlen=-limit)

        result = klass()
        filenames = set()
        for f, (lineno, end_lineno, colno, end_colno) in frame_gen:
            code = f.f_code
            filename = code.co_filename
            filenames.add(filename)
            linecache.lazycache(filename, f.f_globals)
            result.append(FrameSummary(
                filename, lineno, code.co_name, lookup_line=False,
                locals=f.f_locals if capture_locals else None,
                end_lineno=end_lineno, colno=colno, end_colno=end_colno,
                _code=code))
        # only now that all lazy entries exist: refresh stale cache entries
        for filename in filenames:
            linecache.checkcache(filename)
        if lookup_lines:
            for fs in result:
                fs.line
        return result

    @classmethod
    def from_list(klass, a_list):
        """Create a StackSummary from FrameSummary objects or old-style
        (filename, lineno, name, line) tuples."""
        result = StackSummary()
        for frame in a_list:
            if isinstance(frame, FrameSummary):
                result.append(frame)
            else:
                filename, lineno, name, line = frame
                result.append(FrameSummary(filename, lineno, name, line=line))
        return result

    def format_frame_summary(self, frame_summary, **kwargs):
        """Format one frame: the 'File ..., line ..., in ...' header, its
        source line(s) with markers, and its locals.  Subclasses may
        override this; returning None omits the frame."""
        colorize = kwargs.get('colorize', False)
        theme = _theme(colorize)
        fs = frame_summary
        filename = fs.filename
        if filename.startswith('<stdin>-'):
            filename = '<stdin>'
        pieces = ['  File {}"{}"{}, line {}{}{}, in {}{}{}\n'.format(
            theme.filename, filename, theme.reset,
            theme.line_no, fs.lineno, theme.reset,
            theme.frame, fs.name, theme.reset)]

        dedented = fs._dedented_lines
        if dedented and dedented.strip():
            if fs.colno is None or fs.end_colno is None:
                # no column information: just the first line
                pieces.append(textwrap.indent(fs.line, '    ') + '\n')
            else:
                pieces.append(_MarkedSource(fs).render(self, colorize, theme))

        if fs.locals:
            for name, value in sorted(fs.locals.items()):
                pieces.append(f'    {name} = {value}\n')
        return ''.join(pieces)

    def _should_show_carets(self, start_offset, end_offset, all_lines,
                            anchors):
        """Whether markers add information for this source excerpt: not
        when the failing expression is the whole line, e.g. a plain
        "return f(...)" or "x = f(...)"."""
        with suppress(SyntaxError, ImportError):
            import ast
            tree = ast.parse('\n'.join(all_lines))
            if not tree.body:
                return False
            statement = tree.body[0]
            call = None
            if (isinstance(statement, ast.Return)
                    and isinstance(statement.value, ast.Call)
                    and isinstance(statement.value.func, ast.Name)):
                call = statement.value
            elif (isinstance(statement, ast.Assign)
                    and isinstance(statement.value, ast.Call)
                    and len(statement.targets) == 1
                    and isinstance(statement.targets[0], ast.Name)):
                call = statement.value
            if (call is not None and call.lineno == 1
                    and call.end_lineno == len(all_lines)
                    and call.col_offset == start_offset
                    and call.end_col_offset == end_offset):
                return False
        if anchors:
            return True
        return bool(all_lines[0][:start_offset].lstrip()
                    or all_lines[-1][end_offset:].rstrip())

    def format(self, **kwargs):
        """Format the frames as a list of strings, one per frame (each
        possibly containing several lines).

        Runs of more than _RECURSIVE_CUTOFF identical consecutive frames
        (deep recursion) are summarised by a "[Previous line repeated N
        more times]" line."""
        colorize = kwargs.get('colorize', False)
        result = []
        previous = None
        repeats = 0

        def summary(count):
            plural = 's' if count > 1 else ''
            return (f'  [Previous line repeated {count} more '
                    f'time{plural}]\n')

        for fs in self:
            text = self.format_frame_summary(fs, colorize=colorize)
            if text is None:
                continue
            key = (fs.filename, fs.lineno, fs.name)
            if previous is None or key != previous or None in key:
                if repeats > _RECURSIVE_CUTOFF:
                    result.append(summary(repeats - _RECURSIVE_CUTOFF))
                previous = key
                repeats = 0
            repeats += 1
            if repeats > _RECURSIVE_CUTOFF:
                continue
            result.append(text)
        if repeats > _RECURSIVE_CUTOFF:
            result.append(summary(repeats - _RECURSIVE_CUTOFF))
        return result


# ---------------------------------------------------------------------------
# Exception groups: indentation and margins
# ---------------------------------------------------------------------------

class _ExceptionPrintContext:
    """State shared while formatting nested exception groups."""

    def __init__(self):
        self.seen = set()
        self.exception_group_depth = 0
        self.need_close = False

    def indent(self):
        return ' ' * (2 * self.exception_group_depth)

    def emit(self, text_gen, margin_char=None):
        if margin_char is None:
            margin_char = '|'
        prefix = self.indent()
        if self.exception_group_depth:
            prefix += margin_char + ' '
        if isinstance(text_gen, str):
            text_gen = [text_gen]
        for text in text_gen:
            yield textwrap.indent(text, prefix, lambda line: True)


# ---------------------------------------------------------------------------
# TracebackException
# ---------------------------------------------------------------------------

class TracebackException:
    """An exception ready for rendering, captured so that the exception
    and its frames need not be kept alive.

    Attributes: __cause__, __context__ (TracebackExceptions or None),
    __suppress_context__, __notes__, exceptions (for exception groups),
    stack (a StackSummary), exc_type_str, and for SyntaxErrors filename,
    lineno, end_lineno, text, offset, end_offset and msg.

    *limit*, *lookup_lines* and *capture_locals* are as for
    StackSummary.extract(); with *compact*, a context that will not be
    shown is not captured; *max_group_width* and *max_group_depth* bound
    how much of an exception group is shown.
    """

    def __init__(self, exc_type, exc_value, exc_traceback, *, limit=None,
                 lookup_lines=True, capture_locals=False, compact=False,
                 max_group_width=15, max_group_depth=10, save_exc_type=True,
                 _seen=None):
        nested = _seen is not None
        if _seen is None:
            _seen = set()
        _seen.add(id(exc_value))

        self.max_group_width = max_group_width
        self.max_group_depth = max_group_depth

        self.stack = StackSummary._extract_from_extended_frame_gen(
            _walk_tb_with_full_positions(exc_traceback), limit=limit,
            lookup_lines=lookup_lines, capture_locals=capture_locals)

        self._exc_type = exc_type if save_exc_type else None

        # the text is captured now so that the exception can go away
        self._str = _safe_string(exc_value, 'exception')
        try:
            self.__notes__ = getattr(exc_value, '__notes__', None)
        except Exception as exc:
            self.__notes__ = [
                f'Ignored error getting __notes__: '
                f'{_safe_string(exc, "__notes__", repr)}']

        self._is_syntax_error = False
        self._have_exc_type = exc_type is not None
        if exc_type is not None:
            self.exc_type_qualname = exc_type.__qualname__
            self.exc_type_module = exc_type.__module__
        else:
            self.exc_type_qualname = None
            self.exc_type_module = None

        if exc_type is not None and issubclass(exc_type, SyntaxError):
            self._capture_syntax_error(exc_value)
        elif exc_type is not None:
            extra = _extra_message(exc_type, exc_value, exc_traceback)
            if isinstance(extra, _ReplaceMessage):
                self._str = str(extra)
            else:
                self._str += extra

        if lookup_lines:
            self._load_lines()
        self.__suppress_context__ = (exc_value.__suppress_context__
                                     if exc_value is not None else False)

        if not nested:
            # Convert the chain (and group members) iteratively: chains
            # can be longer than the recursion limit.
            options = dict(limit=limit, lookup_lines=lookup_lines,
                           capture_locals=capture_locals,
                           max_group_width=max_group_width,
                           max_group_depth=max_group_depth)
            self._convert_related(exc_value, compact, options, _seen)

    def _capture_syntax_error(self, exc_value):
        self.filename = exc_value.filename
        lineno = exc_value.lineno
        self.lineno = str(lineno) if lineno is not None else None
        end_lineno = exc_value.end_lineno
        self.end_lineno = str(end_lineno) if end_lineno is not None else None
        self.text = exc_value.text
        self.offset = exc_value.offset
        self.end_offset = exc_value.end_offset
        self.msg = exc_value.msg
        self._is_syntax_error = True
        self._exc_metadata = getattr(exc_value, '_metadata', None)

    def _convert_related(self, exc_value, compact, options, seen):
        pending = [(self, exc_value)]
        while pending:
            te, exc = pending.pop()

            def wrap(related):
                return TracebackException(type(related), related,
                                          related.__traceback__,
                                          _seen=seen, **options)

            cause = None
            if (exc is not None and exc.__cause__ is not None
                    and id(exc.__cause__) not in seen):
                cause = wrap(exc.__cause__)

            if compact:
                # the context is only shown without cause and unless
                # suppressed
                want_context = (cause is None and exc is not None
                                and not exc.__suppress_context__)
            else:
                want_context = True
            context = None
            if (exc is not None and exc.__context__ is not None
                    and want_context and id(exc.__context__) not in seen):
                context = wrap(exc.__context__)

            members = None
            if exc is not None and isinstance(exc, BaseExceptionGroup):
                members = [wrap(member) for member in exc.exceptions]

            te.__cause__ = cause
            te.__context__ = context
            te.exceptions = members
            if cause is not None:
                pending.append((cause, exc.__cause__))
            if context is not None:
                pending.append((context, exc.__context__))
            if members:
                pending.extend(zip(members, exc.exceptions))

    @classmethod
    def from_exception(cls, exc, *args, **kwargs):
        """Capture *exc* (with its own traceback)."""
        return cls(type(exc), exc, exc.__traceback__, *args, **kwargs)

    @property
    def exc_type(self):
        import warnings
        warnings.warn('Deprecated in 3.13. Use exc_type_str instead.',
                      DeprecationWarning, stacklevel=2)
        return self._exc_type

    @property
    def exc_type_str(self):
        if not self._have_exc_type:
            return None
        name = self.exc_type_qualname
        module = self.exc_type_module
        if module not in ('__main__', 'builtins'):
            if not isinstance(module, str):
                module = '<unknown>'
            name = module + '.' + name
        return name

    def _load_lines(self):
        """Load the source lines of all frames now."""
        for frame in self.stack:
            frame.line

    def __eq__(self, other):
        if isinstance(other, TracebackException):
            return self.__dict__ == other.__dict__
        return NotImplemented

    def __str__(self):
        return self._str

    # -- the "Type: message" part ------------------------------------------------

    def format_exception_only(self, *, show_group=False, _depth=0,
                              **kwargs):
        """Format the exception part of the traceback.

        Yields "Type: message" (or "Type" when there is no message), the
        location details of a SyntaxError, and the notes; with
        *show_group* the members of an exception group follow, indented.
        """
        colorize = kwargs.get('colorize', False)
        theme = _theme(colorize)
        indent = 3 * _depth * ' '

        if not self._have_exc_type:
            yield indent + _format_final_exc_line(None, self._str,
                                                  colorize=colorize)
            return

        stype = self.exc_type_str
        if not self._is_syntax_error:
            if _depth > 0:
                # every line of a nested member's message is indented
                text = _format_final_exc_line(
                    stype, self._str, insert_final_newline=False,
                    colorize=colorize)
                for line in text.split('\n'):
                    yield indent + line + '\n'
            else:
                yield _format_final_exc_line(stype, self._str,
                                             colorize=colorize)
        else:
            yield from (textwrap.indent(line, indent) for line in
                        self._format_syntax_error(stype, colorize=colorize))

        notes = self.__notes__
        if notes is not None:
            if (not isinstance(notes, collections.abc.Sequence)
                    or isinstance(notes, (str, bytes))):
                notes = [_safe_string(notes, '__notes__', func=repr)]
            for note in notes:
                note = _safe_string(note, 'note')
                for line in note.split('\n'):
                    yield f'{indent}{theme.note}{line}{theme.reset}\n'

        if self.exceptions and show_group:
            for member in self.exceptions:
                yield from member.format_exception_only(
                    show_group=show_group, _depth=_depth + 1,
                    colorize=colorize)

    def _format_syntax_error(self, stype, **kwargs):
        """Location, source line with markers and message of a SyntaxError."""
        colorize = kwargs.get('colorize', False)
        theme = _theme(colorize)
        suffix = ''
        if self.lineno is not None:
            yield '  File {}"{}"{}, line {}{}{}\n'.format(
                theme.filename, self.filename or '<string>', theme.reset,
                theme.line_no, self.lineno, theme.reset)
        elif self.filename is not None:
            suffix = ' ({})'.format(self.filename)

        if isinstance(self.text, str):
            with suppress(Exception):
                self._find_keyword_typos()
            yield from self._syntax_error_source_lines(theme, colorize)

        msg = self.msg
        if msg is None or (isinstance(msg, str) and not msg):
            msg = '<no detail available>'
        yield '{}{}{}: {}{}{}{}\n'.format(theme.type, stype, theme.reset,
                                         theme.message, msg, theme.reset,
                                         suffix)

    def _syntax_error_source_lines(self, theme, colorize):
        text = self.text
        stripped_right = text.rstrip('\n')
        content = stripped_right.lstrip(' \n\f')
        leading = len(stripped_right) - len(content)
        offset = self.offset
        if offset is None or not isinstance(offset, int):
            if offset is None:
                yield '    {}\n'.format(content)
            return

        if self.lineno == self.end_lineno:
            end_offset = self.end_offset
            if not isinstance(end_offset, int) or end_offset == 0:
                end_offset = offset
        else:
            end_offset = len(stripped_right) + 1
        # clamp positions beyond the end of the line
        if text and offset > len(text):
            offset = len(stripped_right) + 1
        if text and end_offset > len(text):
            end_offset = len(stripped_right) + 1
        if offset >= end_offset or end_offset < 0:
            end_offset = offset + 1

        # 1-based columns -> indices into the stripped text
        start = offset - 1 - leading
        stop = end_offset - 1 - leading
        if start < 0:
            yield '    {}\n'.format(content)
            return

        # keep tabs and other whitespace so that the markers line up; other
        # characters are replaced by as many spaces as columns they take
        pad = ''.join(ch if ch.isspace() else ' ' * width
                      for ch, width in _char_widths(content[:start]))
        markers = '^' * max(1, _display_width(content[start:stop]))
        if colorize:
            content = (content[:start] + theme.error_highlight
                       + content[start:stop] + theme.reset + content[stop:])
            markers = theme.error_highlight + markers + theme.reset
        yield '    {}\n'.format(content)
        yield '    {}{}\n'.format(pad, markers)

    def _in_error_range(self, tok, first_line):
        """Whether token *tok* lies inside the highlighted error range."""
        try:
            start_row = int(self.lineno)
            start_col = int(self.offset) - 1
        except (TypeError, ValueError):
            return False
        try:
            end_row = int(self.end_lineno)
            end_col = int(self.end_offset) - 1
        except (TypeError, ValueError):
            end_row, end_col = start_row, start_col + 1
        row = tok.start[0] + first_line - 1
        tok_start = (row, tok.start[1])
        tok_end = (tok.end[0] + first_line - 1, tok.end[1])
        return (start_row, start_col) <= tok_start and \
            tok_end <= (end_row, end_col)

    def _find_keyword_typos(self):
        """For a plain "invalid syntax" error, look for a name next to the
        error that is a misspelt keyword: if replacing it makes the code
        valid, say so in the message."""
        within_range = False
        if self.msg != 'invalid syntax':
            if not (isinstance(self.msg, str) and
                    self.msg.startswith('invalid syntax. Perhaps')):
                # the parser already gave a specific explanation
                return
            # the parser only guessed: a name inside the highlighted
            # range may still be a misspelt keyword
            within_range = True
        lines = None
        first_line = 1
        if self._exc_metadata:
            meta_line, _, source = self._exc_metadata
            if source is not None:
                lines = source.splitlines()
                first_line = meta_line if meta_line > 0 else 1
        error_line = int(self.lineno) if self.lineno is not None else 1
        if lines is None and self.filename and not \
                self.filename.startswith('<'):
            try:
                with open(self.filename, encoding='utf-8') as f:
                    lines = f.read().splitlines()
            except Exception:
                lines = None
        if lines is None:
            lines = self.text.splitlines()
            error_line = len(lines)
            first_line = 1
        if not lines:
            return
        code = textwrap.dedent('\n'.join(lines))
        if len(code) > 4096:
            return
        code_lines = code.splitlines()

        import io
        import keyword
        import tokenize
        import codeop

        candidates = keyword.kwlist + keyword.softkwlist
        tried = 0
        tokens = []
        stream = tokenize.generate_tokens(io.StringIO(code).readline)
        while True:
            # broken code may stop the tokenizer early: keep what we got
            try:
                tokens.append(next(stream))
            except StopIteration:
                break
            except Exception:
                break
        relevant = [tok for tok in tokens
                    if tok.type == tokenize.NAME
                    and tok.start[0] == error_line - first_line + 1
                    and tok.string not in keyword.kwlist]
        if within_range:
            relevant = [tok for tok in relevant
                        if self._in_error_range(tok, first_line)]
        elif not relevant:
            relevant = [tok for tok in tokens if tok.type == tokenize.NAME
                        and tok.string not in keyword.kwlist]
        for tok in relevant:
            tried += 1
            if tried > 10:
                break
            for kw in _keyword_matches(tok.string, candidates):
                row = tok.start[0] - 1
                line = code_lines[row]
                fixed = line[:tok.start[1]] + kw + line[tok.end[1]:]
                attempt = code_lines[:row] + [fixed] + code_lines[row + 1:]
                try:
                    codeop.compile_command('\n'.join(attempt) + '\n',
                                           symbol='exec',
                                           flags=codeop.PyCF_ONLY_AST)
                except (SyntaxError, ValueError, OverflowError):
                    continue
                self.text = tok.line
                self.offset = tok.start[1] + 1
                self.end_offset = tok.end[1] + 1
                self.lineno = str(tok.start[0] + first_line - 1)
                self.end_lineno = str(tok.end[0] + first_line - 1)
                self.msg = f"invalid syntax. Did you mean '{kw}'?"
                return

    # -- the whole report ------------------------------------------------------------

    def print(self, *, file=None, chain=True, **kwargs):
        """Write the formatted exception to *file* (default sys.stderr)."""
        colorize = kwargs.get('colorize', False)
        if file is None:
            file = sys.stderr
        for line in self.format(chain=chain, colorize=colorize):
            print(line, file=file, end='')

    def format(self, *, chain=True, _ctx=None, **kwargs):
        """Format the exception (with its chain, unless *chain* is false).

        Yields strings ending in newlines, possibly containing several
        lines; the concatenation is the complete report.
        """
        colorize = kwargs.get('colorize', False)
        if _ctx is None:
            _ctx = _ExceptionPrintContext()

        # collect the chain, newest first
        chained = []
        te = self
        while te is not None:
            if not chain:
                chained.append((None, te))
                break
            if te.__cause__ is not None:
                link, nxt = _cause_message, te.__cause__
            elif te.__context__ is not None and not te.__suppress_context__:
                link, nxt = _context_message, te.__context__
            else:
                link, nxt = None, None
            chained.append((link, te))
            te = nxt

        for link, te in reversed(chained):
            if link is not None:
                yield from _ctx.emit(link)
            if te.exceptions is None:
                if te.stack:
                    yield from _ctx.emit(
                        'Traceback (most recent call last):\n')
                    yield from _ctx.emit(te.stack.format(colorize=colorize))
                yield from _ctx.emit(
                    te.format_exception_only(colorize=colorize))
            elif _ctx.exception_group_depth > self.max_group_depth:
                yield from _ctx.emit(
                    f'... (max_group_depth is {self.max_group_depth})\n')
            else:
                yield from te._format_group(_ctx, chain, colorize)

    def _format_group(self, _ctx, chain, colorize):
        toplevel = _ctx.exception_group_depth == 0
        if toplevel:
            _ctx.exception_group_depth += 1
        if self.stack:
            yield from _ctx.emit(
                'Exception Group Traceback (most recent call last):\n',
                margin_char='+' if toplevel else None)
            yield from _ctx.emit(self.stack.format(colorize=colorize))
        yield from _ctx.emit(self.format_exception_only(colorize=colorize))

        total = len(self.exceptions)
        width = self.max_group_width
        shown = total if total <= width else width + 1
        _ctx.need_close = False
        for index in range(shown):
            last = index == shown - 1
            if last:
                # a nested group may close the box for us
                _ctx.need_close = True
            truncated = index >= width
            title = '...' if truncated else f'{index + 1}'
            corner = '+-' if index == 0 else '  '
            yield (_ctx.indent() + corner +
                   f'+---------------- {title} ----------------\n')
            _ctx.exception_group_depth += 1
            if truncated:
                rest = total - width
                plural = 's' if rest > 1 else ''
                yield from _ctx.emit(f'and {rest} more exception{plural}\n')
            else:
                yield from self.exceptions[index].format(
                    chain=chain, _ctx=_ctx, colorize=colorize)
            if last and _ctx.need_close:
                yield _ctx.indent() + '+------------------------------------\n'
                _ctx.need_close = False
            _ctx.exception_group_depth -= 1

        if toplevel:
            assert _ctx.exception_group_depth == 1
            _ctx.exception_group_depth = 0


def _keyword_matches(word, candidates, limit=3):
    """Keywords that *word* could be a misspelling of, best first."""
    scored = []
    for kw in candidates:
        if kw == word:
            continue
        threshold = (len(kw) + len(word) + 3) * _MOVE_COST // 6
        distance = _levenshtein_distance(word, kw, threshold)
        if distance <= threshold:
            scored.append((distance, kw))
    scored.sort()
    found = [kw for _, kw in scored]
    if len(found) < limit:
        import difflib
        for kw in difflib.get_close_matches(word, candidates, n=limit,
                                            cutoff=0.5):
            if kw not in found and kw != word:
                found.append(kw)
    return found[:limit]


# ---------------------------------------------------------------------------
# Extra explanations appended to exception messages
# ---------------------------------------------------------------------------

def _extra_message(exc_type, exc_value, tb):
    """Text to append to str(exc_value): suggestions and hints."""
    try:
        if issubclass(exc_type, ModuleNotFoundError):
            return _module_not_found_hint(exc_value)
        if issubclass(exc_type, ImportError):
            return _import_hint(exc_value, tb)
        if issubclass(exc_type, AttributeError):
            return _attribute_hint(exc_value, tb)
        if issubclass(exc_type, UnboundLocalError):
            return ''
        if issubclass(exc_type, NameError):
            return _name_hint(exc_value, tb)
    except Exception:
        pass
    return ''


def _quoted_name(name, prefix=''):
    text = f"'{prefix}{name}'"
    if not name.isascii():
        text += f' ({ascii(name)})'
    return text


def _module_not_found_hint(exc_value):
    name = getattr(exc_value, 'name', None)
    if name in sys.stdlib_module_names:
        message = _MISSING_STDLIB_MODULE_MESSAGES.get(
            name, f'Standard library module {name!r} was not found')
        # replaces the message: printed as is
        return _ReplaceMessage(message)
    hint = ''
    if isinstance(name, str):
        hint = _find_incompatible_extension_module(name) or ''
        if hint:
            hint = '. ' + hint
    if sys.flags.no_site:
        hint += ('. Site initialization is disabled, did you forget to '
                 'add the site-packages directory to sys.path or to enable '
                 'your virtual environment?')
    return hint


class _ReplaceMessage(str):
    """Marker: the hint replaces the exception message."""


def _import_hint(exc_value, tb):
    wrong_name = getattr(exc_value, 'name_from', None)
    if wrong_name is None:
        return ''
    suggestion = _compute_suggestion_error(exc_value, tb, wrong_name)
    if suggestion:
        return f". Did you mean: {_quoted_name(suggestion)}?"
    return ''


def _attribute_hint(exc_value, tb):
    wrong_name = getattr(exc_value, 'name', None)
    if not isinstance(wrong_name, str):
        return ''
    obj = getattr(exc_value, 'obj', _sentinel)
    if obj is not _sentinel:
        hint = _cross_language_hint(obj, wrong_name)
        if hint:
            return hint
    suggestion = _compute_suggestion_error(exc_value, tb, wrong_name)
    if suggestion:
        return (f'. Did you mean {_quoted_name(suggestion, ".")}'
                f' instead of {_quoted_name(wrong_name, ".")}?')
    if obj is not _sentinel:
        nested = _nested_attribute(obj, wrong_name)
        if nested:
            return (f". Did you mean '.{nested}' instead of "
                    f"'.{wrong_name}'?")
    return ''


def _name_hint(exc_value, tb):
    wrong_name = getattr(exc_value, 'name', None)
    if not isinstance(wrong_name, str):
        return ''
    suggestion = _compute_suggestion_error(exc_value, tb, wrong_name)
    text = ''
    if suggestion:
        text = f'. Did you mean: {_quoted_name(suggestion)}?'
    if wrong_name in sys.stdlib_module_names:
        if suggestion:
            text += f" Or did you forget to import '{wrong_name}'?"
        else:
            text += f". Did you forget to import '{wrong_name}'?"
    return text


def _normalize_name(name):
    if name.isascii():
        return name
    import unicodedata
    return unicodedata.normalize('NFKC', name)


# Cross-language method names: (type, attribute) -> hint.  A hint that is
# an identifier names the Python method; anything else is a sentence.
_CROSS_LANGUAGE_HINTS = (
    (list, {
        'push': 'append',
        'concat': 'extend',
        'addAll': 'extend',
        'contains': "Use 'x in list'.",
        'add': "Did you mean to use a 'set' object?",
        'size': "Use 'len(list)'.",
        'length': "Use 'len(list)'.",
    }),
    (str, {
        'toUpperCase': 'upper',
        'toLowerCase': 'lower',
        'trimStart': 'lstrip',
        'trimEnd': 'rstrip',
        'length': "Use 'len(str)'.",
    }),
    (dict, {
        'keySet': 'keys',
        'entrySet': 'items',
        'entries': 'items',
        'putAll': 'update',
        'put': 'Use d[k] = v.',
    }),
    (tuple, dict.fromkeys(
        ('append', 'extend', 'insert', 'remove', 'pop', 'clear', 'sort',
         'reverse'),
        "Did you mean to use a 'list' object?")),
    (frozenset, dict.fromkeys(
        ('add', 'discard', 'remove', 'update', 'pop', 'clear',
         'difference_update', 'intersection_update',
         'symmetric_difference_update'),
        "Did you mean to use a 'set' object?")),
    (float, dict.fromkeys(
        ('__or__', '__and__', '__xor__', '__lshift__', '__rshift__',
         '__ror__', '__rand__', '__rxor__', '__rlshift__', '__rrshift__',
         '__invert__'),
        "Bitwise operators are not supported for 'float' objects; "
        "convert to 'int' first.")),
)


def _cross_language_hint(obj, wrong_name):
    table = list(_CROSS_LANGUAGE_HINTS)
    frozendict_type = getattr(__import__('builtins'), 'frozendict', None)
    if frozendict_type is not None:
        table.append((frozendict_type, dict.fromkeys(
            ('update', 'pop', 'popitem', 'clear', 'setdefault'),
            "Did you mean to use a 'dict' object?")))
    for kind, hints in table:
        if isinstance(obj, kind) and wrong_name in hints:
            hint = hints[wrong_name]
            if hint.isidentifier():
                return f". Did you mean '.{hint}'?"
            return '. ' + hint
    return ''


def _nested_attribute(obj, wrong_name):
    """'name.wrong_name' if one of the first public attributes of *obj*
    has the wanted attribute (found without running any code of it)."""
    try:
        names = sorted(x for x in dir(obj) if isinstance(x, str))
    except Exception:
        return None
    checked = 0
    for name in names:
        if name.startswith('_'):
            continue
        checked += 1
        if checked > 20:
            break
        try:
            value = getattr(obj, name)
        except Exception:
            continue
        if callable(value) and not isinstance(value, type):
            continue
        try:
            inner = dir(value)
        except Exception:
            continue
        if wrong_name in inner:
            return f'{name}.{wrong_name}'
    return None


def _find_incompatible_extension_module(module_name):
    """Hint text if an extension module of that name exists but was built
    for another Python (different ABI tag)."""
    import importlib.machinery

    if not module_name or not importlib.machinery.EXTENSION_SUFFIXES:
        return None
    # the last suffix is the plain, untagged one (e.g. '.so')
    plain_suffix = importlib.machinery.EXTENSION_SUFFIXES[-1]
    parent, _, child = module_name.rpartition('.')
    if parent:
        try:
            package = __import__(parent, fromlist=['__path__'])
            search = list(package.__path__)
        except Exception:
            return None
    else:
        search = sys.path
    import os
    for directory in search:
        try:
            entries = os.listdir(directory or '.')
        except OSError:
            continue
        for entry in entries:
            if (entry.startswith(child + '.') and entry.endswith(plain_suffix)
                    and entry not in (child + suffix for suffix in
                                      importlib.machinery.EXTENSION_SUFFIXES)):
                return ('Although a module with this name was found for a '
                        f'different Python version ({entry}).')
    return None


def _build_missing_stdlib_messages():
    windows_only = {'msvcrt', 'winreg', 'winsound', '_winapi',
                    '_overlapped', 'nt', '_msi', '_wmi'}
    unix_only = {'posix', 'pwd', 'grp', 'fcntl', 'termios', 'resource',
                 'syslog', '_posixsubprocess', 'tty', 'pty', 'curses',
                 '_curses', 'readline', '_posixshmem'}
    messages = {}
    if sys.platform == 'win32':
        for name in unix_only:
            messages[name] = (f'Unsupported platform for Unix-only standard '
                              f'library module {name!r}')
    else:
        for name in windows_only:
            messages[name] = (f'Unsupported platform for Windows-only '
                              f'standard library module {name!r}')
    return messages


_MISSING_STDLIB_MODULE_MESSAGES = _build_missing_stdlib_messages()


# ---------------------------------------------------------------------------
# "Did you mean ...?"
# ---------------------------------------------------------------------------

_MAX_CANDIDATE_ITEMS = 750
_MAX_STRING_SIZE = 40
_MOVE_COST = 2
_CASE_COST = 1


def _substitution_cost(ch_a, ch_b):
    if ch_a == ch_b:
        return 0
    if ch_a.lower() == ch_b.lower():
        return _CASE_COST
    return _MOVE_COST


def _levenshtein_distance(a, b, max_cost):
    """Weighted edit distance between *a* and *b*: insertions, deletions
    and substitutions cost 2, a change of case only 1.  Anything above
    *max_cost* may be reported as max_cost + 1 without exact computation.
    """
    if a == b:
        return 0
    # common prefix and suffix do not matter
    start = 0
    limit = min(len(a), len(b))
    while start < limit and a[start] == b[start]:
        start += 1
    a, b = a[start:], b[start:]
    end = 0
    limit = min(len(a), len(b))
    while end < limit and a[-1 - end] == b[-1 - end]:
        end += 1
    if end:
        a, b = a[:-end], b[:-end]

    if not a or not b:
        return _MOVE_COST * (len(a) + len(b))
    if len(a) > _MAX_STRING_SIZE or len(b) > _MAX_STRING_SIZE:
        return max_cost + 1
    if len(b) < len(a):
        a, b = b, a
    # at least the length difference has to be inserted
    if (len(b) - len(a)) * _MOVE_COST > max_cost:
        return max_cost + 1

    # costs of turning a[:i] into the current prefix of b, row by row
    previous = [i * _MOVE_COST for i in range(len(a) + 1)]
    for j, ch_b in enumerate(b, 1):
        current = [j * _MOVE_COST]
        row_best = current[0]
        for i, ch_a in enumerate(a, 1):
            cost = min(previous[i - 1] + _substitution_cost(ch_a, ch_b),
                       previous[i] + _MOVE_COST,
                       current[i - 1] + _MOVE_COST)
            current.append(cost)
            if cost < row_best:
                row_best = cost
        if row_best > max_cost:
            # every alignment is already too expensive
            return max_cost + 1
        previous = current
    return previous[-1]


def _candidate_names(exc_value, tb, wrong_name):
    """The names that *wrong_name* may be a misspelling of, or a direct
    answer ('self.x'), as (names, answer)."""
    if isinstance(exc_value, AttributeError):
        obj = exc_value.obj
        try:
            try:
                # the raw __dir__ result: dir() would fail on unsortable
                # entries, which are filtered out below anyway
                names = list(type(obj).__dir__(obj))
            except TypeError:
                names = (list(obj.__class__.__dict__.keys()) +
                         list(obj.__dict__.keys()))
            names = sorted(x for x in names if isinstance(x, str))
            hide_private = wrong_name[:1] != '_'
            if hide_private and tb is not None:
                while tb.tb_next is not None:
                    tb = tb.tb_next
                frame_locals = tb.tb_frame.f_locals
                if 'self' in frame_locals and frame_locals['self'] is obj:
                    # inside a method of the object: private names are fine
                    hide_private = False
            if hide_private:
                names = [x for x in names if x[:1] != '_']
        except Exception:
            return None, None
        return names, None

    if isinstance(exc_value, ImportError):
        try:
            module = __import__(exc_value.name)
            try:
                names = dir(module)
            except TypeError:
                names = list(module.__dict__.keys())
            names = sorted(x for x in names if isinstance(x, str))
            if wrong_name[:1] != '_':
                names = [x for x in names if x[:1] != '_']
        except Exception:
            return None, None
        return names, None

    # NameError: look at the innermost frame
    if tb is None:
        return None, None
    while tb.tb_next is not None:
        tb = tb.tb_next
    frame = tb.tb_frame
    names = (list(frame.f_locals) + list(frame.f_globals) +
             list(frame.f_builtins))
    names = [x for x in names if isinstance(x, str)]
    if 'self' in frame.f_locals:
        owner = frame.f_locals['self']
        try:
            found = hasattr(owner, wrong_name)
        except Exception:
            found = False
        if found:
            return names, f'self.{wrong_name}'
    return names, None


def _compute_suggestion_error(exc_value, tb, wrong_name):
    if wrong_name is None or not isinstance(wrong_name, str):
        return None
    names, answer = _candidate_names(exc_value, tb, wrong_name)
    if answer is not None:
        return answer
    if names is None:
        return None

    normalized = _normalize_name(wrong_name)
    if normalized != wrong_name and normalized in names:
        return normalized

    if len(names) > _MAX_CANDIDATE_ITEMS:
        return None
    wrong_len = len(wrong_name)
    if wrong_len > _MAX_STRING_SIZE:
        return None
    best = None
    best_distance = sys.maxsize
    for name in names:
        if name == wrong_name:
            # the name exists after all (e.g. failing property)
            continue
        # at most about a third of the characters may differ
        allowed = (len(name) + wrong_len + 3) * _MOVE_COST // 6
        allowed = min(allowed, best_distance - 1)
        distance = _levenshtein_distance(wrong_name, name, allowed)
        if distance > allowed:
            continue
        if best is None or distance < best_distance:
            best = name
            best_distance = distance
    return best
