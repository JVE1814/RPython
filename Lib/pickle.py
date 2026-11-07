"""Serialize and deserialize Python object graphs ("pickling").

The main entry points are dump()/dumps() and load()/loads(), plus the
Pickler and Unpickler classes for finer control.  The fast implementations
come from the _pickle accelerator when it is available; _Pickler and
_Unpickler are the pure-Python implementations, kept for reference and as a
fallback.  See pickletools for a description of the opcodes.
"""

from types import FunctionType
from copyreg import dispatch_table
from copyreg import _extension_registry, _inverted_registry, _extension_cache
from itertools import islice
from functools import partial
import sys
from sys import maxsize
from struct import pack, unpack
import io
import codecs
import _compat_pickle

__all__ = ["PickleError", "PicklingError", "UnpicklingError", "Pickler",
           "Unpickler", "dump", "dumps", "load", "loads"]

try:
    from _pickle import PickleBuffer
except ImportError:
    _HAVE_PICKLE_BUFFER = False
else:
    __all__.append("PickleBuffer")
    _HAVE_PICKLE_BUFFER = True

bytes_types = (bytes, bytearray)

# Informational only.
format_version = "5.0"
compatible_formats = ["1.0", "1.1", "1.2", "1.3", "2.0", "3.0", "4.0", "5.0"]

HIGHEST_PROTOCOL = 5
DEFAULT_PROTOCOL = 5


class PickleError(Exception):
    """Base class of the pickling exceptions."""


class PicklingError(PickleError):
    """Raised when an object cannot be pickled."""


class UnpicklingError(PickleError):
    """Raised when pickle data is malformed or cannot be loaded.

    Other exceptions (EOFError, AttributeError, ImportError, IndexError,
    ...) can also escape from the unpickler.
    """


class _Stop(Exception):
    # Raised by the STOP opcode handler to end the load loop.
    def __init__(self, value):
        self.value = value


# ---------------------------------------------------------------------------
# Opcodes.  The byte values are fixed by the pickle format.

MARK = b'('
STOP = b'.'
POP = b'0'
POP_MARK = b'1'
DUP = b'2'
FLOAT = b'F'
INT = b'I'
BININT = b'J'
BININT1 = b'K'
LONG = b'L'
BININT2 = b'M'
NONE = b'N'
PERSID = b'P'
BINPERSID = b'Q'
REDUCE = b'R'
STRING = b'S'
BINSTRING = b'T'
SHORT_BINSTRING = b'U'
UNICODE = b'V'
BINUNICODE = b'X'
APPEND = b'a'
BUILD = b'b'
GLOBAL = b'c'
DICT = b'd'
EMPTY_DICT = b'}'
APPENDS = b'e'
GET = b'g'
BINGET = b'h'
INST = b'i'
LONG_BINGET = b'j'
LIST = b'l'
EMPTY_LIST = b']'
OBJ = b'o'
PUT = b'p'
BINPUT = b'q'
LONG_BINPUT = b'r'
SETITEM = b's'
TUPLE = b't'
EMPTY_TUPLE = b')'
SETITEMS = b'u'
BINFLOAT = b'G'

# Not opcodes: the complete protocol 0/1 spellings of the booleans.
TRUE = b'I01\n'
FALSE = b'I00\n'

# protocol 2
PROTO = b'\x80'
NEWOBJ = b'\x81'
EXT1 = b'\x82'
EXT2 = b'\x83'
EXT4 = b'\x84'
TUPLE1 = b'\x85'
TUPLE2 = b'\x86'
TUPLE3 = b'\x87'
NEWTRUE = b'\x88'
NEWFALSE = b'\x89'
LONG1 = b'\x8a'
LONG4 = b'\x8b'

# protocol 3
BINBYTES = b'B'
SHORT_BINBYTES = b'C'

# protocol 4
SHORT_BINUNICODE = b'\x8c'
BINUNICODE8 = b'\x8d'
BINBYTES8 = b'\x8e'
EMPTY_SET = b'\x8f'
ADDITEMS = b'\x90'
FROZENSET = b'\x91'
NEWOBJ_EX = b'\x92'
STACK_GLOBAL = b'\x93'
MEMOIZE = b'\x94'
FRAME = b'\x95'

# protocol 5
BYTEARRAY8 = b'\x96'
NEXT_BUFFER = b'\x97'
READONLY_BUFFER = b'\x98'

__all__.extend(_n for _n in list(globals())
               if _n.isupper() and not _n.startswith('_'))

_SMALL_TUPLE_OPS = (EMPTY_TUPLE, TUPLE1, TUPLE2, TUPLE3)
_tuplesize2code = list(_SMALL_TUPLE_OPS)

_MISSING = object()

# Length-prefixed reads above this size are performed in pieces so that a
# corrupt length field cannot force a huge allocation.
_READ_PIECE = 1 << 20


def _T(obj):
    """Type label of *obj* used in "when serializing ..." notes."""
    tp = type(obj)
    mod = tp.__module__
    if mod is None or mod == 'builtins' or mod == '__main__':
        return tp.__qualname__
    return mod + '.' + tp.__qualname__


def _noted(note):
    """Context manager adding *note* to any exception passing through."""
    return _Note(note)


class _Note:
    __slots__ = ('text',)

    def __init__(self, text):
        self.text = text

    def __enter__(self):
        return self

    def __exit__(self, tp, exc, tb):
        if exc is not None:
            exc.add_note(self.text)
        return False


# ---------------------------------------------------------------------------
# Long integer helpers

def encode_long(x):
    r"""Two's complement little-endian encoding of the int *x*.

    Zero encodes to b''.

    >>> encode_long(0)
    b''
    >>> encode_long(255)
    b'\xff\x00'
    >>> encode_long(-256)
    b'\x00\xff'
    >>> encode_long(-128)
    b'\x80'
    """
    if not x:
        return b''
    size = (x.bit_length() >> 3) + 1
    raw = x.to_bytes(size, 'little', signed=True)
    # A negative number may have got one redundant sign byte.
    if x < 0 and size > 1 and raw[-1] == 0xff and raw[-2] >= 0x80:
        raw = raw[:-1]
    return raw


def decode_long(data):
    r"""Inverse of encode_long().

    >>> decode_long(b'')
    0
    >>> decode_long(b'\xff\x00')
    255
    >>> decode_long(b'\x00\x80')
    -32768
    """
    return int.from_bytes(data, 'little', signed=True)


