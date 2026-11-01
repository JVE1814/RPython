"""Tools for inspecting pickles.

genops(pickle)
    Iterate over (opcode, argument, position) triples of a pickle.
dis(pickle, out=None, memo=None, indentlevel=4, annotate=0)
    Print a symbolic disassembly of a pickle, checking its stack discipline.
optimize(pickle)
    Drop PUT opcodes whose memo slot is never read, renumbering the rest.

The opcode tables below (``opcodes``, ``code2op``) describe every opcode of
pickle protocols 0 to 5: its argument encoding (an ArgumentDescriptor), its
effect on the unpickling stack (lists of StackObject) and the protocol that
introduced it.
"""

import codecs
import io
import pickle
import sys
from struct import unpack as _unpack

__all__ = ['dis', 'genops', 'optimize']

bytes_types = pickle.bytes_types

# Special values of ArgumentDescriptor.n for variable-length arguments.
UP_TO_NEWLINE = -1
TAKEN_FROM_ARGUMENT1 = -2     # length in a 1-byte unsigned prefix
TAKEN_FROM_ARGUMENT4 = -3     # length in a 4-byte signed prefix
TAKEN_FROM_ARGUMENT4U = -4    # length in a 4-byte unsigned prefix
TAKEN_FROM_ARGUMENT8U = -5    # length in an 8-byte unsigned prefix

_VARIABLE = (UP_TO_NEWLINE, TAKEN_FROM_ARGUMENT1, TAKEN_FROM_ARGUMENT4,
             TAKEN_FROM_ARGUMENT4U, TAKEN_FROM_ARGUMENT8U)


class ArgumentDescriptor:
    """How an opcode argument is encoded in the byte stream."""
    __slots__ = ('name', 'n', 'reader', 'doc')

    def __init__(self, name, n, reader, doc):
        if not isinstance(name, str) or not isinstance(doc, str):
            raise TypeError("name and doc must be strings")
        if not isinstance(n, int) or (n < 0 and n not in _VARIABLE):
            raise ValueError("bad argument size %r" % (n,))
        self.name = name
        self.n = n
        self.reader = reader
        self.doc = doc

    def __repr__(self):
        return self.name


# ---------------------------------------------------------------------------
# Argument readers.  Each takes a binary file and returns the decoded value,
# raising ValueError when the stream does not hold a complete argument.

def _fixed(f, size, what):
    data = f.read(size)
    if len(data) != size:
        raise ValueError("not enough data in stream to read " + what)
    return data


def read_uint1(f):
    r"""
    >>> read_uint1(io.BytesIO(b'\xff'))
    255
    """
    return _fixed(f, 1, "uint1")[0]

def read_uint2(f):
    r"""
    >>> read_uint2(io.BytesIO(b'\xff\xff'))
    65535
    """
    return _unpack("<H", _fixed(f, 2, "uint2"))[0]

def read_int4(f):
    r"""
    >>> read_int4(io.BytesIO(b'\x00\x00\x00\x80')) == -(2**31)
    True
    """
    return _unpack("<i", _fixed(f, 4, "int4"))[0]

def read_uint4(f):
    r"""
    >>> read_uint4(io.BytesIO(b'\x00\x00\x00\x80')) == 2**31
    True
    """
    return _unpack("<I", _fixed(f, 4, "uint4"))[0]

def read_uint8(f):
    r"""
    >>> read_uint8(io.BytesIO(b'\xff' * 8)) == 2**64 - 1
    True
    """
    return _unpack("<Q", _fixed(f, 8, "uint8"))[0]


def read_stringnl(f, decode=True, stripquotes=True, *, encoding='latin-1'):
    r"""Read a newline-terminated argument.

    With *stripquotes* the text must be enclosed in matching quotes; with
    *decode* backslash escapes are undone and the result is a str.

    >>> read_stringnl(io.BytesIO(b"'abcd'\nefg\n"))
    'abcd'
    >>> read_stringnl(io.BytesIO(b"\n"), stripquotes=False)
    ''
    """
    line = f.readline()
    if not line.endswith(b'\n'):
        raise ValueError("no newline found when trying to read stringnl")
    data = line[:-1]
    if stripquotes:
        quote = data[:1]
        if quote not in (b'"', b"'"):
            raise ValueError("no string quotes around %r" % data)
        if len(data) < 2 or not data.endswith(quote):
            raise ValueError("string quote %r not found at both ends of %r"
                             % (quote, data))
        data = data[1:-1]
    if decode:
        data = codecs.escape_decode(data)[0].decode(encoding)
    return data

