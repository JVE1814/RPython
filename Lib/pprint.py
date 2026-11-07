"""Pretty-print Python data structures.

The output is meant to be readable by people and, for literal-like data,
valid input to eval().  Containers whose one-line representation would be
wider than the requested width are spread over several lines.

Functions:

    pformat()     -- the pretty-printed representation as a string
    pprint()      -- write that representation to a stream (sys.stdout)
    pp()          -- pprint() that keeps dictionaries in insertion order
    saferepr()    -- a one-line representation that survives recursion
    isreadable()  -- whether saferepr() of an object could be eval()'ed
    isrecursive() -- whether an object contains itself

Class:

    PrettyPrinter -- holds the formatting options; subclasses may refine
                     format() to change how individual objects are shown
"""

import collections as _collections
import sys as _sys
import types as _types
from collections.abc import MappingView as _MappingView
from operator import itemgetter as _itemgetter

__all__ = ["pprint", "pformat", "isreadable", "isrecursive", "saferepr",
           "PrettyPrinter", "pp"]


# ---------------------------------------------------------------------------
# Convenience functions

def pprint(object, stream=None, indent=1, width=80, depth=None, *,
           compact=False, sort_dicts=True, underscore_numbers=False,
           expand=False):
    """Write the pretty representation of object to stream (default:
    sys.stdout), followed by a newline."""
    PrettyPrinter(indent=indent, width=width, depth=depth, stream=stream,
                  compact=compact, sort_dicts=sort_dicts,
                  underscore_numbers=underscore_numbers,
                  expand=expand).pprint(object)


def pformat(object, indent=1, width=80, depth=None, *, compact=False,
            sort_dicts=True, underscore_numbers=False, expand=False):
    """Return the pretty representation of object as a string."""
    return PrettyPrinter(indent=indent, width=width, depth=depth,
                         compact=compact, sort_dicts=sort_dicts,
                         underscore_numbers=underscore_numbers,
                         expand=expand).pformat(object)


def pp(object, *args, sort_dicts=False, **kwargs):
    """Like pprint(), but dictionaries keep their insertion order unless
    sort_dicts=True is passed."""
    pprint(object, *args, sort_dicts=sort_dicts, **kwargs)


def saferepr(object):
    """Return a one-line representation of object in which containers that
    hold themselves show a marker instead of recursing forever."""
    return _DEFAULT._safe_repr(object, {}, None, 0)[0]


def isreadable(object):
    """Return True if saferepr(object) is meant to be accepted by eval()."""
    return _DEFAULT._safe_repr(object, {}, None, 0)[1]


def isrecursive(object):
    """Return True if object (directly or indirectly) contains itself."""
    return _DEFAULT._safe_repr(object, {}, None, 0)[2]


# ---------------------------------------------------------------------------
# Ordering helpers

class _OrderingKey:
    """Sort key for values of arbitrary, possibly incomparable types.

    Values compare with "<" when they support it; a pair that raises
    TypeError is ordered by the name of the value's type, then by id().
    """

    __slots__ = ('value',)

    def __init__(self, value):
        self.value = value

    def __lt__(self, other):
        mine = self.value
        theirs = other.value
        try:
            return mine < theirs
        except TypeError:
            return ((str(type(mine)), id(mine)) <
                    (str(type(theirs)), id(theirs)))


_first = _itemgetter(0)


def _first_ordering_key(pair):
    return _OrderingKey(pair[0])


def _ordered(values, by_first=False):
    """Return a sorted list of values (sorted by the first element of each
    value when by_first is true), tolerating incomparable elements.

    The plain sort is tried first: when no comparison fails it produces the
    same order as the tolerant key, only faster.
    """
    try:
        return sorted(values, key=_first if by_first else None)
    except TypeError:
        return sorted(values,
                      key=_first_ordering_key if by_first else _OrderingKey)


def _cycle_marker(obj):
    return '<Recursion on %s with id=%s>' % (type(obj).__name__, id(obj))


def _lookup(table, cls):
    """Find cls's entry in table: an exact match, or the entry of the
    nearest registered base class provided cls still uses that base's
    __repr__ (a subclass with its own __repr__ is shown with it)."""
    entry = table.get(cls)
    if entry is not None:
        return entry
    for base in cls.__mro__[1:-1]:
        entry = table.get(base)
        if entry is not None:
            if cls.__repr__ is base.__repr__:
                return entry
            return None
    return None