# ---------------------------------------------------------------------------
# Name lookup

def _private_alias(owner, name):
    """The mangled spelling of a private *name* defined in class *owner*."""
    if (isinstance(owner, type) and name.startswith('__')
            and not name.endswith('__')):
        base = owner.__name__.lstrip('_')
        if base:
            return '_' + base + name
    return name


def _getattribute(obj, dotted_path):
    """Follow *dotted_path* (a list of names) starting at *obj*.

    Private names of classes are also tried in their mangled form.
    """
    for part in dotted_path:
        try:
            obj = getattr(obj, part)
        except AttributeError as exc:
            alias = _private_alias(obj, part)
            if alias == part:
                raise
            try:
                obj = getattr(obj, alias)
            except AttributeError:
                raise exc from None
    return obj


def _concrete_path(root, dotted_path):
    """*dotted_path* with each component replaced by the attribute name that
    actually resolves it from *root* (mangled for private class members)."""
    result = []
    cur = root
    for part in dotted_path:
        if not hasattr(cur, part):
            part = _private_alias(cur, part)
        cur = getattr(cur, part)
        result.append(part)
    return result


def whichmodule(obj, name):
    """Return the name of the module through which *obj* is reachable as
    *name*, raising PicklingError when it cannot be found."""
    path = name.split('.')
    if '<locals>' in path:
        raise PicklingError(f"Can't pickle local object {obj!r}")
    module_name = getattr(obj, '__module__', None)
    if module_name is None:
        # Search the loaded modules (a snapshot: lookups may import more).
        for candidate, module in list(sys.modules.items()):
            if module is None or candidate in ('__main__', '__mp_main__'):
                continue
            try:
                found = _getattribute(module, path)
            except AttributeError:
                continue
            if found is obj:
                return candidate
        module_name = '__main__'
    try:
        __import__(module_name, level=0)
        module = sys.modules[module_name]
    except (ImportError, ValueError, KeyError) as exc:
        raise PicklingError(f"Can't pickle {obj!r}: {exc!s}")
    try:
        found = _getattribute(module, path)
    except AttributeError:
        raise PicklingError(f"Can't pickle {obj!r}: "
                            f"it's not found as {module_name}.{name}")
    if found is not obj:
        raise PicklingError(f"Can't pickle {obj!r}: it's not the same "
                            f"object as {module_name}.{name}")
    return module_name


# ---------------------------------------------------------------------------
# Output side

class _Framer:
    """Accumulates protocol 4+ output into frames of about
    _FRAME_SIZE_TARGET bytes, each preceded by a FRAME opcode."""

    _FRAME_SIZE_MIN = 4
    _FRAME_SIZE_TARGET = 64 * 1024

    def __init__(self, file_write):
        self.file_write = file_write
        self.current_frame = None

    def start_framing(self):
        self.current_frame = io.BytesIO()

    def end_framing(self):
        if self.current_frame is not None:
            if self.current_frame.tell():
                self.commit_frame(force=True)
            self.current_frame = None

    def commit_frame(self, force=False):
        buf = self.current_frame
        if buf is None:
            return
        size = buf.tell()
        if not force and size < self._FRAME_SIZE_TARGET:
            return
        payload = buf.getbuffer()
        if size >= self._FRAME_SIZE_MIN:
            self.file_write(FRAME + pack('<Q', size))
        # The view stays valid for the file object: a fresh buffer is used
        # for the next frame instead of reusing this one.
        self.file_write(payload)
        self.current_frame = io.BytesIO()

    def write(self, data):
        buf = self.current_frame
        if buf is None:
            return self.file_write(data)
        return buf.write(data)

    def write_large_bytes(self, header, payload):
        # Large payloads bypass framing and are not concatenated with
        # their header.
        if self.current_frame is not None:
            self.commit_frame(force=True)
        self.file_write(header)
        self.file_write(payload)