def read_stringnl_noescape(f):
    return read_stringnl(f, stripquotes=False, encoding='utf-8')

def read_stringnl_noescape_pair(f):
    r"""
    >>> read_stringnl_noescape_pair(io.BytesIO(b"Queue\nEmpty\njunk"))
    'Queue Empty'
    """
    first = read_stringnl_noescape(f)
    second = read_stringnl_noescape(f)
    return "%s %s" % (first, second)


def _counted(f, n, kind):
    data = f.read(n)
    if len(data) != n:
        raise ValueError("expected %d bytes in a %s, but only %d remain"
                         % (n, kind, len(data)))
    return data

def _checked_size(n, kind):
    if n > sys.maxsize:
        raise ValueError("%s byte count > sys.maxsize: %d" % (kind, n))
    return n

def read_string1(f):
    r"""
    >>> read_string1(io.BytesIO(b"\x03abcdef"))
    'abc'
    """
    return _counted(f, read_uint1(f), "string1").decode("latin-1")

def read_string4(f):
    r"""
    >>> read_string4(io.BytesIO(b"\x03\x00\x00\x00abcdef"))
    'abc'
    """
    n = read_int4(f)
    if n < 0:
        raise ValueError("string4 byte count < 0: %d" % n)
    return _counted(f, n, "string4").decode("latin-1")

def read_bytes1(f):
    r"""
    >>> read_bytes1(io.BytesIO(b"\x03abcdef"))
    b'abc'
    """
    return _counted(f, read_uint1(f), "bytes1")

def read_bytes4(f):
    r"""
    >>> read_bytes4(io.BytesIO(b"\x03\x00\x00\x00abcdef"))
    b'abc'
    """
    return _counted(f, _checked_size(read_uint4(f), "bytes4"), "bytes4")

def read_bytes8(f):
    r"""
    >>> read_bytes8(io.BytesIO(b"\x03" + b"\x00" * 7 + b"abcdef"))
    b'abc'
    """
    return _counted(f, _checked_size(read_uint8(f), "bytes8"), "bytes8")

def read_bytearray8(f):
    r"""
    >>> read_bytearray8(io.BytesIO(b"\x03" + b"\x00" * 7 + b"abcdef"))
    bytearray(b'abc')
    """
    n = _checked_size(read_uint8(f), "bytearray8")
    return bytearray(_counted(f, n, "bytearray8"))

def read_unicodestringnl(f):
    r"""
    >>> read_unicodestringnl(io.BytesIO(b"abc\\uabcd\njunk")) == 'abcꯍ'
    True
    """
    line = f.readline()
    if not line.endswith(b'\n'):
        raise ValueError("no newline found when trying to read "
                         "unicodestringnl")
    return str(line[:-1], 'raw-unicode-escape')

def _utf8(data):
    return str(data, 'utf-8', 'surrogatepass')

def read_unicodestring1(f):
    r"""
    >>> read_unicodestring1(io.BytesIO(b"\x02\xc3\xa9junk"))
    'é'
    """
    return _utf8(_counted(f, read_uint1(f), "unicodestring1"))

def read_unicodestring4(f):
    r"""
    >>> read_unicodestring4(io.BytesIO(b"\x02\x00\x00\x00\xc3\xa9junk"))
    'é'
    """
    n = _checked_size(read_uint4(f), "unicodestring4")
    return _utf8(_counted(f, n, "unicodestring4"))

def read_unicodestring8(f):
    r"""
    >>> read_unicodestring8(io.BytesIO(b"\x02" + b"\x00" * 7 + b"\xc3\xa9"))
    'é'
    """
    n = _checked_size(read_uint8(f), "unicodestring8")
    return _utf8(_counted(f, n, "unicodestring8"))