def _is_generated_dataclass(obj):
    """True for instances of dataclasses whose __repr__ was generated."""
    cls = type(obj)
    if isinstance(obj, type) or not hasattr(cls, '__dataclass_fields__'):
        return False
    params = getattr(cls, '__dataclass_params__', None)
    if params is None or not getattr(params, 'repr', False):
        return False
    inner = getattr(cls.__repr__, '__wrapped__', None)
    return '__create_fn__' in getattr(inner, '__qualname__', '')


_word_finder = None


def _words(line):
    """Split a line into pieces: a run of non-blanks plus following blanks."""
    global _word_finder
    if _word_finder is None:
        import re
        _word_finder = re.compile(r'\S+\s*|\s+').findall
    return _word_finder(line)


def _text_chunks(text, room, tail):
    """Cut text into string-literal reprs no wider than room.

    Every line break ends a chunk; longer lines are cut between words.  The
    final chunk must leave tail columns free.  A word wider than room
    becomes a chunk of its own.
    """
    chunks = []
    lines = text.splitlines(True)
    final_line = len(lines) - 1
    for number, line in enumerate(lines):
        last_line = number == final_line
        whole = repr(line)
        if len(whole) <= (room - tail if last_line else room):
            chunks.append(whole)
            continue
        words = _words(line)
        final_word = len(words) - 1
        pending = ''
        for index, word in enumerate(words):
            limit = room - tail if last_line and index == final_word else room
            joined = pending + word
            if pending and len(repr(joined)) > limit:
                chunks.append(repr(pending))
                pending = word
            else:
                pending = joined
        if pending:
            chunks.append(repr(pending))
    return chunks


def _bytes_chunks(data, room, tail):
    """Cut data into bytes-literal reprs no wider than room, breaking only
    at offsets that are multiples of four.  tail is reserved after a final
    group shorter than four bytes."""
    chunks = []
    pending = b''
    short_tail_at = len(data) - len(data) % 4
    for start in range(0, len(data), 4):
        group = data[start:start + 4]
        if start == short_tail_at:
            room -= tail
        joined = pending + group
        if pending and len(repr(joined)) > room:
            chunks.append(repr(pending))
            pending = group
        else:
            pending = joined
    if pending:
        chunks.append(repr(pending))
    return chunks


# ---------------------------------------------------------------------------
# One-line representations (the engine behind format() and saferepr())

_PLAIN_TYPES = frozenset({str, bytes, bytearray, float, complex, bool,
                          type(None)})

_dict_keys = type({}.keys())
_dict_values = type({}.values())
_dict_items = type({}.items())
_odict_keys = type(_collections.OrderedDict().keys())
_odict_values = type(_collections.OrderedDict().values())
_odict_items = type(_collections.OrderedDict().items())
_ITEM_VIEWS = (_dict_items, _odict_items)


def _parts(printer, values, context, maxlevels, level):
    """Format each value one level down.

    Returns (list of texts, all readable, any recursive).  With the stock
    format()/_safe_repr() the type dispatch is done right here, which saves
    two calls per value.
    """
    texts = []
    add = texts.append
    readable = True
    recursive = False
    kind = type(printer)
    if kind.format is _STOCK_FORMAT and kind._safe_repr is _STOCK_SAFE_REPR:
        inline_ints = not printer._underscore_numbers
        handlers = _FLAT
        for value in values:
            cls = type(value)
            if cls in _PLAIN_TYPES or (cls is int and inline_ints):
                add(repr(value))
                continue
            handler = handlers.get(cls)
            if handler is None:
                handler = _lookup(handlers, cls)
                if handler is None:
                    text = repr(value)
                    add(text)
                    if not text or text.startswith('<'):
                        readable = False
                    continue
            text, ok, rec = handler(printer, value, context, maxlevels, level)
            add(text)
            if not ok:
                readable = False
            if rec:
                recursive = True
    else:
        fmt = printer.format
        for value in values:
            text, ok, rec = fmt(value, context, maxlevels, level)
            add(text)
            if not ok:
                readable = False
            if rec:
                recursive = True
    return texts, readable, recursive


def _pair_parts(printer, pairs, context, maxlevels, level):
    """Like _parts() for (key, value) pairs, giving "key: value" texts."""
    texts, ok, rec = _parts(printer, [x for pair in pairs for x in pair],
                            context, maxlevels, level)
    joined = [k + ': ' + v for k, v in zip(texts[::2], texts[1::2])]
    return joined, ok, rec