class _Pickler:

    _BATCHSIZE = 1000

    def __init__(self, file, protocol=None, *, fix_imports=True,
                 buffer_callback=None):
        """Write pickles of objects to the binary *file* (anything with a
        write() method accepting bytes).

        *protocol* selects the format version (0-5, default
        DEFAULT_PROTOCOL; negative means HIGHEST_PROTOCOL).  With
        *fix_imports* and protocol < 3, Python 3 names are translated to
        their Python 2 spelling.  *buffer_callback* (protocol 5 only)
        receives PickleBuffer objects; a false result makes the buffer
        out-of-band.
        """
        if protocol is None:
            protocol = DEFAULT_PROTOCOL
        if protocol < 0:
            protocol = HIGHEST_PROTOCOL
        elif protocol > HIGHEST_PROTOCOL:
            raise ValueError("pickle protocol must be <= %d" % HIGHEST_PROTOCOL)
        if buffer_callback is not None and protocol < 5:
            raise ValueError("buffer_callback needs protocol >= 5")
        try:
            self._file_write = file.write
        except AttributeError:
            raise TypeError("file must have a 'write' attribute")
        self._buffer_callback = buffer_callback
        self.framer = _Framer(self._file_write)
        self.write = self.framer.write
        self._write_large_bytes = self.framer.write_large_bytes
        self.memo = {}
        self.proto = int(protocol)
        self.bin = protocol >= 1
        self.fast = 0
        self.fix_imports = fix_imports and protocol < 3

    def clear_memo(self):
        """Forget all objects seen so far (they will be pickled again)."""
        self.memo.clear()

    def dump(self, obj):
        """Write the pickle of *obj* to the file."""
        if not hasattr(self, "_file_write"):
            raise PicklingError("Pickler.__init__() was not called by "
                                "%s.__init__()" % type(self).__name__)
        if self.proto >= 2:
            self.write(PROTO + bytes((self.proto,)))
        if self.proto >= 4:
            self.framer.start_framing()
        self.save(obj)
        self.write(STOP)
        self.framer.end_framing()

    def persistent_id(self, obj):
        """Hook: return a non-None id to pickle *obj* by reference."""
        return None

    # -- memo ---------------------------------------------------------------

    def memoize(self, obj):
        """Record *obj* in the memo and emit the matching PUT opcode."""
        if self.fast:
            return
        assert id(obj) not in self.memo
        index = len(self.memo)
        self.write(self.put(index))
        # Keep obj alive so its id cannot be recycled meanwhile.
        self.memo[id(obj)] = index, obj

    def put(self, idx):
        if self.proto >= 4:
            return MEMOIZE
        if not self.bin:
            return b'p%d\n' % idx
        if idx < 256:
            return BINPUT + bytes((idx,))
        return LONG_BINPUT + pack('<I', idx)

    def get(self, i):
        if not self.bin:
            return b'g%d\n' % i
        if i < 256:
            return BINGET + bytes((i,))
        return LONG_BINGET + pack('<I', i)

    # -- generic machinery ----------------------------------------------------

    def save(self, obj, save_persistent_id=True):
        self.framer.commit_frame()

        if save_persistent_id:
            pid = self.persistent_id(obj)
            if pid is not None:
                self.save_pers(pid)
                return

        entry = self.memo.get(id(obj))
        if entry is not None:
            self.write(self.get(entry[0]))
            return

        rv = NotImplemented
        override = getattr(self, "reducer_override", _MISSING)
        if override is not _MISSING:
            rv = override(obj)

        if rv is NotImplemented:
            tp = type(obj)
            handler = self.dispatch.get(tp)
            if handler is not None:
                handler(self, obj)
                return
            table = getattr(self, 'dispatch_table', dispatch_table)
            reducer = table.get(tp, _MISSING)
            if reducer is not _MISSING:
                rv = reducer(obj)
            elif issubclass(tp, type):
                # classes with a custom metaclass
                self.save_global(obj)
                return
            else:
                rv = self._call_reduce(obj, tp)

        if isinstance(rv, str):
            self.save_global(obj, rv)
            return

        with _noted(f'when serializing {_T(obj)} object'):
            if not isinstance(rv, tuple):
                raise PicklingError('__reduce__ must return a string or '
                                    f'tuple, not {_T(rv)}')
            if not 2 <= len(rv) <= 6:
                raise PicklingError("tuple returned by __reduce__ must "
                                    "contain 2 through 6 elements")
            self.save_reduce(*rv, obj=obj)

    def _call_reduce(self, obj, tp):
        method = getattr(obj, "__reduce_ex__", _MISSING)
        if method is not _MISSING:
            return method(self.proto)
        method = getattr(obj, "__reduce__", _MISSING)
        if method is not _MISSING:
            return method()
        raise PicklingError(f"Can't pickle {_T(tp)} object")

    def save_pers(self, pid):
        if self.bin:
            self.save(pid, save_persistent_id=False)
            self.write(BINPERSID)
            return
        try:
            text = str(pid).encode('ascii')
        except UnicodeEncodeError:
            raise PicklingError(
                "persistent IDs in protocol 0 must be ASCII strings")
        self.write(PERSID + text + b'\n')

    def save_reduce(self, func, args, state=None, listitems=None,
                    dictitems=None, state_setter=None, *, obj=None):
        """Emit the opcodes that rebuild *obj* from a reduce tuple."""
        if not callable(func):
            raise PicklingError("first item of the tuple returned by "
                                f"__reduce__ must be callable, not {_T(func)}")
        if not isinstance(args, tuple):
            raise PicklingError("second item of the tuple returned by "
                                f"__reduce__ must be a tuple, not {_T(args)}")
        label = _T(obj)
        kind = getattr(func, "__name__", "") if self.proto >= 2 else ""

        if kind == "__newobj_ex__":
            cls, args, kwargs = args
            self._check_new_class(cls, obj, "__newobj_ex__")
            if self.proto >= 4:
                with _noted(f'when serializing {label} class'):
                    self.save(cls)
                with _noted(f'when serializing {label} __new__ arguments'):
                    self.save(args)
                    self.save(kwargs)
                self.write(NEWOBJ_EX)
            else:
                # Protocols 2 and 3 have no keyword-argument form: rebuild
                # through a partial of cls.__new__.
                builder = partial(cls.__new__, cls, *args, **kwargs)
                with _noted(f'when serializing {label} reconstructor'):
                    self.save(builder)
                self.save(())
                self.write(REDUCE)
        elif kind == "__newobj__":
            cls = args[0]
            self._check_new_class(cls, obj, "__newobj__")
            with _noted(f'when serializing {label} class'):
                self.save(cls)
            with _noted(f'when serializing {label} __new__ arguments'):
                self.save(args[1:])
            self.write(NEWOBJ)
        else:
            with _noted(f'when serializing {label} reconstructor'):
                self.save(func)
            with _noted(f'when serializing {label} reconstructor arguments'):
                self.save(args)
            self.write(REDUCE)

        if obj is not None:
            known = self.memo.get(id(obj))
            if known is not None:
                # Saving the arguments already produced obj (recursion):
                # drop the duplicate and reuse the memoized one.
                self.write(POP + self.get(known[0]))
            else:
                self.memoize(obj)

        if listitems is not None:
            self._batch_appends(listitems, obj)
        if dictitems is not None:
            self._batch_setitems(dictitems, obj)

        if state is None:
            return
        if state_setter is None:
            with _noted(f'when serializing {label} state'):
                self.save(state)
            self.write(BUILD)
        else:
            # state_setter(obj, state), result discarded.
            with _noted(f'when serializing {label} state setter'):
                self.save(state_setter)
            self.save(obj)
            with _noted(f'when serializing {label} state'):
                self.save(state)
            self.write(TUPLE2 + REDUCE + POP)

    @staticmethod
    def _check_new_class(cls, obj, what):
        if not hasattr(cls, "__new__"):
            raise PicklingError(f"first argument to {what}() has no __new__")
        if obj is not None and cls is not obj.__class__:
            raise PicklingError(f"first argument to {what}() must be "
                                f"{obj.__class__!r}, not {cls!r}")

    # -- per-type savers --------------------------------------------------------

    dispatch = {}

    def save_none(self, obj):
        self.write(NONE)

    def save_bool(self, obj):
        if self.proto >= 2:
            self.write(NEWTRUE if obj else NEWFALSE)
        else:
            self.write(TRUE if obj else FALSE)

    def save_long(self, obj):
        if self.bin:
            if 0 <= obj < 0x100:
                self.write(BININT1 + bytes((obj,)))
                return
            if 0 <= obj < 0x10000:
                self.write(BININT2 + pack('<H', obj))
                return
            if -0x80000000 <= obj < 0x80000000:
                self.write(BININT + pack('<i', obj))
                return
        if self.proto >= 2:
            data = encode_long(obj)
            if len(data) < 256:
                self.write(LONG1 + bytes((len(data),)) + data)
            else:
                self.write(LONG4 + pack('<i', len(data)) + data)
        elif -0x80000000 <= obj < 0x80000000:
            self.write(INT + b'%d\n' % obj)
        else:
            self.write(LONG + b'%dL\n' % obj)

    def save_float(self, obj):
        if self.bin:
            self.write(BINFLOAT + pack('>d', obj))
        else:
            self.write(FLOAT + repr(obj).encode('ascii') + b'\n')

    def _write_sized(self, data, short_op, op4, op8, short_limit=0xff):
        """Emit a length-prefixed binary payload with the smallest opcode."""
        n = len(data)
        if short_op is not None and n <= short_limit:
            self.write(short_op + bytes((n,)) + data)
        elif op8 is not None and n > 0xffffffff:
            self._write_large_bytes(op8 + pack('<Q', n), data)
        elif n >= self.framer._FRAME_SIZE_TARGET:
            self._write_large_bytes(op4 + pack('<I', n), data)
        else:
            self.write(op4 + pack('<I', n) + data)

    def _save_bytes_no_memo(self, obj):
        self._write_sized(obj, SHORT_BINBYTES, BINBYTES,
                          BINBYTES8 if self.proto >= 4 else None)

    def save_bytes(self, obj):
        if self.proto >= 3:
            self._save_bytes_no_memo(obj)
            self.memoize(obj)
        elif obj:
            self.save_reduce(codecs.encode, (str(obj, 'latin1'), 'latin1'),
                             obj=obj)
        else:
            self.save_reduce(bytes, (), obj=obj)

    def _save_bytearray_no_memo(self, obj):
        n = len(obj)
        header = BYTEARRAY8 + pack('<Q', n)
        if n >= self.framer._FRAME_SIZE_TARGET:
            self._write_large_bytes(header, obj)
        else:
            self.write(header + obj)

    def save_bytearray(self, obj):
        if self.proto >= 5:
            self._save_bytearray_no_memo(obj)
            self.memoize(obj)
        else:
            self.save_reduce(bytearray, (bytes(obj),) if obj else (), obj=obj)

    def save_picklebuffer(self, obj):
        if self.proto < 5:
            raise PicklingError("PickleBuffer can only be pickled with "
                                "protocol >= 5")
        with obj.raw() as view:
            if not view.contiguous:
                raise PicklingError("PickleBuffer can not be pickled when "
                                    "pointing to a non-contiguous buffer")
            readonly = view.readonly
            if self._buffer_callback is None or self._buffer_callback(obj):
                data = view.tobytes()
                if readonly:
                    self._save_bytes_no_memo(data)
                else:
                    self._save_bytearray_no_memo(data)
            else:
                self.write(NEXT_BUFFER + READONLY_BUFFER if readonly
                           else NEXT_BUFFER)
        # The buffer object (not the temporary copy) is what gets shared.
        self.memoize(obj)

    def save_str(self, obj):
        if not self.bin:
            # raw-unicode-escape leaves these alone; escape them by hand.
            text = (obj.replace("\\", "\\u005c").replace("\0", "\\u0000")
                       .replace("\n", "\\u000a").replace("\r", "\\u000d")
                       .replace("\x1a", "\\u001a"))
            self.write(UNICODE + text.encode('raw-unicode-escape') + b'\n')
        else:
            data = obj.encode('utf-8', 'surrogatepass')
            big = self.proto >= 4
            self._write_sized(data, SHORT_BINUNICODE if big else None,
                              BINUNICODE, BINUNICODE8 if big else None)
        self.memoize(obj)

    def save_tuple(self, obj):
        n = len(obj)
        if not n:
            self.write(EMPTY_TUPLE if self.bin else MARK + TUPLE)
            return
        short = n <= 3 and self.proto >= 2
        if not short:
            self.write(MARK)
        for i, item in enumerate(obj):
            with _noted(f'when serializing {_T(obj)} item {i}'):
                self.save(item)
        known = self.memo.get(id(obj))
        if known is not None:
            # A recursive reference created the tuple while its items were
            # being saved; throw our copy away and fetch that one.
            if short:
                drop = POP * n
            elif self.bin:
                drop = POP_MARK
            else:
                drop = POP * (n + 1)
            self.write(drop + self.get(known[0]))
            return
        self.write(_SMALL_TUPLE_OPS[n] if short else TUPLE)
        self.memoize(obj)

    def save_list(self, obj):
        self.write(EMPTY_LIST if self.bin else MARK + LIST)
        self.memoize(obj)
        self._batch_appends(obj, obj)

    def _batch_appends(self, items, obj):
        label = _T(obj)
        if not self.bin:
            for i, item in enumerate(items):
                with _noted(f'when serializing {label} item {i}'):
                    self.save(item)
                self.write(APPEND)
            return
        source = iter(items)
        index = 0
        while True:
            chunk = list(islice(source, self._BATCHSIZE))
            if len(chunk) > 1:
                self.write(MARK)
            for item in chunk:
                with _noted(f'when serializing {label} item {index}'):
                    self.save(item)
                index += 1
            if len(chunk) > 1:
                self.write(APPENDS)
            elif chunk:
                self.write(APPEND)
            if len(chunk) < self._BATCHSIZE:
                return

    def save_dict(self, obj):
        self.write(EMPTY_DICT if self.bin else MARK + DICT)
        self.memoize(obj)
        self._batch_setitems(obj.items(), obj)

    def _save_pair(self, key, value, label):
        self.save(key)
        with _noted(f'when serializing {label} item {key!r}'):
            self.save(value)

    def _batch_setitems(self, items, obj):
        label = _T(obj)
        if not self.bin:
            for key, value in items:
                self._save_pair(key, value, label)
                self.write(SETITEM)
            return
        source = iter(items)
        while True:
            chunk = list(islice(source, self._BATCHSIZE))
            if len(chunk) > 1:
                self.write(MARK)
            for key, value in chunk:
                self._save_pair(key, value, label)
            if len(chunk) > 1:
                self.write(SETITEMS)
            elif chunk:
                self.write(SETITEM)
            if len(chunk) < self._BATCHSIZE:
                return

    def save_set(self, obj):
        if self.proto < 4:
            self.save_reduce(set, (list(obj),), obj=obj)
            return
        self.write(EMPTY_SET)
        self.memoize(obj)
        source = iter(obj)
        while True:
            chunk = list(islice(source, self._BATCHSIZE))
            if chunk:
                self.write(MARK)
                with _noted(f'when serializing {_T(obj)} element'):
                    for item in chunk:
                        self.save(item)
                self.write(ADDITEMS)
            if len(chunk) < self._BATCHSIZE:
                return

    def save_frozenset(self, obj):
        if self.proto < 4:
            self.save_reduce(frozenset, (list(obj),), obj=obj)
            return
        self.write(MARK)
        with _noted(f'when serializing {_T(obj)} element'):
            for item in obj:
                self.save(item)
        known = self.memo.get(id(obj))
        if known is not None:
            self.write(POP_MARK + self.get(known[0]))
            return
        self.write(FROZENSET)
        self.memoize(obj)

    def save_frozendict(self, obj):
        if self.proto < 2:
            # protocols 0 and 1 have no way to express a frozendict: the
            # generic reduction refuses it (copyreg._reduce_ex)
            self.save_reduce(obj=obj, *obj.__reduce_ex__(self.proto))
            return
        # Emitted as frozendict({...}) with the same "built while saving
        # the items" check as tuples, so that cycles through mutable
        # values resolve to the one memoized frozendict.
        with _noted(f'when serializing {_T(obj)} reconstructor'):
            self.save(type(obj))
        self.write(EMPTY_DICT)
        self._batch_setitems(obj.items(), obj)
        known = self.memo.get(id(obj))
        if known is not None:
            self.write(POP + POP + self.get(known[0]))
            return
        self.write(TUPLE1 + REDUCE)
        self.memoize(obj)

    def save_global(self, obj, name=None):
        if name is None:
            name = getattr(obj, '__qualname__', None)
            if name is None:
                name = obj.__name__
        module_name = whichmodule(obj, name)

        if self.proto >= 2:
            code = _extension_registry.get((module_name, name), _MISSING)
            if code is not _MISSING:
                self._write_ext(code)
                return

        path = name.split('.')
        if any(p.startswith('__') and not p.endswith('__') for p in path):
            path = _concrete_path(sys.modules[module_name], path)
            name = '.'.join(path)

        if self.proto >= 4:
            self.save(module_name)
            self.save(name)
            self.write(STACK_GLOBAL)
        elif len(path) > 1:
            # Older protocols have no dotted names: emit
            # getattr(getattr(<module.first>, a), b) ...
            for _ in path[1:]:
                self.save(getattr)
                if self.proto < 2:
                    self.write(MARK)
            self._save_toplevel_by_name(module_name, path[0])
            for attr in path[1:]:
                self.save(attr)
                self.write((TUPLE if self.proto < 2 else TUPLE2) + REDUCE)
        else:
            self._save_toplevel_by_name(module_name, name)
        self.memoize(obj)

    def _write_ext(self, code):
        if code <= 0xff:
            arg = pack('<B', code)
            if arg == b'\0':
                raise RuntimeError("extension code 0 is out of range")
            self.write(EXT1 + arg)
        elif code <= 0xffff:
            self.write(EXT2 + pack('<H', code))
        else:
            self.write(EXT4 + pack('<i', code))

    def _save_toplevel_by_name(self, module_name, name):
        if self.proto >= 3:
            encoding = 'utf-8'
        else:
            encoding = 'ascii'
            if self.fix_imports:
                key = (module_name, name)
                if key in _compat_pickle.REVERSE_NAME_MAPPING:
                    module_name, name = _compat_pickle.REVERSE_NAME_MAPPING[key]
                else:
                    module_name = _compat_pickle.REVERSE_IMPORT_MAPPING.get(
                        module_name, module_name)
        try:
            mod = module_name.encode(encoding)
        except UnicodeEncodeError:
            raise PicklingError(f"can't pickle module identifier "
                                f"{module_name!r} using pickle protocol "
                                f"{self.proto}")
        try:
            attr = name.encode(encoding)
        except UnicodeEncodeError:
            raise PicklingError(f"can't pickle global identifier {name!r} "
                                f"using pickle protocol {self.proto}")
        self.write(GLOBAL + mod + b'\n' + attr + b'\n')

    def save_type(self, obj):
        for singleton in (None, NotImplemented, ...):
            if obj is type(singleton):
                return self.save_reduce(type, (singleton,), obj=obj)
        return self.save_global(obj)