def read_decimalnl_short(f):
    r"""
    >>> read_decimalnl_short(io.BytesIO(b"1234\n56"))
    1234
    >>> read_decimalnl_short(io.BytesIO(b"01\n"))
    True
    """
    text = read_stringnl(f, decode=False, stripquotes=False)
    if text == b"00":
        return False
    if text == b"01":
        return True
    return int(text)

def read_decimalnl_long(f):
    r"""
    >>> read_decimalnl_long(io.BytesIO(b"1234L\n56"))
    1234
    """
    text = read_stringnl(f, decode=False, stripquotes=False)
    if text.endswith(b'L'):
        text = text[:-1]
    return int(text)

def read_floatnl(f):
    r"""
    >>> read_floatnl(io.BytesIO(b"-1.25\n6"))
    -1.25
    """
    return float(read_stringnl(f, decode=False, stripquotes=False))

def read_float8(f):
    r"""
    >>> read_float8(io.BytesIO(b'\xbf\xf4\x00\x00\x00\x00\x00\x00'))
    -1.25
    """
    return _unpack(">d", _fixed(f, 8, "float8"))[0]

def read_long1(f):
    r"""
    >>> read_long1(io.BytesIO(b"\x02\x00\x80"))
    -32768
    """
    n = read_uint1(f)
    data = f.read(n)
    if len(data) != n:
        raise ValueError("not enough data in stream to read long1")
    return pickle.decode_long(data)

def read_long4(f):
    r"""
    >>> read_long4(io.BytesIO(b"\x02\x00\x00\x00\xff\x00"))
    255
    """
    n = read_int4(f)
    if n < 0:
        raise ValueError("long4 byte count < 0: %d" % n)
    data = f.read(n)
    if len(data) != n:
        raise ValueError("not enough data in stream to read long4")
    return pickle.decode_long(data)


A = ArgumentDescriptor
uint1 = A('uint1', 1, read_uint1, "One-byte unsigned integer.")
uint2 = A('uint2', 2, read_uint2, "Two-byte unsigned integer, little-endian.")
int4 = A('int4', 4, read_int4,
         "Four-byte signed integer, little-endian, two's complement.")
uint4 = A('uint4', 4, read_uint4, "Four-byte unsigned integer, little-endian.")
uint8 = A('uint8', 8, read_uint8, "Eight-byte unsigned integer, little-endian.")
stringnl = A('stringnl', UP_TO_NEWLINE, read_stringnl,
             "A quoted, backslash-escaped string ending at a newline.")
stringnl_noescape = A('stringnl_noescape', UP_TO_NEWLINE,
                      read_stringnl_noescape,
                      "An unquoted UTF-8 string ending at a newline.")
stringnl_noescape_pair = A('stringnl_noescape_pair', UP_TO_NEWLINE,
                           read_stringnl_noescape_pair,
                           "Two newline-terminated strings, returned joined "
                           "by a blank.")
string1 = A('string1', TAKEN_FROM_ARGUMENT1, read_string1,
            "A string with a one-byte length prefix.")
string4 = A('string4', TAKEN_FROM_ARGUMENT4, read_string4,
            "A string with a four-byte signed length prefix.")
bytes1 = A('bytes1', TAKEN_FROM_ARGUMENT1, read_bytes1,
           "Bytes with a one-byte length prefix.")
bytes4 = A('bytes4', TAKEN_FROM_ARGUMENT4U, read_bytes4,
           "Bytes with a four-byte unsigned length prefix.")
bytes8 = A('bytes8', TAKEN_FROM_ARGUMENT8U, read_bytes8,
           "Bytes with an eight-byte unsigned length prefix.")
bytearray8 = A('bytearray8', TAKEN_FROM_ARGUMENT8U, read_bytearray8,
               "A bytearray with an eight-byte unsigned length prefix.")
unicodestringnl = A('unicodestringnl', UP_TO_NEWLINE, read_unicodestringnl,
                    "A raw-unicode-escape encoded string ending at a "
                    "newline.")
unicodestring1 = A('unicodestring1', TAKEN_FROM_ARGUMENT1,
                   read_unicodestring1,
                   "UTF-8 text with a one-byte length prefix.")