def _flat_int(printer, obj, context, maxlevels, level):
    if printer._underscore_numbers:
        return f'{obj:_d}', True, False
    return repr(obj), True, False


def _flat_list(printer, obj, context, maxlevels, level):
    if not obj:
        return '[]', True, False
    key = id(obj)
    if maxlevels and level >= maxlevels:
        return '[...]', False, key in context
    if key in context:
        return _cycle_marker(obj), False, True
    context[key] = 1
    texts, ok, rec = _parts(printer, obj, context, maxlevels, level + 1)
    del context[key]
    return '[' + ', '.join(texts) + ']', ok, rec


def _flat_tuple(printer, obj, context, maxlevels, level):
    if not obj:
        return '()', True, False
    key = id(obj)
    if maxlevels and level >= maxlevels:
        return ('(...,)' if len(obj) == 1 else '(...)'), False, key in context
    if key in context:
        return _cycle_marker(obj), False, True
    context[key] = 1
    texts, ok, rec = _parts(printer, obj, context, maxlevels, level + 1)
    del context[key]
    if len(texts) == 1:
        return '(' + texts[0] + ',)', ok, rec
    return '(' + ', '.join(texts) + ')', ok, rec


def _flat_mapping_body(printer, obj, context, maxlevels, level):
    """The "{key: value, ...}" text of a dict-like object (never empty)."""
    pairs = obj.items()
    if printer._sort_dicts:
        pairs = _ordered(pairs, True)
    texts, ok, rec = _pair_parts(printer, pairs, context, maxlevels, level)
    return '{' + ', '.join(texts) + '}', ok, rec


def _flat_dict(printer, obj, context, maxlevels, level):
    if not obj:
        return '{}', True, False
    key = id(obj)
    if maxlevels and level >= maxlevels:
        return '{...}', False, key in context
    if key in context:
        return _cycle_marker(obj), False, True
    context[key] = 1
    text, ok, rec = _flat_mapping_body(printer, obj, context, maxlevels,
                                       level + 1)
    del context[key]
    return text, ok, rec


def _flat_frozendict(printer, obj, context, maxlevels, level):
    name = type(obj).__name__
    if not obj:
        return name + '()', True, False
    key = id(obj)
    if maxlevels and level >= maxlevels:
        return name + '({...})', False, key in context
    if key in context:
        return _cycle_marker(obj), False, True
    context[key] = 1
    text, ok, rec = _flat_mapping_body(printer, obj, context, maxlevels,
                                       level + 1)
    del context[key]
    return name + '(' + text + ')', ok, rec


def _flat_dict_view(printer, obj, context, maxlevels, level):
    name = type(obj).__name__
    if not obj:
        return name + '([])', True, False
    key = id(obj)
    if maxlevels and level >= maxlevels:
        return '{...}', False, key in context
    if key in context:
        return _cycle_marker(obj), False, True
    context[key] = 1
    values = obj
    if printer._sort_dicts:
        values = _ordered(obj, isinstance(obj, _ITEM_VIEWS))
    texts, ok, rec = _parts(printer, values, context, maxlevels, level + 1)
    del context[key]
    return name + '([' + ', '.join(texts) + '])', ok, rec


def _flat_mapping_view(printer, obj, context, maxlevels, level):
    key = id(obj)
    if maxlevels and level >= maxlevels:
        return '{...}', False, key in context
    if key in context:
        return _cycle_marker(obj), False, True
    context[key] = 1
    # the view adds no nesting level of its own: its mapping is shown
    # at the level of the view
    text, ok, rec = printer.format(obj._mapping, context, maxlevels, level)
    del context[key]
    return type(obj).__name__ + '(' + text + ')', ok, rec


_FLAT = {
    int: _flat_int,
    list: _flat_list,
    tuple: _flat_tuple,
    dict: _flat_dict,
    frozendict: _flat_frozendict,
    _dict_keys: _flat_dict_view,
    _dict_values: _flat_dict_view,
    _dict_items: _flat_dict_view,
    _odict_keys: _flat_dict_view,
    _odict_values: _flat_dict_view,
    _odict_items: _flat_dict_view,
    _MappingView: _flat_mapping_view,
}


# The pretty-printer first needs the one-line text of an object to decide
# whether it fits; for plain lists, tuples and dicts it also keeps the texts
# of the items, so that a container spread over several lines does not
# format its items a second time.  These return (text, parts) where parts is
# None when no item texts were produced.