_Pickler.dispatch.update({
    type(None): _Pickler.save_none,
    bool: _Pickler.save_bool,
    int: _Pickler.save_long,
    float: _Pickler.save_float,
    bytes: _Pickler.save_bytes,
    bytearray: _Pickler.save_bytearray,
    str: _Pickler.save_str,
    tuple: _Pickler.save_tuple,
    list: _Pickler.save_list,
    dict: _Pickler.save_dict,
    set: _Pickler.save_set,
    frozenset: _Pickler.save_frozenset,
    FunctionType: _Pickler.save_global,
    type: _Pickler.save_type,
})
if _HAVE_PICKLE_BUFFER:
    _Pickler.dispatch[PickleBuffer] = _Pickler.save_picklebuffer
try:
    _Pickler.dispatch[frozendict] = _Pickler.save_frozendict
except NameError:
    pass


# ---------------------------------------------------------------------------
# Input side

class _Unframer:
    """Serves reads either from the current frame or from the file.

    A frame may only be left between two opcodes: an opcode argument that
    runs past the end of its frame is an error.
    """

    def __init__(self, file_read, file_readline):
        self.file_read = file_read
        self.file_readline = file_readline
        self.current_frame = None

    def _from_file(self, n):
        if n <= _READ_PIECE:
            return self.file_read(n)
        pieces = []
        left = n
        while left:
            piece = self.file_read(min(left, _READ_PIECE))
            pieces.append(piece)
            if len(piece) < min(left, _READ_PIECE):
                break
            left -= len(piece)
        return b''.join(pieces)

    def read_opcode(self):
        frame = self.current_frame
        if frame is not None:
            op = frame.read(1)
            if op:
                return op
            self.current_frame = None
        return self.file_read(1)

    def read(self, n):
        frame = self.current_frame
        if frame is None:
            return self._from_file(n)
        data = frame.read(n)
        if len(data) != n:
            raise UnpicklingError("pickle exhausted before end of frame")
        return data

    def readinto(self, buf):
        frame = self.current_frame
        if frame is None:
            data = self._from_file(len(buf))
            buf[:len(data)] = data
            return len(data)
        n = frame.readinto(buf)
        if n != len(buf):
            raise UnpicklingError("pickle exhausted before end of frame")
        return n

    def readline(self):
        frame = self.current_frame
        if frame is None:
            return self.file_readline()
        line = frame.readline()
        if not line.endswith(b'\n'):
            raise UnpicklingError("pickle exhausted before end of frame")
        return line

    def load_frame(self, size):
        frame = self.current_frame
        if frame is not None and frame.read(1):
            raise UnpicklingError(
                "beginning of a new frame before end of current frame")
        data = self._from_file(size)
        if len(data) < size:
            raise EOFError
        self.current_frame = io.BytesIO(data)