unicodestring4 = A('unicodestring4', TAKEN_FROM_ARGUMENT4U,
                   read_unicodestring4,
                   "UTF-8 text with a four-byte length prefix.")
unicodestring8 = A('unicodestring8', TAKEN_FROM_ARGUMENT8U,
                   read_unicodestring8,
                   "UTF-8 text with an eight-byte length prefix.")
decimalnl_short = A('decimalnl_short', UP_TO_NEWLINE, read_decimalnl_short,
                    "A decimal integer literal ending at a newline "
                    "('00' and '01' stand for False and True).")
decimalnl_long = A('decimalnl_long', UP_TO_NEWLINE, read_decimalnl_long,
                   "A decimal integer literal with an optional trailing "
                   "'L', ending at a newline.")
floatnl = A('floatnl', UP_TO_NEWLINE, read_floatnl,
            "repr() of a float, ending at a newline.")
float8 = A('float8', 8, read_float8,
           "An IEEE-754 double, big-endian.")
long1 = A('long1', TAKEN_FROM_ARGUMENT1, read_long1,
          "Two's complement little-endian int with a one-byte length.")
long4 = A('long4', TAKEN_FROM_ARGUMENT4, read_long4,
          "Two's complement little-endian int with a four-byte length.")
del A


# ---------------------------------------------------------------------------
# Kinds of values living on the unpickling stack.

class StackObject:
    """A kind of object on the unpickling stack (for dis() emulation)."""
    __slots__ = ('name', 'obtype', 'doc')

    def __init__(self, name, obtype, doc):
        self.name = name
        self.obtype = obtype
        self.doc = doc

    def __repr__(self):
        return self.name


S = StackObject
pyint = pylong = S('int', int, "A Python int.")
pyinteger_or_bool = S('int_or_bool', (int, bool), "An int or a bool.")
pybool = S('bool', bool, "A bool.")
pyfloat = S('float', float, "A float.")
pybytes_or_str = pystring = S('bytes_or_str', (bytes, str),
                              "A bytes or str object.")
pybytes = S('bytes', bytes, "A bytes object.")
pybytearray = S('bytearray', bytearray, "A bytearray object.")
pyunicode = S('str', str, "A str object.")
pynone = S('None', type(None), "None.")
pytuple = S('tuple', tuple, "A tuple.")
pylist = S('list', list, "A list.")
pydict = S('dict', dict, "A dict.")
pyset = S('set', set, "A set.")
pyfrozenset = S('frozenset', set, "A frozenset.")
pybuffer = S('buffer', object, "A buffer-like object.")
anyobject = S('any', object, "An arbitrary object.")
markobject = S('mark', StackObject,
               "The marker pushed by MARK; opcodes taking a variable number "
               "of items consume everything above the topmost marker.")
stackslice = S('stackslice', StackObject,
               "All stack items above the topmost marker.")
del S


class OpcodeInfo:
    """Description of one pickle opcode."""
    __slots__ = ('name', 'code', 'arg', 'stack_before', 'stack_after',
                 'proto', 'doc')

    def __init__(self, name, code, arg, stack_before, stack_after, proto,
                 doc):
        if len(code) != 1:
            raise ValueError("opcode code must be one character")
        if not 0 <= proto <= pickle.HIGHEST_PROTOCOL:
            raise ValueError("bad protocol %r" % (proto,))
        self.name = name
        self.code = code
        self.arg = arg
        self.stack_before = stack_before
        self.stack_after = stack_after
        self.proto = proto
        self.doc = doc

    def __repr__(self):
        return '<OpcodeInfo %s>' % self.name