def _split_sequence(printer, obj, context, maxlevels, level):
    key = id(obj)
    if not obj or (maxlevels and level >= maxlevels) or key in context:
        return _FLAT[type(obj)](printer, obj, context, maxlevels, level)[0], None
    context[key] = 1
    texts = _parts(printer, obj, context, maxlevels, level + 1)[0]
    del context[key]
    if type(obj) is list:
        return '[' + ', '.join(texts) + ']', texts
    if len(texts) == 1:
        return '(' + texts[0] + ',)', texts
    return '(' + ', '.join(texts) + ')', texts


def _split_dict(printer, obj, context, maxlevels, level):
    key = id(obj)
    if not obj or (maxlevels and level >= maxlevels) or key in context:
        return _flat_dict(printer, obj, context, maxlevels, level)[0], None
    context[key] = 1
    pairs = _ordered(obj.items(), True) if printer._sort_dicts else list(
        obj.items())
    ktexts = _parts(printer, [pair[0] for pair in pairs], context,
                    maxlevels, level + 1)[0]
    vtexts = _parts(printer, [pair[1] for pair in pairs], context,
                    maxlevels, level + 1)[0]
    del context[key]
    joined = ', '.join([k + ': ' + v for k, v in zip(ktexts, vtexts)])
    return '{' + joined + '}', (pairs, ktexts, vtexts)


_SPLIT = {list: _split_sequence, tuple: _split_sequence, dict: _split_dict}


# ---------------------------------------------------------------------------