def _opcode(code):
    """Decorator marking an Unpickler method as the handler of *code*."""
    def mark(func):
        func._pickle_opcode = code[0]
        return func
    return mark


class _Unpickler:

    dispatch = {}

    def __init__(self, file, *, fix_imports=True,
                 encoding="ASCII", errors="strict", buffers=None):
        """Read pickles from the binary *file* (needs read() and
        readline()).

        *buffers* supplies the out-of-band buffers of a protocol 5 pickle,
        in order.  *fix_imports*, *encoding* and *errors* control how
        Python 2 pickles are read; encoding='bytes' keeps 8-bit strings as
        bytes.
        """
        self._buffers = None if buffers is None else iter(buffers)
        self._file_readline = file.readline
        self._file_read = file.read
        self.memo = {}
        self.encoding = encoding
        self.errors = errors
        self.proto = 0
        self.fix_imports = fix_imports

    def load(self):
        """Read one pickle from the file and return the object."""
        if not hasattr(self, "_file_read"):
            raise UnpicklingError("Unpickler.__init__() was not called by "
                                  "%s.__init__()" % type(self).__name__)
        frames = self._unframer = _Unframer(self._file_read,
                                            self._file_readline)
        self.read = frames.read
        self.readinto = frames.readinto
        self.readline = frames.readline
        self.metastack = []
        self.stack = []
        self.append = self.stack.append
        self.proto = 0
        next_op = frames.read_opcode
        handlers = self.dispatch
        try:
            while True:
                op = next_op()
                if not op:
                    raise EOFError
                handlers[op[0]](self)
        except _Stop as stop:
            return stop.value

    def pop_mark(self):
        items = self.stack
        self.stack = self.metastack.pop()
        self.append = self.stack.append
        return items

    def persistent_load(self, pid):
        raise UnpicklingError("unsupported persistent id encountered")

    def _decode_string(self, value):
        # 8-bit strings written by Python 2.
        if self.encoding == "bytes":
            return value
        return value.decode(self.encoding, self.errors)

    def _sized(self, fmt, size, opname):
        n, = unpack(fmt, self.read(size))
        if n > maxsize:
            raise UnpicklingError(f"{opname} exceeds system's maximum size "
                                  f"of {maxsize} bytes")
        return n

    def find_class(self, module, name):
        """Return the object *name* of *module* (subclasses may restrict)."""
        sys.audit('pickle.find_class', module, name)
        if self.proto < 3 and self.fix_imports:
            key = (module, name)
            if key in _compat_pickle.NAME_MAPPING:
                module, name = _compat_pickle.NAME_MAPPING[key]
            else:
                module = _compat_pickle.IMPORT_MAPPING.get(module, module)
        __import__(module, level=0)
        mod = sys.modules[module]
        if self.proto < 4 or '.' not in name:
            return getattr(mod, name)
        try:
            return _getattribute(mod, name.split('.'))
        except AttributeError:
            raise AttributeError(
                f"Can't resolve path {name!r} on module {module!r}")

    def get_extension(self, code):
        if code <= 0:
            raise UnpicklingError("EXT specifies code <= 0")
        obj = _extension_cache.get(code, _MISSING)
        if obj is _MISSING:
            key = _inverted_registry.get(code)
            if not key:
                raise ValueError("unregistered extension code %d" % code)
            obj = self.find_class(*key)
            _extension_cache[code] = obj
        self.append(obj)

    def _instantiate(self, klass, args):
        if args or not isinstance(klass, type) or \
                hasattr(klass, "__getinitargs__"):
            try:
                value = klass(*args)
            except TypeError as err:
                raise TypeError("in constructor for %s: %s" %
                                (klass.__name__, err)).with_traceback(
                                    err.__traceback__)
        else:
            value = klass.__new__(klass)
        self.append(value)

    def _memo_get(self, index):
        try:
            self.append(self.memo[index])
        except KeyError:
            raise UnpicklingError(
                f'Memo value not found at index {index}') from None


    # Handlers.  Each reads its argument (if any) and updates the stack.

    @_opcode(PROTO)
    def load_proto(self):
        proto = self.read(1)[0]
        if proto > HIGHEST_PROTOCOL:
            raise ValueError("unsupported pickle protocol: %d" % proto)
        self.proto = proto

    @_opcode(FRAME)
    def load_frame(self):
        size, = unpack('<Q', self.read(8))
        if size > maxsize:
            raise ValueError("frame size > sys.maxsize: %d" % size)
        self._unframer.load_frame(size)

    @_opcode(PERSID)
    def load_persid(self):
        try:
            pid = self.readline()[:-1].decode('ascii')
        except UnicodeDecodeError:
            raise UnpicklingError(
                "persistent IDs in protocol 0 must be ASCII strings")
        self.append(self.persistent_load(pid))

    @_opcode(BINPERSID)
    def load_binpersid(self):
        self.append(self.persistent_load(self.stack.pop()))

    @_opcode(NONE)
    def load_none(self):
        self.append(None)

    @_opcode(NEWFALSE)
    def load_false(self):
        self.append(False)

    @_opcode(NEWTRUE)
    def load_true(self):
        self.append(True)

    @_opcode(INT)
    def load_int(self):
        line = self.readline()
        if line == b'00\n':
            self.append(False)
        elif line == b'01\n':
            self.append(True)
        else:
            self.append(int(line))

    @_opcode(BININT)
    def load_binint(self):
        self.append(unpack('<i', self.read(4))[0])

    @_opcode(BININT1)
    def load_binint1(self):
        self.append(self.read(1)[0])

    @_opcode(BININT2)
    def load_binint2(self):
        self.append(unpack('<H', self.read(2))[0])

    @_opcode(LONG)
    def load_long(self):
        digits = self.readline()[:-1]
        if digits.endswith(b'L'):
            digits = digits[:-1]
        self.append(int(digits))

    @_opcode(LONG1)
    def load_long1(self):
        n = self.read(1)[0]
        self.append(decode_long(self.read(n)))

    @_opcode(LONG4)
    def load_long4(self):
        n, = unpack('<i', self.read(4))
        if n < 0:
            raise UnpicklingError("LONG pickle has negative byte count")
        self.append(decode_long(self.read(n)))

    @_opcode(FLOAT)
    def load_float(self):
        self.append(float(self.readline()[:-1]))

    @_opcode(BINFLOAT)
    def load_binfloat(self):
        self.append(unpack('>d', self.read(8))[0])

    @_opcode(STRING)
    def load_string(self):
        body = self.readline()[:-1]
        if len(body) < 2 or body[0] != body[-1] or body[:1] not in (b'"', b"'"):
            raise UnpicklingError("the STRING opcode argument must be quoted")
        self.append(self._decode_string(codecs.escape_decode(body[1:-1])[0]))

    @_opcode(BINSTRING)
    def load_binstring(self):
        n, = unpack('<i', self.read(4))
        if n < 0:
            raise UnpicklingError("BINSTRING pickle has negative byte count")
        self.append(self._decode_string(self.read(n)))

    @_opcode(SHORT_BINSTRING)
    def load_short_binstring(self):
        n = self.read(1)[0]
        self.append(self._decode_string(self.read(n)))

    @_opcode(BINBYTES)
    def load_binbytes(self):
        self.append(self.read(self._sized('<I', 4, 'BINBYTES')))

    @_opcode(SHORT_BINBYTES)
    def load_short_binbytes(self):
        self.append(self.read(self.read(1)[0]))

    @_opcode(BINBYTES8)
    def load_binbytes8(self):
        self.append(self.read(self._sized('<Q', 8, 'BINBYTES8')))

    @_opcode(BYTEARRAY8)
    def load_bytearray8(self):
        n = self._sized('<Q', 8, 'BYTEARRAY8')
        if n <= _READ_PIECE:
            buf = bytearray(n)
            self.readinto(buf)
        else:
            buf = bytearray(self.read(n))
        self.append(buf)

    @_opcode(NEXT_BUFFER)
    def load_next_buffer(self):
        if self._buffers is None:
            raise UnpicklingError("pickle stream refers to out-of-band data "
                                  "but no *buffers* argument was given")
        buf = next(self._buffers, _MISSING)
        if buf is _MISSING:
            raise UnpicklingError("not enough out-of-band buffers")
        self.append(buf)

    @_opcode(READONLY_BUFFER)
    def load_readonly_buffer(self):
        with memoryview(self.stack[-1]) as view:
            if not view.readonly:
                self.stack[-1] = view.toreadonly()

    @_opcode(UNICODE)
    def load_unicode(self):
        self.append(str(self.readline()[:-1], 'raw-unicode-escape'))

    @_opcode(BINUNICODE)
    def load_binunicode(self):
        n = self._sized('<I', 4, 'BINUNICODE')
        self.append(str(self.read(n), 'utf-8', 'surrogatepass'))

    @_opcode(SHORT_BINUNICODE)
    def load_short_binunicode(self):
        n = self.read(1)[0]
        self.append(str(self.read(n), 'utf-8', 'surrogatepass'))

    @_opcode(BINUNICODE8)
    def load_binunicode8(self):
        n = self._sized('<Q', 8, 'BINUNICODE8')
        self.append(str(self.read(n), 'utf-8', 'surrogatepass'))

    @_opcode(EMPTY_TUPLE)
    def load_empty_tuple(self):
        self.append(())

    @_opcode(TUPLE)
    def load_tuple(self):
        items = self.pop_mark()
        self.append(tuple(items))

    @_opcode(TUPLE1)
    def load_tuple1(self):
        self.stack[-1] = (self.stack[-1],)

    @_opcode(TUPLE2)
    def load_tuple2(self):
        second = self.stack.pop()
        self.stack[-1] = (self.stack[-1], second)

    @_opcode(TUPLE3)
    def load_tuple3(self):
        third = self.stack.pop()
        second = self.stack.pop()
        self.stack[-1] = (self.stack[-1], second, third)

    @_opcode(EMPTY_LIST)
    def load_empty_list(self):
        self.append([])

    @_opcode(LIST)
    def load_list(self):
        items = self.pop_mark()
        self.append(items)

    @_opcode(EMPTY_DICT)
    def load_empty_dictionary(self):
        self.append({})

    @_opcode(DICT)
    def load_dict(self):
        flat = self.pop_mark()
        self.append({flat[i]: flat[i + 1] for i in range(0, len(flat), 2)})

    @_opcode(EMPTY_SET)
    def load_empty_set(self):
        self.append(set())

    @_opcode(FROZENSET)
    def load_frozenset(self):
        items = self.pop_mark()
        self.append(frozenset(items))

    @_opcode(INST)
    def load_inst(self):
        module = self.readline()[:-1].decode('ascii')
        name = self.readline()[:-1].decode('ascii')
        klass = self.find_class(module, name)
        self._instantiate(klass, self.pop_mark())

    @_opcode(OBJ)
    def load_obj(self):
        args = self.pop_mark()
        klass = args.pop(0)
        self._instantiate(klass, args)

    @_opcode(NEWOBJ)
    def load_newobj(self):
        args = self.stack.pop()
        cls = self.stack.pop()
        if not isinstance(args, tuple):
            raise UnpicklingError("NEWOBJ expected an arg tuple.")
        self.append(cls.__new__(cls, *args))

    @_opcode(NEWOBJ_EX)
    def load_newobj_ex(self):
        kwargs = self.stack.pop()
        args = self.stack.pop()
        cls = self.stack.pop()
        if not isinstance(args, tuple):
            raise UnpicklingError("NEWOBJ_EX args argument must be a tuple.")
        if not isinstance(kwargs, dict):
            raise UnpicklingError("NEWOBJ_EX kwargs argument must be a dict.")
        self.append(cls.__new__(cls, *args, **kwargs))

    @_opcode(GLOBAL)
    def load_global(self):
        module = self.readline()[:-1].decode('utf-8')
        name = self.readline()[:-1].decode('utf-8')
        self.append(self.find_class(module, name))

    @_opcode(STACK_GLOBAL)
    def load_stack_global(self):
        name = self.stack.pop()
        module = self.stack.pop()
        if type(name) is not str or type(module) is not str:
            raise UnpicklingError("STACK_GLOBAL requires str")
        self.append(self.find_class(module, name))

    @_opcode(EXT1)
    def load_ext1(self):
        self.get_extension(self.read(1)[0])

    @_opcode(EXT2)
    def load_ext2(self):
        self.get_extension(unpack('<H', self.read(2))[0])

    @_opcode(EXT4)
    def load_ext4(self):
        self.get_extension(unpack('<i', self.read(4))[0])

    @_opcode(REDUCE)
    def load_reduce(self):
        args = self.stack.pop()
        self.stack[-1] = self.stack[-1](*args)

    @_opcode(POP)
    def load_pop(self):
        if self.stack:
            self.stack.pop()
        else:
            self.pop_mark()

    @_opcode(POP_MARK)
    def load_pop_mark(self):
        self.pop_mark()

    @_opcode(DUP)
    def load_dup(self):
        self.append(self.stack[-1])

    @_opcode(GET)
    def load_get(self):
        self._memo_get(int(self.readline()[:-1]))

    @_opcode(BINGET)
    def load_binget(self):
        self._memo_get(self.read(1)[0])

    @_opcode(LONG_BINGET)
    def load_long_binget(self):
        self._memo_get(unpack('<I', self.read(4))[0])

    @_opcode(PUT)
    def load_put(self):
        index = int(self.readline()[:-1])
        if index < 0:
            raise ValueError("negative PUT argument")
        self.memo[index] = self.stack[-1]

    @_opcode(BINPUT)
    def load_binput(self):
        self.memo[self.read(1)[0]] = self.stack[-1]

    @_opcode(LONG_BINPUT)
    def load_long_binput(self):
        index, = unpack('<I', self.read(4))
        if index > maxsize:
            raise ValueError("negative LONG_BINPUT argument")
        self.memo[index] = self.stack[-1]

    @_opcode(MEMOIZE)
    def load_memoize(self):
        self.memo[len(self.memo)] = self.stack[-1]

    @_opcode(APPEND)
    def load_append(self):
        value = self.stack.pop()
        self.stack[-1].append(value)

    @_opcode(APPENDS)
    def load_appends(self):
        items = self.pop_mark()
        target = self.stack[-1]
        extend = getattr(target, 'extend', None)
        if extend is not None:
            extend(items)
            return
        # Objects without extend() only need append().
        add = target.append
        for item in items:
            add(item)

    @_opcode(SETITEM)
    def load_setitem(self):
        value = self.stack.pop()
        key = self.stack.pop()
        self.stack[-1][key] = value

    @_opcode(SETITEMS)
    def load_setitems(self):
        flat = self.pop_mark()
        target = self.stack[-1]
        for i in range(0, len(flat), 2):
            target[flat[i]] = flat[i + 1]

    @_opcode(ADDITEMS)
    def load_additems(self):
        items = self.pop_mark()
        target = self.stack[-1]
        if isinstance(target, set):
            target.update(items)
            return
        add = target.add
        for item in items:
            add(item)

    @_opcode(BUILD)
    def load_build(self):
        state = self.stack.pop()
        inst = self.stack[-1]
        setstate = getattr(inst, "__setstate__", _MISSING)
        if setstate is not _MISSING:
            setstate(state)
            return
        slotstate = None
        if isinstance(state, tuple) and len(state) == 2:
            state, slotstate = state
        if state:
            target = inst.__dict__
            for key, value in state.items():
                if type(key) is str:
                    key = sys.intern(key)
                target[key] = value
        if slotstate:
            for key, value in slotstate.items():
                setattr(inst, key, value)

    @_opcode(MARK)
    def load_mark(self):
        self.metastack.append(self.stack)
        self.stack = []
        self.append = self.stack.append

    @_opcode(STOP)
    def load_stop(self):
        raise _Stop(self.stack.pop())