# name, code, argument, stack before, stack after, protocol, description
_any, _mark, _slice = anyobject, markobject, stackslice
_TABLE = [
    ('INT', 'I', decimalnl_short, [], [pyinteger_or_bool], 0,
     "Push an integer or bool.\n\nThe literal '01' means True and '00' "
     "False."),
    ('BININT', 'J', int4, [], [pyint], 1,
     "Push a four-byte signed integer."),
    ('BININT1', 'K', uint1, [], [pyint], 1,
     "Push a one-byte unsigned integer."),
    ('BININT2', 'M', uint2, [], [pyint], 1,
     "Push a two-byte unsigned integer."),
    ('LONG', 'L', decimalnl_long, [], [pyint], 0,
     "Push a long integer given in decimal."),
    ('LONG1', '\x8a', long1, [], [pyint], 2,
     "Push an int stored in at most 255 bytes."),
    ('LONG4', '\x8b', long4, [], [pyint], 2,
     "Push an int stored with a four-byte length."),
    ('STRING', 'S', stringnl, [], [pybytes_or_str], 0,
     "Push a Python 2 string given as a quoted literal."),
    ('BINSTRING', 'T', string4, [], [pybytes_or_str], 1,
     "Push a Python 2 string with a four-byte length."),
    ('SHORT_BINSTRING', 'U', string1, [], [pybytes_or_str], 1,
     "Push a Python 2 string with a one-byte length."),
    ('BINBYTES', 'B', bytes4, [], [pybytes], 3,
     "Push a bytes object with a four-byte length."),
    ('SHORT_BINBYTES', 'C', bytes1, [], [pybytes], 3,
     "Push a bytes object with a one-byte length."),
    ('BINBYTES8', '\x8e', bytes8, [], [pybytes], 4,
     "Push a bytes object with an eight-byte length."),
    ('BYTEARRAY8', '\x96', bytearray8, [], [pybytearray], 5,
     "Push a bytearray with an eight-byte length."),
    ('NEXT_BUFFER', '\x97', None, [], [pybuffer], 5,
     "Push the next out-of-band buffer."),
    ('READONLY_BUFFER', '\x98', None, [pybuffer], [pybuffer], 5,
     "Turn the buffer on the stack top into a read-only view."),
    ('NONE', 'N', None, [], [pynone], 0,
     "Push None on the stack."),
    ('NEWTRUE', '\x88', None, [], [pybool], 2,
     "Push True."),
    ('NEWFALSE', '\x89', None, [], [pybool], 2,
     "Push False."),
    ('UNICODE', 'V', unicodestringnl, [], [pyunicode], 0,
     "Push a str given in raw-unicode-escape form."),
    ('SHORT_BINUNICODE', '\x8c', unicodestring1, [], [pyunicode], 4,
     "Push a Python Unicode string object.\n\nUTF-8 data with a one-byte "
     "length."),
    ('BINUNICODE', 'X', unicodestring4, [], [pyunicode], 1,
     "Push a str stored as UTF-8 with a four-byte length."),
    ('BINUNICODE8', '\x8d', unicodestring8, [], [pyunicode], 4,
     "Push a str stored as UTF-8 with an eight-byte length."),
    ('FLOAT', 'F', floatnl, [], [pyfloat], 0,
     "Push a float given as decimal text."),
    ('BINFLOAT', 'G', float8, [], [pyfloat], 1,
     "Push a float stored as eight big-endian bytes."),
    ('EMPTY_LIST', ']', None, [], [pylist], 1,
     "Push a new empty list."),
    ('APPEND', 'a', None, [pylist, _any], [pylist], 0,
     "Append the stack top to the list below it."),
    ('APPENDS', 'e', None, [pylist, _mark, _slice], [pylist], 1,
     "Extend the list below the marker by the items above it."),
    ('LIST', 'l', None, [_mark, _slice], [pylist], 0,
     "Replace the items above the marker (and the marker) by a list."),
    ('EMPTY_TUPLE', ')', None, [], [pytuple], 1,
     "Push the empty tuple."),
    ('TUPLE', 't', None, [_mark, _slice], [pytuple], 0,
     "Build a tuple out of the topmost stack slice, after markobject."),
    ('TUPLE1', '\x85', None, [_any], [pytuple], 2,
     "Build a one-tuple out of the topmost item on the stack."),
    ('TUPLE2', '\x86', None, [_any, _any], [pytuple], 2,
     "Pack the two topmost items into a tuple."),
    ('TUPLE3', '\x87', None, [_any, _any, _any], [pytuple], 2,
     "Pack the three topmost items into a tuple."),
    ('EMPTY_DICT', '}', None, [], [pydict], 1,
     "Push a new empty dict."),
    ('DICT', 'd', None, [_mark, _slice], [pydict], 0,
     "Replace alternating keys and values above the marker by a dict."),
    ('SETITEM', 's', None, [pydict, _any, _any], [pydict], 0,
     "Store a key and value into the dict below them."),
    ('SETITEMS', 'u', None, [pydict, _mark, _slice], [pydict], 1,
     "Store the key/value pairs above the marker into the dict below it."),
    ('EMPTY_SET', '\x8f', None, [], [pyset], 4,
     "Push a new empty set."),
    ('ADDITEMS', '\x90', None, [pyset, _mark, _slice], [pyset], 4,
     "Add the items above the marker to the set below it."),
    ('FROZENSET', '\x91', None, [_mark, _slice], [pyfrozenset], 4,
     "Replace the items above the marker by a frozenset."),
    ('POP', '0', None, [_any], [], 0,
     "Discard the stack top."),
    ('DUP', '2', None, [_any], [_any, _any], 0,
     "Push the stack top a second time."),
    ('MARK', '(', None, [], [_mark], 0,
     "Push markobject onto the stack."),
    ('POP_MARK', '1', None, [_mark, _slice], [], 1,
     "Discard everything from the topmost marker upwards."),
    ('GET', 'g', decimalnl_short, [], [_any], 0,
     "Push a memo entry (decimal index)."),
    ('BINGET', 'h', uint1, [], [_any], 1,
     "Push a memo entry (one-byte index)."),
    ('LONG_BINGET', 'j', uint4, [], [_any], 1,
     "Push a memo entry (four-byte index)."),
    ('PUT', 'p', decimalnl_short, [], [], 0,
     "Store the stack top into the memo (decimal index)."),
    ('BINPUT', 'q', uint1, [], [], 1,
     "Store the stack top into the memo (one-byte index)."),
    ('LONG_BINPUT', 'r', uint4, [], [], 1,
     "Store the stack top into the memo (four-byte index)."),
    ('MEMOIZE', '\x94', None, [_any], [_any], 4,
     "Store the stack top into the memo.  The stack is not popped.\n\n"
     "The index is the number of entries already in the memo."),
    ('EXT1', '\x82', uint1, [], [_any], 2,
     "Push an object from the extension registry (one-byte code)."),
    ('EXT2', '\x83', uint2, [], [_any], 2,
     "Push an object from the extension registry (two-byte code)."),
    ('EXT4', '\x84', int4, [], [_any], 2,
     "Push an object from the extension registry (four-byte code)."),
    ('GLOBAL', 'c', stringnl_noescape_pair, [], [_any], 0,
     "Push the object named by a module and a qualified name."),
    ('STACK_GLOBAL', '\x93', None, [pyunicode, pyunicode], [_any], 4,
     "Push the object named by the module and name strings on the stack."),
    ('REDUCE', 'R', None, [_any, _any], [_any], 0,
     "Call the callable below the stack top with the argument tuple on "
     "top."),
    ('BUILD', 'b', None, [_any, _any], [_any], 0,
     "Apply the state on the stack top to the object below it."),
    ('INST', 'i', stringnl_noescape_pair, [_mark, _slice], [_any], 0,
     "Instantiate a named class with the arguments above the marker."),
    ('OBJ', 'o', None, [_mark, _any, _slice], [_any], 1,
     "Instantiate the class just above the marker with the remaining "
     "items."),
    ('NEWOBJ', '\x81', None, [_any, _any], [_any], 2,
     "Push cls.__new__(cls, *args)."),
    ('NEWOBJ_EX', '\x92', None, [_any, _any, _any], [_any], 4,
     "Push cls.__new__(cls, *args, **kwargs)."),
    ('PROTO', '\x80', uint1, [], [], 2,
     "Protocol version indicator."),
    ('STOP', '.', None, [_any], [], 0,
     "Stop the unpickling machine.\n\nThe stack top is the result."),
    ('FRAME', '\x95', uint8, [], [], 4,
     "Indicate the beginning of a new frame.\n\nThe argument is the frame "
     "length."),
    ('PERSID', 'P', stringnl_noescape, [], [_any], 0,
     "Push persistent_load(id) for a textual persistent id."),
    ('BINPERSID', 'Q', None, [_any], [_any], 1,
     "Replace the stack top by persistent_load(stack top)."),
]
del _any, _mark, _slice