class PrettyPrinter:
    """Formatter holding the pretty-printing options.

    indent      -- extra columns for each nesting level
    width       -- the column limit the output tries to respect
    depth       -- nesting levels to show; deeper containers become "..."
    stream      -- where pprint() writes (sys.stdout at creation time if
                   None; pprint() writes nothing when that is None too)
    compact     -- pack several short sequence items onto one line
    sort_dicts  -- show dictionary entries sorted by key
    underscore_numbers -- group the digits of integers with "_"
    expand      -- break containers after the opening bracket, one item per
                   line with a trailing comma, closing bracket on its own
                   line (cannot be combined with compact)
    """

    def __init__(self, indent=1, width=80, depth=None, stream=None, *,
                 compact=False, sort_dicts=True, underscore_numbers=False,
                 expand=False):
        indent = int(indent)
        width = int(width)
        if indent < 0:
            raise ValueError('indent must not be negative')
        if depth is not None and depth <= 0:
            raise ValueError('depth must be a positive number')
        if not width:
            raise ValueError('width must not be zero')
        if compact and expand:
            raise ValueError('compact and expand are mutually exclusive')
        self._indent = indent
        self._width = width
        self._depth = depth
        self._stream = _sys.stdout if stream is None else stream
        self._compact = bool(compact)
        self._sort_dicts = sort_dicts
        self._underscore_numbers = underscore_numbers
        self._expand = bool(expand)

    # -- public interface ---------------------------------------------------

    def pprint(self, object):
        """Write the pretty representation of object and a newline."""
        stream = self._stream
        if stream is not None:
            self._emit(object, stream.write, 0, 0, {}, 0)
            stream.write('\n')

    def pformat(self, object):
        """Return the pretty representation of object."""
        pieces = []
        self._emit(object, pieces.append, 0, 0, {}, 0)
        return ''.join(pieces)

    def isrecursive(self, object):
        """Return True if object contains itself."""
        return self.format(object, {}, 0, 0)[2]

    def isreadable(self, object):
        """Return True if the representation can be passed to eval()."""
        _, readable, recursive = self.format(object, {}, 0, 0)
        return readable and not recursive

    def format(self, object, context, maxlevels, level):
        """Return (text, readable, recursive) for object on one line.

        context maps the ids of the containers currently being shown to a
        true value; maxlevels is the depth limit (0 or None: unlimited) and
        level the current depth.  Subclasses may override this hook; it is
        used for every object the printer shows, nested ones included.
        """
        return self._safe_repr(object, context, maxlevels, level)

    def _safe_repr(self, object, context, maxlevels, level):
        cls = type(object)
        if cls in _PLAIN_TYPES:
            return repr(object), True, False
        flat = _lookup(_FLAT, cls)
        if flat is not None:
            return flat(self, object, context, maxlevels, level)
        rep = repr(object)
        return rep, bool(rep) and not rep.startswith('<'), False

    # -- layout -------------------------------------------------------------
    #
    # _emit() and the _lay_* methods write the representation of one object
    # through `write`.  col is the column of the block the object belongs
    # to, tail the number of characters that will follow the object on its
    # last line, level the nesting depth.  In expand mode nested blocks are
    # indented from the start of the line, not from the object, so lead
    # counts the characters already written on the line after col.

    def _repr(self, obj, context, level):
        if _stock_hooks(self):
            # through format(), as debuggers stepping out of a __repr__ expect
            return self.format(obj, context, self._depth, level)[0]
        return self.format(obj, context.copy(), self._depth, level)[0]

    def _emit(self, obj, write, col, tail, context, level, lead=0,
              text=None):
        key = id(obj)
        if key in context:
            write(_cycle_marker(obj))
            return
        parts = None
        if text is None:
            split = _SPLIT.get(type(obj))
            if split is not None and _stock_hooks(self):
                text, parts = split(self, obj, context, self._depth, level)
            else:
                text = self._repr(obj, context, level)
        if len(text) <= self._width - col - lead - tail:
            write(text)
            return
        if parts is not None:
            # a plain list, tuple or dict whose item texts are already known
            context[key] = 1
            if type(obj) is dict:
                self._pairs(write, '{', parts[0], '}', col, tail, context,
                            level + 1, parts[1], parts[2])
            else:
                self._sequence(obj, write, col, tail, context, level + 1,
                               parts)
            del context[key]
            return
        layout = _lookup(_LAYOUTS, type(obj))
        if layout is None:
            if not _is_generated_dataclass(obj):
                write(text)
                return
            layout = PrettyPrinter._lay_dataclass
        context[key] = 1
        layout(self, obj, write, col, tail, context, level + 1, lead)
        del context[key]

    # Generic shapes.  "values" are shown bare, "pairs" as key: value with
    # the key's repr, "fields" as name=value with the name as given.

    def _values(self, write, opener, values, closer, col, tail, context,
                level, *, item_col=None, pad=True, last_tail=None,
                packable=True, texts=None):
        # texts, when given, holds the one-line text of each value
        if self._expand:
            self._expanded(write, opener, values, closer, col, context,
                           level, None, texts)
            return
        write(opener)
        if pad and self._indent > 1:
            write(' ' * (self._indent - 1))
        if item_col is None:
            item_col = col + len(opener) + (self._indent - 1 if pad else 0)
        if last_tail is None:
            last_tail = tail + len(closer)
        if values:
            if self._compact and packable:
                self._packed(write, values, item_col, last_tail, context,
                             level, texts)
            else:
                newline = ',\n' + ' ' * item_col
                final = len(values) - 1
                for index, value in enumerate(values):
                    if index:
                        write(newline)
                    self._emit(value, write, item_col,
                               last_tail if index == final else 1,
                               context, level,
                               text=None if texts is None else texts[index])
        write(closer)

    def _packed(self, write, values, item_col, last_tail, context, level,
                texts=None):
        # Compact mode: fill each line with as many short items as fit.  A
        # line of k items needs their widths plus 2 per item minus 1 (the
        # separators and the trailing comma); room starts one higher to
        # account for that.
        newline = ',\n' + ' ' * item_col
        line_room = room = self._width - item_col + 1
        separator = ''
        final = len(values) - 1
        for index, value in enumerate(values):
            if index == final:
                line_room -= last_tail
                room -= last_tail
            if texts is None:
                text = self._repr(value, context, level)
            else:
                text = texts[index]
            needed = len(text) + 2
            if needed > room:
                room = line_room
                if separator:
                    separator = newline
            if needed <= room:
                room -= needed
                write(separator)
                write(text)
                separator = ', '
                continue
            write(separator)
            separator = newline
            self._emit(value, write, item_col,
                       last_tail if index == final else 1, context, level,
                       text=text)

    def _pairs(self, write, opener, pairs, closer, col, tail, context,
               level, ktexts=None, vtexts=None):
        # ktexts/vtexts, when given, hold the one-line texts of the keys and
        # of the values
        if self._expand:
            self._expanded(write, opener, pairs, closer, col, context,
                           level, False, vtexts, ktexts)
            return
        write(opener)
        if self._indent > 1:
            write(' ' * (self._indent - 1))
        item_col = col + len(opener) + self._indent - 1
        newline = ',\n' + ' ' * item_col
        final = len(pairs) - 1
        for index, (key, value) in enumerate(pairs):
            if ktexts is None:
                ktext = self._repr(key, context, level)
            else:
                ktext = ktexts[index]
            write(ktext)
            write(': ')
            self._emit(value, write, item_col + len(ktext) + 2,
                       tail + len(closer) if index == final else 1,
                       context, level,
                       text=None if vtexts is None else vtexts[index])
            if index != final:
                write(newline)
        write(closer)

    def _fields(self, write, opener, fields, closer, col, tail, context,
                level):
        # In the classic layout the last field leaves room for what follows
        # the object but not for its own closing parenthesis.
        if self._expand:
            self._expanded(write, opener, fields, closer, col, context,
                           level, True)
            return
        write(opener)
        item_col = col + len(opener)
        newline = ',\n' + ' ' * item_col
        final = len(fields) - 1
        for index, (name, value) in enumerate(fields):
            write(name)
            write('=')
            if id(value) in context:
                write('...')
            else:
                self._emit(value, write, item_col + len(name) + 1,
                           tail if index == final else 1,
                           context, level)
            if index != final:
                write(newline)
        write(closer)

    def _expanded(self, write, opener, items, closer, col, context, level,
                  named, texts=None, ktexts=None):
        # One item per line, each followed by a comma, one indentation step
        # in from col; the closer goes on a line of its own at col.  named
        # is None for bare values, False for key: value pairs and True for
        # name=value fields.
        write(opener)
        if items:
            inner = col + self._indent
            newline = '\n' + ' ' * inner
            for index, item in enumerate(items):
                write(newline)
                text = None if texts is None else texts[index]
                if named is None:
                    self._emit(item, write, inner, 1, context, level,
                               text=text)
                else:
                    name, value = item
                    if named:
                        prefix = name + '='
                        if id(value) in context:
                            write(prefix + '...,')
                            continue
                    elif ktexts is not None:
                        prefix = ktexts[index] + ': '
                    else:
                        prefix = self._repr(name, context, level) + ': '
                    write(prefix)
                    self._emit(value, write, inner, 1, context, level,
                               lead=len(prefix), text=text)
                write(',')
            write('\n' + ' ' * col)
        write(closer)

    def _call_around(self, write, prefix, inner, col, tail, context, level,
                     lead):
        """Write prefix, then inner formatted on the same line, then ")"."""
        write(prefix)
        if self._expand:
            self._emit(inner, write, col, tail + 1, context, level,
                       lead + len(prefix))
        else:
            self._emit(inner, write, col + len(prefix), tail + 1, context,
                       level)
        write(')')

    def _chunks(self, write, chunks, opener, closer, col):
        """Write literal chunks one per line (implicit concatenation)."""
        if self._expand and opener:
            inner = ' ' * (col + self._indent)
            write(opener + '\n' + inner)
            write(('\n' + inner).join(chunks))
            write('\n' + ' ' * col + closer)
        else:
            write(opener)
            write(('\n' + ' ' * (col + len(opener))).join(chunks))
            write(closer)

    # -- layouts for the individual types -----------------------------------

    def _lay_dict(self, obj, write, col, tail, context, level, lead=0):
        pairs = _ordered(obj.items(), True) if self._sort_dicts else list(
            obj.items())
        self._pairs(write, '{', pairs, '}', col, tail, context, level)

    def _lay_frozendict(self, obj, write, col, tail, context, level, lead=0):
        if not obj:
            write(repr(obj))
            return
        pairs = _ordered(obj.items(), True) if self._sort_dicts else list(
            obj.items())
        self._pairs(write, type(obj).__name__ + '({', pairs, '})', col, tail,
                    context, level)

    def _lay_counter(self, obj, write, col, tail, context, level, lead=0):
        if not obj:
            write(repr(obj))
            return
        self._pairs(write, type(obj).__name__ + '({', obj.most_common(),
                    '})', col, tail, context, level)

    def _lay_defaultdict(self, obj, write, col, tail, context, level,
                         lead=0):
        if not obj:
            write(repr(obj))
            return
        name = type(obj).__name__
        factory = self._repr(obj.default_factory, context, level)
        if self._expand:
            pairs = _ordered(obj.items(), True) if self._sort_dicts else list(
                obj.items())
            self._pairs(write, f'{name}({factory}, {{', pairs, '})', col,
                        tail, context, level)
            return
        col += len(name) + 1
        write(f'{name}({factory},\n' + ' ' * col)
        self._lay_dict(obj, write, col, tail + 1, context, level)
        write(')')

    def _lay_ordered_dict(self, obj, write, col, tail, context, level,
                          lead=0):
        if not obj:
            write(repr(obj))
            return
        self._call_around(write, type(obj).__name__ + '(', list(obj.items()),
                          col, tail, context, level, lead)

    def _lay_mappingproxy(self, obj, write, col, tail, context, level,
                          lead=0):
        self._call_around(write, 'mappingproxy(', obj.copy(), col, tail,
                          context, level, lead)

    def _sequence(self, obj, write, col, tail, context, level, texts=None):
        if isinstance(obj, list):
            opener, closer = '[', ']'
        else:
            opener = '('
            closer = ',)' if len(obj) == 1 and not self._expand else ')'
        self._values(write, opener, obj, closer, col, tail, context, level,
                     texts=texts)

    def _lay_list(self, obj, write, col, tail, context, level, lead=0):
        self._sequence(obj, write, col, tail, context, level)

    _lay_tuple = _lay_list

    def _lay_set(self, obj, write, col, tail, context, level, lead=0):
        if not obj:
            write(repr(obj))
            return
        if type(obj) is set:
            opener, closer = '{', '}'
        else:
            opener, closer = type(obj).__name__ + '({', '})'
        self._values(write, opener, _ordered(obj), closer, col, tail,
                     context, level)

    def _lay_dict_view(self, obj, write, col, tail, context, level, lead=0):
        if not obj:
            write(repr(obj))
            return
        if self._sort_dicts:
            values = _ordered(obj, isinstance(obj, _ITEM_VIEWS))
        else:
            values = list(obj)
        self._values(write, type(obj).__name__ + '([', values, '])', col,
                     tail, context, level, item_col=col + self._indent)

    def _lay_mapping_view(self, obj, write, col, tail, context, level,
                          lead=0):
        prefix = type(obj).__name__ + '('
        if self._expand:
            self._call_around(write, prefix, obj._mapping, col, tail,
                              context, level, lead)
            return
        # classic layout: the mapping keeps the view's column and its width
        # check does not reserve room for the closing parenthesis
        write(prefix)
        self._emit(obj._mapping, write, col, tail, context, level)
        write(')')

    def _lay_deque(self, obj, write, col, tail, context, level, lead=0):
        if not obj:
            write(repr(obj))
            return
        opener = type(obj).__name__ + '(['
        if obj.maxlen is None:
            self._values(write, opener, obj, '])', col, tail, context, level)
            return
        limit = self._repr(obj.maxlen, context, level)
        if self._expand:
            closer = f'], maxlen={limit})'
        else:
            closer = ('],\n' + ' ' * (col + len(opener) - 1) +
                      f'maxlen={limit})')
        self._values(write, opener, obj, closer, col, tail, context, level,
                     last_tail=2)

    def _lay_chainmap(self, obj, write, col, tail, context, level, lead=0):
        maps = obj.maps
        if not maps or (len(maps) == 1 and not maps[0]):
            write(repr(obj))
            return
        self._values(write, type(obj).__name__ + '(', maps, ')', col, tail,
                     context, level, pad=False, packable=False)

    def _lay_namespace(self, obj, write, col, tail, context, level, lead=0):
        cls = type(obj)
        name = 'namespace' if cls is _types.SimpleNamespace else cls.__name__
        self._fields(write, name + '(', list(obj.__dict__.items()), ')', col,
                     tail, context, level)

    def _lay_dataclass(self, obj, write, col, tail, context, level, lead=0):
        from dataclasses import fields
        shown = [(f.name, getattr(obj, f.name)) for f in fields(obj) if f.repr]
        self._fields(write, type(obj).__name__ + '(', shown, ')', col, tail,
                     context, level)

    def _lay_template(self, obj, write, col, tail, context, level, lead=0):
        shown = [('strings', obj.strings),
                 ('interpolations', obj.interpolations)]
        self._fields(write, type(obj).__name__ + '(', shown, ')', col, tail,
                     context, level)

    def _lay_interpolation(self, obj, write, col, tail, context, level,
                           lead=0):
        opener = type(obj).__name__ + '('
        if self._expand:
            shown = [('value', obj.value), ('expression', obj.expression),
                     ('conversion', obj.conversion),
                     ('format_spec', obj.format_spec)]
            self._fields(write, opener, shown, ')', col, tail, context, level)
            return
        shown = [obj.value, obj.expression, obj.conversion, obj.format_spec]
        self._values(write, opener, shown, ')', col, tail, context, level,
                     pad=False)

    def _lay_str(self, obj, write, col, tail, context, level, lead=0):
        if not obj:
            write(repr(obj))
            return
        top = level == 1
        if self._expand:
            if top:
                room, tail = self._width - col - self._indent, 0
            else:
                room = self._width - col
        else:
            if top:
                col += 1
                tail += 1
            room = self._width - col
        chunks = _text_chunks(obj, room, tail)
        if len(chunks) == 1:
            write(chunks[0])
        elif top:
            self._chunks(write, chunks, '(', ')', col - (not self._expand))
        else:
            self._chunks(write, chunks, '', '', col)

    def _lay_bytes(self, obj, write, col, tail, context, level, lead=0):
        if len(obj) <= 4:
            write(repr(obj))
            return
        top = level == 1
        if self._expand:
            if top:
                chunks = _bytes_chunks(obj, self._width - col - self._indent,
                                       0)
                self._chunks(write, chunks, '(', ')', col)
            else:
                self._chunks(write, _bytes_chunks(obj, self._width - col,
                                                  tail), '', '', col)
            return
        if top:
            chunks = _bytes_chunks(obj, self._width - col - 1, tail + 1)
            self._chunks(write, chunks, '(', ')', col)
        else:
            self._chunks(write, _bytes_chunks(obj, self._width - col, tail),
                         '', '', col)

    def _lay_bytearray(self, obj, write, col, tail, context, level, lead=0):
        data = bytes(obj)
        if self._expand:
            if len(data) <= 4:
                write('bytearray(' + repr(data) + ')')
                return
            chunks = _bytes_chunks(data, self._width - col - self._indent, 0)
            self._chunks(write, chunks, 'bytearray(', ')', col)
            return
        write('bytearray(')
        self._lay_bytes(data, write, col + 10, tail + 1, context, level + 1)
        write(')')

    def _lay_user_wrapper(self, obj, write, col, tail, context, level,
                          lead=0):
        # UserDict, UserList and UserString look like what they wrap.
        self._emit(obj.data, write, col, tail, context, level - 1, lead)