for _name, _func in list(vars(_Unpickler).items()):
    if hasattr(_func, "_pickle_opcode"):
        _Unpickler.dispatch[_func._pickle_opcode] = _func
del _name, _func


# ---------------------------------------------------------------------------
# Convenience functions

def _dump(obj, file, protocol=None, *, fix_imports=True, buffer_callback=None):
    _Pickler(file, protocol, fix_imports=fix_imports,
             buffer_callback=buffer_callback).dump(obj)

def _dumps(obj, protocol=None, *, fix_imports=True, buffer_callback=None):
    out = io.BytesIO()
    _dump(obj, out, protocol, fix_imports=fix_imports,
          buffer_callback=buffer_callback)
    return out.getvalue()

def _load(file, *, fix_imports=True, encoding="ASCII", errors="strict",
          buffers=None):
    return _Unpickler(file, fix_imports=fix_imports, buffers=buffers,
                      encoding=encoding, errors=errors).load()

def _loads(s, /, *, fix_imports=True, encoding="ASCII", errors="strict",
           buffers=None):
    if isinstance(s, str):
        raise TypeError("Can't load pickle from unicode string")
    return _load(io.BytesIO(s), fix_imports=fix_imports, encoding=encoding,
                 errors=errors, buffers=buffers)


try:
    from _pickle import (
        PickleError, PicklingError, UnpicklingError,
        Pickler, Unpickler, dump, dumps, load, loads,
    )
except ImportError:
    Pickler, Unpickler = _Pickler, _Unpickler
    dump, dumps, load, loads = _dump, _dumps, _load, _loads


def _main(args=None):
    import argparse
    import pprint
    parser = argparse.ArgumentParser(
        description='display contents of the pickle files', color=True)
    parser.add_argument('pickle_file', nargs='+', help='the pickle file')
    options = parser.parse_args(args)
    for path in options.pickle_file:
        if path == '-':
            pprint.pprint(load(sys.stdin.buffer))
        else:
            with open(path, 'rb') as f:
                pprint.pprint(load(f))


if __name__ == "__main__":
    _main()