opcodes = [OpcodeInfo(*row) for row in _TABLE]
del _TABLE

code2op = {}
_names = set()
for _op in opcodes:
    if _op.code in code2op or _op.name in _names:
        raise ValueError("duplicate opcode %s" % _op.name)
    code2op[_op.code] = _op
    _names.add(_op.name)
    # The table must agree with the constants used by pickle itself.
    if getattr(pickle, _op.name) != _op.code.encode('latin-1'):
        raise ValueError("opcode %s disagrees with pickle.py" % _op.name)
del _op, _names


# ---------------------------------------------------------------------------
# Iteration over opcodes

def _genops(data, yield_end_pos=False):
    if isinstance(data, bytes_types):
        data = io.BytesIO(data)
    tell = getattr(data, 'tell', None)
    while True:
        pos = tell() if tell is not None else None
        code = data.read(1)
        info = code2op.get(code.decode('latin-1'))
        if info is None:
            if not code:
                raise ValueError("pickle exhausted before seeing STOP")
            where = "<unknown>" if pos is None else pos
            raise ValueError("at position %s, opcode %r unknown"
                             % (where, code))
        arg = None if info.arg is None else info.arg.reader(data)
        if yield_end_pos:
            yield info, arg, pos, (tell() if tell is not None else None)
        else:
            yield info, arg, pos
        if info.name == 'STOP':
            return