def _stock_hooks(printer):
    """True when printer uses PrettyPrinter's own format() and
    _safe_repr(), so that shortcuts bypassing them are invisible."""
    cls = type(printer)
    return cls.format is _STOCK_FORMAT and cls._safe_repr is _STOCK_SAFE_REPR


_STOCK_FORMAT = PrettyPrinter.format
_STOCK_SAFE_REPR = PrettyPrinter._safe_repr


_LAYOUTS = {
    dict: PrettyPrinter._lay_dict,
    frozendict: PrettyPrinter._lay_frozendict,
    _collections.OrderedDict: PrettyPrinter._lay_ordered_dict,
    _collections.defaultdict: PrettyPrinter._lay_defaultdict,
    _collections.Counter: PrettyPrinter._lay_counter,
    _collections.ChainMap: PrettyPrinter._lay_chainmap,
    _collections.deque: PrettyPrinter._lay_deque,
    _collections.UserDict: PrettyPrinter._lay_user_wrapper,
    _collections.UserList: PrettyPrinter._lay_user_wrapper,
    _collections.UserString: PrettyPrinter._lay_user_wrapper,
    _types.MappingProxyType: PrettyPrinter._lay_mappingproxy,
    _types.SimpleNamespace: PrettyPrinter._lay_namespace,
    list: PrettyPrinter._lay_list,
    tuple: PrettyPrinter._lay_tuple,
    set: PrettyPrinter._lay_set,
    frozenset: PrettyPrinter._lay_set,
    str: PrettyPrinter._lay_str,
    bytes: PrettyPrinter._lay_bytes,
    bytearray: PrettyPrinter._lay_bytearray,
    _dict_keys: PrettyPrinter._lay_dict_view,
    _dict_values: PrettyPrinter._lay_dict_view,
    _dict_items: PrettyPrinter._lay_dict_view,
    _odict_keys: PrettyPrinter._lay_dict_view,
    _odict_values: PrettyPrinter._lay_dict_view,
    _odict_items: PrettyPrinter._lay_dict_view,
    _MappingView: PrettyPrinter._lay_mapping_view,
    type(t''): PrettyPrinter._lay_template,
    type(t'{0}'.interpolations[0]): PrettyPrinter._lay_interpolation,
}

_DEFAULT = PrettyPrinter()