def genops(pickle):
    """Yield (opcode, arg, pos) for each opcode of *pickle* up to STOP.

    *pickle* is bytes or a binary file; *pos* is the offset of the opcode
    (taken from the file's tell(), or None if it has none).
    """
    return _genops(pickle)


# ---------------------------------------------------------------------------
# Optimizer

def optimize(p):
    'Optimize a pickle string by removing unused PUT opcodes'
    pieces = []          # ('put', old index) | ('get', old index) | (start, end)
    read_slots = set()
    proto = 0
    header = b''
    memoized = 0
    for info, arg, pos, end in _genops(p, yield_end_pos=True):
        name = info.name
        if name in ('PUT', 'BINPUT', 'LONG_BINPUT'):
            pieces.append(('put', arg))
            memoized += 1
        elif name == 'MEMOIZE':
            pieces.append(('put', memoized))
            memoized += 1
        elif name == 'FRAME':
            continue
        elif name in ('GET', 'BINGET', 'LONG_BINGET'):
            proto = max(proto, info.proto)
            read_slots.add(arg)
            pieces.append(('get', arg))
        elif name == 'PROTO':
            proto = max(proto, arg)
            if pos == 0:
                header = p[pos:end]
            else:
                pieces.append((pos, end))
        else:
            pieces.append((pos, end))

    out = io.BytesIO()
    out.write(header)
    writer = pickle._Pickler(out, proto)
    framer = writer.framer
    if proto >= 4:
        framer.start_framing()
    renumber = {}
    for kind, value in pieces:
        unframed = False
        if kind == 'put':
            if value not in read_slots:
                continue
            renumber[value] = len(renumber)
            chunk = writer.put(renumber[value])
        elif kind == 'get':
            chunk = writer.get(renumber[value])
        else:
            chunk = p[kind:value]
            unframed = len(chunk) > framer._FRAME_SIZE_TARGET
        framer.commit_frame(force=unframed)
        if unframed:
            framer.file_write(chunk)
        else:
            writer.write(chunk)
    framer.end_framing()
    return out.getvalue()


# ---------------------------------------------------------------------------
# Disassembler

def dis(pickle, out=None, memo=None, indentlevel=4, annotate=0):
    """Print a symbolic disassembly of *pickle* to *out* (default stdout).

    The disassembly also emulates the unpickling stack and memo and raises
    ValueError on inconsistencies (popping a missing MARK, reading an
    unset memo slot, a non-empty stack at STOP, ...).  *memo* may be shared
    between calls for pickles written by one Pickler.  *indentlevel* is the
    indentation added per open MARK; a nonzero *annotate* appends the
    opcode's one-line description, aligned near that column.
    """
    stack = []
    if memo is None:
        memo = {}
    marks = []               # positions of the open MARK opcodes
    highest = -1
    pad = ' ' * indentlevel
    column = annotate

    for info, arg, pos in genops(pickle):
        if pos is not None:
            print("%5d:" % pos, end=' ', file=out)
        text = "%-4s %s%s" % (repr(info.code)[1:-1], pad * len(marks),
                              info.name)
        highest = max(highest, info.proto)
        before, after = info.stack_before, info.stack_after
        npop = len(before)
        problem = None
        note = None

        takes_mark = markobject in before or (
            info.name == 'POP' and stack and stack[-1] is markobject)
        if takes_mark:
            if not marks:
                problem = "no MARK exists on stack"
            else:
                where = marks.pop()
                note = ("(MARK at unknown opcode offset)" if where is None
                        else "(MARK at %d)" % where)
                while stack[-1] is not markobject:
                    stack.pop()
                stack.pop()
                npop = before.index(markobject) if markobject in before else 0

        if info.name in ('PUT', 'BINPUT', 'LONG_BINPUT', 'MEMOIZE'):
            slot = arg
            if info.name == 'MEMOIZE':
                slot = len(memo)
                note = "(as %d)" % slot
            if not stack:
                problem = "stack is empty -- can't store into memo"
            elif stack[-1] is markobject:
                problem = "can't store markobject in the memo"
            else:
                memo[slot] = stack[-1]
        elif info.name in ('GET', 'BINGET', 'LONG_BINGET'):
            if arg in memo:
                after = [memo[arg]]
            else:
                problem = "memo key %r has never been stored into" % (arg,)

        if arg is not None or note:
            text += ' ' * (10 - len(info.name))
            if arg is not None:
                quoted = (ascii(arg) if info.name in
                          ('STRING', 'BINSTRING', 'SHORT_BINSTRING')
                          else repr(arg))
                text += ' ' + quoted
            if note:
                text += ' ' + note
        if annotate:
            text += ' ' * (column - len(text))
            column = len(text) if len(text) <= 50 else annotate
            text += ' ' + info.doc.split('\n', 1)[0]
        print(text, file=out)

        if problem:
            raise ValueError(problem)
        if len(stack) < npop:
            raise ValueError("tries to pop %d items from stack with only %d "
                             "items" % (npop, len(stack)))
        if npop:
            del stack[-npop:]
        if markobject in after:
            marks.append(pos)
        stack.extend(after)

    print("highest protocol among opcodes =", highest, file=out)
    if stack:
        raise ValueError("stack not empty after STOP: %r" % stack)


def _main(args=None):
    import argparse
    parser = argparse.ArgumentParser(
        description='disassemble one or more pickle files', color=True)
    parser.add_argument('pickle_file', nargs='+', help='the pickle file')
    parser.add_argument('-o', '--output',
                        help='the file where the output should be written')
    parser.add_argument('-m', '--memo', action='store_true',
                        help='preserve memo between disassemblies')
    parser.add_argument('-l', '--indentlevel', default=4, type=int,
                        help='the number of blanks by which to indent a '
                             'new MARK level')
    parser.add_argument('-a', '--annotate', action='store_true',
                        help='annotate each line with a short opcode '
                             'description')
    parser.add_argument('-p', '--preamble', default="==> {name} <==",
                        help='if more than one pickle file is specified, '
                             'print this before each disassembly')
    options = parser.parse_args(args)
    memo = {} if options.memo else None
    annotate = 30 if options.annotate else 0
    out = sys.stdout if options.output is None else open(options.output, 'w')
    try:
        several = len(options.pickle_file) > 1
        for path in options.pickle_file:
            if several:
                label = '<stdin>' if path == '-' else path
                out.write(options.preamble.format(name=label) + '\n')
            if path == '-':
                dis(sys.stdin.buffer, out, memo, options.indentlevel,
                    annotate)
            else:
                with open(path, 'rb') as f:
                    dis(f, out, memo, options.indentlevel, annotate)
    finally:
        if out is not sys.stdout:
            out.close()


if __name__ == "__main__":
    _main()
