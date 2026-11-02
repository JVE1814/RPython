"""Disassembler of Python bytecode (CPython 3.15 wordcode) into human readable form.

The instruction stream is a sequence of 2-byte code units (opcode, oparg); EXTENDED_ARG
prefixes widen an oparg and some opcodes are followed by inline CACHE units.
"""

import collections
import io
import sys
import types

from opcode import *
from opcode import (
    __all__ as _opcode_all,
    _cache_format,
    _common_constants,
    _inline_cache_entries,
    _intrinsic1_descs,
    _intrinsic2_descs,
    _nb_ops,
    _special_method_names,
    _specialized_opmap,
    _specializations,
)

__all__ = ["code_info", "dis", "disassemble", "distb", "disco",
           "findlinestarts", "findlabels", "show_code",
           "get_instructions", "Instruction", "Bytecode"] + _opcode_all
del _opcode_all

_have_code = (types.MethodType, types.FunctionType, types.CodeType,
              classmethod, staticmethod, type)

# every opcode name, including the specialised ones, by number
_all_opname = list(opname)
_all_opmap = dict(opmap)
for _name, _op in _specialized_opmap.items():
    _all_opname[_op] = _name
    _all_opmap[_name] = _op

# specialised opcode -> the generic opcode it stands for
deoptmap = {
    _specialized_opmap[spec]: opmap[base]
    for base, family in _specializations.items()
    for spec in family
}
del _name, _op

FORMAT_VALUE_CONVERTERS = (
    (None, ''),
    (str, 'str'),
    (repr, 'repr'),
    (ascii, 'ascii'),
)
MAKE_FUNCTION_FLAGS = ('defaults', 'kwdefaults', 'annotations', 'closure', 'annotate')
FUNCTION_ATTR_FLAGS = MAKE_FUNCTION_FLAGS

_OPNAME_WIDTH = 20
_OPARG_WIDTH = 5
_NO_LINENO = '  --'
_INT_OVERFLOW = 2 ** 31

LOAD_CONST = opmap['LOAD_CONST']
LOAD_SMALL_INT = opmap['LOAD_SMALL_INT']
LOAD_GLOBAL = opmap['LOAD_GLOBAL']
LOAD_ATTR = opmap['LOAD_ATTR']
LOAD_SUPER_ATTR = opmap['LOAD_SUPER_ATTR']
IMPORT_NAME = opmap['IMPORT_NAME']
COMPARE_OP = opmap['COMPARE_OP']
IS_OP = opmap['IS_OP']
CONTAINS_OP = opmap['CONTAINS_OP']
BINARY_OP = opmap['BINARY_OP']
CONVERT_VALUE = opmap['CONVERT_VALUE']
SET_FUNCTION_ATTRIBUTE = opmap['SET_FUNCTION_ATTRIBUTE']
CALL_INTRINSIC_1 = opmap['CALL_INTRINSIC_1']
CALL_INTRINSIC_2 = opmap['CALL_INTRINSIC_2']
LOAD_SPECIAL = opmap['LOAD_SPECIAL']
LOAD_COMMON_CONSTANT = opmap['LOAD_COMMON_CONSTANT']
LOAD_FAST_LOAD_FAST = opmap['LOAD_FAST_LOAD_FAST']
LOAD_FAST_BORROW_LOAD_FAST_BORROW = opmap['LOAD_FAST_BORROW_LOAD_FAST_BORROW']
STORE_FAST_LOAD_FAST = opmap['STORE_FAST_LOAD_FAST']
STORE_FAST_STORE_FAST = opmap['STORE_FAST_STORE_FAST']
END_ASYNC_FOR = opmap['END_ASYNC_FOR']
CACHE = opmap['CACHE']

_BACKWARD_JUMPS = frozenset([
    opmap['JUMP_BACKWARD'],
    opmap['JUMP_BACKWARD_NO_INTERRUPT'],
    END_ASYNC_FOR,
])
_DOUBLE_LOCALS = frozenset([
    LOAD_FAST_LOAD_FAST, LOAD_FAST_BORROW_LOAD_FAST_BORROW,
    STORE_FAST_LOAD_FAST, STORE_FAST_STORE_FAST,
])
_IMPORT_MODES = ('', ' + lazy', ' + eager', '')

# co_flags bit names (inspect takes its CO_* constants from here)
COMPILER_FLAG_NAMES = {
    0x0001: "OPTIMIZED",
    0x0002: "NEWLOCALS",
    0x0004: "VARARGS",
    0x0008: "VARKEYWORDS",
    0x0010: "NESTED",
    0x0020: "GENERATOR",
    0x0040: "NOFREE",
    0x0080: "COROUTINE",
    0x0100: "ITERABLE_COROUTINE",
    0x0200: "ASYNC_GENERATOR",
    0x4000000: "HAS_DOCSTRING",
    0x8000000: "METHOD",
}


def pretty_flags(flags):
    """Names of the flag bits set in `flags`, comma separated (unknown bits in hex)."""
    names = []
    for bit in range(32):
        mask = 1 << bit
        if flags & mask:
            name = COMPILER_FLAG_NAMES.get(mask)
            if name is None:
                break
            names.append(name)
            flags ^= mask
    if flags or not names:
        names.append(hex(flags))
    return ", ".join(names)


def _deoptop(op):
    return deoptmap.get(op, op)


def _cache_units(op):
    return _inline_cache_entries.get(_all_opname[_deoptop(op)], 0)


# ----------------------------------------------------------------------------- code objects

class _NoCompile(Exception):
    pass


def _try_compile(source, name):
    """Compile `source` as an expression if possible, else as statements.

    The fallback compile happens outside the handler so that a SyntaxError from it has no
    chained context."""
    try:
        return compile(source, name, 'eval')
    except SyntaxError:
        pass
    return compile(source, name, 'exec')


def _get_code_object(x):
    """The code object behind a function, method, generator, string or code."""
    if hasattr(x, '__func__'):
        x = x.__func__
    if hasattr(x, '__code__'):
        x = x.__code__
    elif hasattr(x, 'gi_code'):
        x = x.gi_code
    elif hasattr(x, 'ag_code'):
        x = x.ag_code
    elif hasattr(x, 'cr_code'):
        x = x.cr_code
    if isinstance(x, str):
        x = _try_compile(x, "<disassembly>")
    if hasattr(x, 'co_code'):
        return x
    raise TypeError("don't know how to disassemble %s objects" % type(x).__name__)


def _get_code_array(co, adaptive):
    if adaptive:
        code = co._co_code_adaptive
        return bytes(code)
    return co.co_code


def code_info(x):
    """Formatted details of methods, functions, or code."""
    return _format_code_info(_get_code_object(x))


def _format_code_info(co):
    out = [
        "Name:              %s" % co.co_name,
        "Filename:          %s" % co.co_filename,
        "Argument count:    %s" % co.co_argcount,
        "Positional-only arguments: %s" % co.co_posonlyargcount,
        "Kw-only arguments: %s" % co.co_kwonlyargcount,
        "Number of locals:  %s" % co.co_nlocals,
        "Stack size:        %s" % co.co_stacksize,
        "Flags:             %s" % pretty_flags(co.co_flags),
    ]
    sections = (
        ("Constants:", co.co_consts, "%4d: %r"),
        ("Names:", co.co_names, "%4d: %s"),
        ("Variable names:", co.co_varnames, "%4d: %s"),
        ("Free variables:", co.co_freevars, "%4d: %s"),
        ("Cell variables:", co.co_cellvars, "%4d: %s"),
    )
    for title, items, fmt in sections:
        if items:
            out.append(title)
            out.extend(fmt % pair for pair in enumerate(items))
    return "\n".join(out)


def show_code(co, *, file=None):
    """Print details of methods, functions, or code to *file* (default: stdout)."""
    print(code_info(co), file=file)


# ----------------------------------------------------------------------------- instructions

Positions = collections.namedtuple(
    'Positions',
    ['lineno', 'end_lineno', 'col_offset', 'end_col_offset'],
    defaults=[None] * 4,
)

_Instruction = collections.namedtuple(
    "_Instruction",
    ["opname", "opcode", "arg", "argval", "argrepr", "offset", "start_offset",
     "starts_line", "line_number", "label", "positions", "cache_info"],
    defaults=[None, None, None],
)

_ExceptionTableEntryBase = collections.namedtuple(
    "_ExceptionTableEntryBase", "start end target depth lasti")


class _ExceptionTableEntry(_ExceptionTableEntryBase):
    pass


class Instruction(_Instruction):
    """One bytecode operation.

    opname/opcode: name and number of the operation; arg: numeric argument (None if the
    operation takes none); argval: resolved argument value; argrepr: its human readable
    form; offset: start of the operation in co_code; start_offset: start including its
    EXTENDED_ARG prefixes; starts_line: whether a source line starts here; line_number:
    source line (or None); label: jump-target label number (or None); positions: the
    dis.Positions of the operation; cache_info: (name, size, data) of its inline caches.
    """

    @staticmethod
    def make(opname, arg, argval, argrepr, offset, start_offset, starts_line,
             line_number, label=None, positions=None, cache_info=None):
        return Instruction(opname, _all_opmap[opname], arg, argval, argrepr, offset,
                           start_offset, starts_line, line_number, label, positions,
                           cache_info)

    @property
    def oparg(self):
        """Alias of `arg`."""
        return self.arg

    @property
    def baseopcode(self):
        """The generic opcode of a specialised instruction (else `opcode`)."""
        return _deoptop(self.opcode)

    @property
    def baseopname(self):
        """Name of `baseopcode`."""
        return opname[self.baseopcode]

    @property
    def cache_offset(self):
        """Offset of the first inline cache unit."""
        return self.offset + 2

    @property
    def end_offset(self):
        """Offset just past the instruction and its caches."""
        return self.cache_offset + _cache_units(self.opcode) * 2

    @property
    def jump_target(self):
        """Target offset of a jump instruction, else None."""
        return _get_jump_target(self.opcode, self.arg, self.offset)

    @property
    def is_jump_target(self):
        """Whether some instruction jumps here."""
        return self.label is not None

    def __str__(self):
        out = io.StringIO()
        _Formatter(file=out).print_instruction(self)
        return out.getvalue()


def _get_jump_target(op, arg, offset):
    """Offset an instruction jumps to (None for non-jumps)."""
    deop = _deoptop(op)
    if arg is None or deop not in hasjump:
        return None
    delta = -arg if deop in _BACKWARD_JUMPS else arg
    return offset + 2 + 2 * _cache_units(deop) + 2 * delta


class ArgResolver:
    """Turns opargs into (argval, argrepr) using a code object's tables."""

    def __init__(self, co_consts=None, names=None, varname_from_oparg=None, labels_map=None):
        self.co_consts = co_consts
        self.names = names
        self.varname_from_oparg = varname_from_oparg
        self.labels_map = labels_map or {}

    def offset_from_jump_arg(self, op, arg, offset):
        deop = _deoptop(op)
        if deop in hasjump:
            return _get_jump_target(deop, arg, offset)
        if deop in hasexc:
            return arg * 2
        return None

    def get_label_for_offset(self, offset):
        return self.labels_map.get(offset, None)

    def _name(self, index):
        if self.names is None:
            return index, ''
        name = self.names[index]
        return name, name

    def _local(self, index):
        if self.varname_from_oparg is None:
            return index, ''
        name = self.varname_from_oparg(index)
        return name, name

    def get_argval_argrepr(self, op, arg, offset):
        if arg is None:
            return None, ''
        deop = _deoptop(op)
        if deop == LOAD_CONST:
            if self.co_consts is None:
                return arg, ''
            value = self.co_consts[arg]
            return value, repr(value)
        if deop in hasname:
            if deop == LOAD_GLOBAL:
                value, text = self._name(arg >> 1)
                if arg & 1 and text:
                    text += " + NULL"
                return value, text
            if deop == LOAD_ATTR:
                value, text = self._name(arg >> 1)
                if arg & 1 and text:
                    text += " + NULL|self"
                return value, text
            if deop == LOAD_SUPER_ATTR:
                value, text = self._name(arg >> 2)
                if arg & 1 and text:
                    text += " + NULL|self"
                return value, text
            if deop == IMPORT_NAME:
                value, text = self._name(arg >> 2)
                if text:
                    text += _IMPORT_MODES[arg & 3]
                return value, text
            return self._name(arg)
        if deop in hasjump or deop in hasexc:
            target = self.offset_from_jump_arg(op, arg, offset)
            label = self.get_label_for_offset(target)
            if label is None:
                return target, "to " + repr(target)
            word = "from" if deop == END_ASYNC_FOR else "to"
            return target, "%s L%s" % (word, label)
        if deop in _DOUBLE_LOCALS:
            if self.varname_from_oparg is None:
                return arg, ''
            pair = (self.varname_from_oparg(arg >> 4), self.varname_from_oparg(arg & 15))
            return pair, ", ".join(pair)
        if deop in haslocal or deop in hasfree:
            return self._local(arg)
        if deop == COMPARE_OP:
            value = cmp_op[arg >> 5]
            return value, ("bool(%s)" % value) if arg & 16 else value
        if deop == IS_OP:
            return arg, "is not" if arg else "is"
        if deop == CONTAINS_OP:
            return arg, "not in" if arg else "in"
        if deop == CONVERT_VALUE:
            return FORMAT_VALUE_CONVERTERS[arg]
        if deop == SET_FUNCTION_ATTRIBUTE:
            names = [n for i, n in enumerate(FUNCTION_ATTR_FLAGS) if arg & (1 << i)]
            return arg, ", ".join(names)
        if deop == BINARY_OP:
            return arg, _nb_ops[arg][1]
        if deop == CALL_INTRINSIC_1:
            return arg, _intrinsic1_descs[arg]
        if deop == CALL_INTRINSIC_2:
            return arg, _intrinsic2_descs[arg]
        if deop == LOAD_SPECIAL:
            return arg, _special_method_names[arg]
        if deop == LOAD_COMMON_CONSTANT:
            value = _common_constants[arg]
            if isinstance(value, type) or isinstance(value, types.BuiltinFunctionType):
                return value, value.__name__
            return value, repr(value)
        return arg, ''


def _unpack_opargs(code):
    """Yield (offset, start_offset, opcode, arg) for every instruction (EXTENDED_ARG
    prefixes included as instructions of their own; caches skipped)."""
    pending = 0          # oparg bits contributed by preceding EXTENDED_ARGs
    prefix_units = 0
    skip = 0
    for i in range(0, len(code) - 1, 2):
        if skip:
            skip -= 1
            continue
        op = code[i]
        deop = _deoptop(op)
        skip = _cache_units(deop)
        if deop in hasarg:
            arg = code[i + 1] | pending
            if deop == EXTENDED_ARG:
                pending = arg << 8
                if pending >= _INT_OVERFLOW:
                    pending -= 2 * _INT_OVERFLOW
            else:
                pending = 0
        else:
            arg = None
            pending = 0
        if deop == EXTENDED_ARG:
            prefix_units += 1
            yield i, i, op, arg
        else:
            yield i, i - 2 * prefix_units, op, arg
            prefix_units = 0


def findlabels(code):
    """Offsets in the bytecode string `code` that are jump targets."""
    labels = []
    for offset, _, op, arg in _unpack_opargs(code):
        target = _get_jump_target(op, arg, offset)
        if target is not None and target not in labels:
            labels.append(target)
    return labels


def findlinestarts(code):
    """Yield (offset, lineno) for every offset where a new source line starts
    (lineno is None for bytecode without a source line)."""
    previous = object()
    for start, end, line in code.co_lines():
        if start < end and line != previous:
            yield start, line
            previous = line


def _parse_varint(it):
    b = next(it)
    value = b & 63
    while b & 64:
        b = next(it)
        value = (value << 6) | (b & 63)
    return value


def _parse_exception_table(code):
    """The exception table as _ExceptionTableEntry tuples (byte offsets)."""
    it = iter(code.co_exceptiontable)
    entries = []
    try:
        while True:
            start = _parse_varint(it) * 2
            length = _parse_varint(it) * 2
            target = _parse_varint(it) * 2
            dl = _parse_varint(it)
            entries.append(_ExceptionTableEntry(start, start + length, target, dl >> 1, bool(dl & 1)))
    except StopIteration:
        return entries


def _make_labels_map(original_code, exception_entries=()):
    targets = set(findlabels(original_code))
    for entry in exception_entries:
        targets.update((entry.start, entry.end, entry.target))
    return {offset: number for number, offset in enumerate(sorted(targets), 1)}


def _get_instructions_bytes(code, linestarts=None, line_offset=0, co_positions=None,
                            original_code=None, arg_resolver=None):
    """Iterate over Instruction tuples for the bytecode string `code` (the adaptive code
    when `original_code` holds the unspecialised one)."""
    positions = iter(co_positions) if co_positions is not None else iter(())
    if original_code is None:
        original_code = code
    line_number = None
    for offset, start_offset, op, arg in _unpack_opargs(original_code):
        starts_line = False
        if linestarts is not None:
            if offset in linestarts:
                starts_line = True
                local_line = linestarts[offset]
                line_number = None if local_line is None else local_line + line_offset
        pos = Positions(*next(positions, ()))
        deop = _deoptop(op)
        op = code[offset]
        if arg_resolver is not None:
            argval, argrepr = arg_resolver.get_argval_argrepr(op, arg, offset)
            label = arg_resolver.get_label_for_offset(offset)
        else:
            argval, argrepr = arg, ''
            label = None
        ncache = _cache_units(deop)
        cache_info = None
        if ncache:
            for _ in range(ncache):
                next(positions, ())
            fields = _cache_format.get(_all_opname[deop], {})
            cache_info = []
            data_at = offset + 2
            for name, size in fields.items():
                cache_info.append((name, size, code[data_at:data_at + 2 * size]))
                data_at += 2 * size
        yield Instruction(_all_opname[op], op, arg, argval, argrepr, offset, start_offset,
                          starts_line, line_number, label, pos, cache_info)


def get_instructions(x, *, first_line=None, show_caches=None, adaptive=False):
    """Iterator over the Instructions of a function, method, code string or code object.

    `first_line` shifts the reported line numbers so that the code starts at that line."""
    co = _get_code_object(x)
    linestarts = dict(findlinestarts(co))
    line_offset = 0 if first_line is None else first_line - co.co_firstlineno
    original_code = co.co_code
    resolver = ArgResolver(co_consts=co.co_consts, names=co.co_names,
                           varname_from_oparg=co._varname_from_oparg,
                           labels_map=_make_labels_map(original_code))
    return _get_instructions_bytes(_get_code_array(co, adaptive), linestarts, line_offset,
                                   co.co_positions(), original_code, resolver)


# ----------------------------------------------------------------------------- formatting

class _Formatter:
    """Writes disassembly lines: [line][label][offset][marker] OPNAME arg (argrepr)."""

    def __init__(self, file=None, lineno_width=0, offset_width=0, label_width=0,
                 line_offset=0, show_caches=False, positions_width=0):
        self.file = file
        self.lineno_width = lineno_width
        self.offset_width = offset_width
        self.label_width = label_width
        self.show_caches = show_caches
        self.positions_width = positions_width

    def print_instruction(self, instr, mark_as_current=False):
        self._print_line(instr, mark_as_current)
        if self.show_caches and instr.cache_info:
            offset = instr.offset
            for name, size, data in instr.cache_info:
                for unit in range(size):
                    offset += 2
                    text = "%s: %d" % (name, int.from_bytes(data, sys.byteorder)) if unit == 0 else ""
                    cache = Instruction("CACHE", CACHE, 0, None, text, offset, offset,
                                        False, None, None, instr.positions)
                    self._print_line(cache, False)

    @staticmethod
    def _positions_text(positions):
        if positions is None or positions.lineno is None:
            return _NO_LINENO
        parts = ['?' if p is None else str(p) for p in positions]
        return "%s:%s-%s:%s" % (parts[0], parts[2], parts[1], parts[3])

    def _print_line(self, instr, mark_as_current):
        pieces = []
        if self.positions_width:
            pieces.append(self._positions_text(instr.positions).ljust(self.positions_width) + "  ")
        elif self.lineno_width:
            if instr.starts_line:
                text = _NO_LINENO if instr.line_number is None else str(instr.line_number)
                pieces.append(text.rjust(self.lineno_width) + " ")
            else:
                pieces.append(" " * (self.lineno_width + 1))
        label = "" if instr.label is None else "L%d:" % instr.label
        pieces.append(label.rjust(self.label_width))
        if self.offset_width:
            pieces.append(repr(instr.offset).rjust(self.offset_width) + "  ")
        pieces.append(" --> " if mark_as_current else "     ")
        pieces.append(instr.opname)
        if instr.arg is not None:
            overflow = max(0, len(instr.opname) - _OPNAME_WIDTH)
            pieces.append(" " * max(0, _OPNAME_WIDTH - len(instr.opname)))
            pieces.append(" " + repr(instr.arg).rjust(_OPARG_WIDTH - overflow))
            if instr.argrepr:
                pieces.append(" (" + instr.argrepr + ")")
        print("".join(pieces).rstrip(), file=self.file)

    def print_exception_table(self, entries, labels_map):
        if not entries:
            return
        print("ExceptionTable:", file=self.file)
        for e in entries:
            text = "  L%s to L%s -> L%s [%d]" % (labels_map.get(e.start), labels_map.get(e.end),
                                               labels_map.get(e.target), e.depth)
            if e.lasti:
                text += " lasti"
            print(text, file=self.file)


def _lineno_width(linestarts):
    if not linestarts:
        return 0
    lines = [line for line in linestarts.values() if line is not None]
    if not lines:
        return 0
    width = max(3, len(str(max(lines))))
    if None in linestarts.values():
        width = max(width, len(_NO_LINENO))
    return width


def _disassemble_bytes(code, lasti=-1, linestarts=None, *, line_offset=0, exception_entries=(),
                       co_positions=None, original_code=None, arg_resolver=None,
                       show_caches=False, show_offsets=False, show_positions=False,
                       labels_map=None, file=None):
    if original_code is None:
        original_code = code
    if labels_map is None:
        labels_map = _make_labels_map(original_code, exception_entries)
    if arg_resolver is None:
        arg_resolver = ArgResolver(labels_map=labels_map)
    instrs = list(_get_instructions_bytes(code, linestarts, line_offset, co_positions,
                                          original_code, arg_resolver))
    lineno_width = 0
    if linestarts is not None:
        lineno_width = _lineno_width({k: (None if v is None else v + line_offset)
                                      for k, v in linestarts.items()})
    positions_width = 0
    if show_positions and any(i.positions is not None and any(p is not None for p in i.positions)
                              for i in instrs):
        positions_width = max(len(_Formatter._positions_text(i.positions)) for i in instrs)
    offset_width = 0
    if show_offsets:
        offset_width = max(5, len(str(len(code))))
    label_width = 4 + len(str(len(labels_map)))
    fmt = _Formatter(file=file, lineno_width=lineno_width, offset_width=offset_width,
                     label_width=label_width, show_caches=show_caches,
                     positions_width=positions_width)
    first = True
    for instr in instrs:
        if instr.starts_line and not first and (lineno_width or positions_width):
            print(file=file)
        first = False
        current = instr.start_offset <= lasti < instr.end_offset if lasti >= 0 else False
        if current and instr.opname == "EXTENDED_ARG":
            current = instr.offset == lasti
        fmt.print_instruction(instr, current)
    fmt.print_exception_table(exception_entries, labels_map)


def disassemble(co, lasti=-1, *, file=None, show_caches=False, adaptive=False,
                show_offsets=False, show_positions=False):
    """Disassemble a code object; `lasti` marks the current instruction."""
    linestarts = dict(findlinestarts(co))
    entries = _parse_exception_table(co)
    labels_map = _make_labels_map(co.co_code, entries)
    resolver = ArgResolver(co_consts=co.co_consts, names=co.co_names,
                           varname_from_oparg=co._varname_from_oparg, labels_map=labels_map)
    _disassemble_bytes(_get_code_array(co, adaptive), lasti, linestarts,
                       exception_entries=entries, co_positions=co.co_positions(),
                       original_code=co.co_code, arg_resolver=resolver,
                       show_caches=show_caches, show_offsets=show_offsets,
                       show_positions=show_positions, labels_map=labels_map, file=file)


disco = disassemble


def _disassemble_recursive(co, *, file=None, depth=None, show_caches=False, adaptive=False,
                           show_offsets=False, show_positions=False):
    disassemble(co, file=file, show_caches=show_caches, adaptive=adaptive,
                show_offsets=show_offsets, show_positions=show_positions)
    if depth is not None and depth <= 0:
        return
    if depth is not None:
        depth -= 1
    for x in co.co_consts:
        if hasattr(x, 'co_code'):
            print(file=file)
            print("Disassembly of %r:" % (x,), file=file)
            _disassemble_recursive(x, file=file, depth=depth, show_caches=show_caches,
                                   adaptive=adaptive, show_offsets=show_offsets,
                                   show_positions=show_positions)


def _disassemble_str(source, **kwargs):
    _disassemble_recursive(_try_compile(source, '<dis>'), **kwargs)


def dis(x=None, *, file=None, depth=None, show_caches=False, adaptive=False,
        show_offsets=False, show_positions=False):
    """Disassemble classes, methods, functions, generators, code objects, source strings
    or raw bytecode (default: the last traceback)."""
    options = dict(show_caches=show_caches, adaptive=adaptive,
                   show_offsets=show_offsets, show_positions=show_positions)
    if x is None:
        distb(file=file, **options)
        return
    if hasattr(x, '__func__'):
        x = x.__func__
    if hasattr(x, '__code__'):
        x = x.__code__
    elif hasattr(x, 'gi_code'):
        x = x.gi_code
    elif hasattr(x, 'ag_code'):
        x = x.ag_code
    elif hasattr(x, 'cr_code'):
        x = x.cr_code
    if hasattr(x, '__dict__'):
        for name, item in sorted(x.__dict__.items()):
            if isinstance(item, _have_code):
                print("Disassembly of %s:" % name, file=file)
                try:
                    dis(item, file=file, depth=depth, **options)
                except TypeError as msg:
                    print("Sorry:", msg, file=file)
                print(file=file)
    elif hasattr(x, 'co_code'):
        _disassemble_recursive(x, file=file, depth=depth, **options)
    elif isinstance(x, (bytes, bytearray)):
        _disassemble_bytes(x, file=file, show_caches=show_caches,
                           show_offsets=show_offsets, show_positions=show_positions)
    elif isinstance(x, str):
        _disassemble_str(x, file=file, depth=depth, **options)
    else:
        raise TypeError("don't know how to disassemble %s objects" % type(x).__name__)


def _last_traceback():
    exc = getattr(sys, 'last_exc', None)
    if exc is not None:
        return exc.__traceback__
    tb = getattr(sys, 'last_traceback', None)
    if tb is None:
        raise RuntimeError("no last traceback to disassemble")
    return tb


def distb(tb=None, *, file=None, show_caches=False, adaptive=False, show_offsets=False,
          show_positions=False):
    """Disassemble the frame of the innermost traceback entry (default: the last one)."""
    if tb is None:
        tb = _last_traceback()
    while tb.tb_next:
        tb = tb.tb_next
    disassemble(tb.tb_frame.f_code, tb.tb_lasti, file=file, show_caches=show_caches,
                adaptive=adaptive, show_offsets=show_offsets, show_positions=show_positions)


# ----------------------------------------------------------------------------- helpers for modulefinder

_IMPORT_NAME = opmap['IMPORT_NAME']
_STORE_OPS = frozenset([opmap['STORE_NAME'], opmap['STORE_GLOBAL']])


def _find_imports(co):
    """Yield (name, level, fromlist) for the import statements of a code object."""
    consts = co.co_consts
    names = co.co_names
    instrs = [(op, arg) for _, _, op, arg in _unpack_opargs(co.co_code)
              if op != EXTENDED_ARG]
    for i, (op, arg) in enumerate(instrs):
        if op != _IMPORT_NAME or i < 2:
            continue
        level_op, fromlist_op = instrs[i - 2], instrs[i - 1]
        values = []
        for load_op, load_arg in (level_op, fromlist_op):
            if load_op == LOAD_CONST:
                values.append(consts[load_arg])
            elif load_op == LOAD_SMALL_INT:
                values.append(load_arg)
            elif load_op == LOAD_COMMON_CONSTANT:
                values.append(_common_constants[load_arg])
            else:
                break
        else:
            yield names[arg >> 2], values[0], values[1]


def _find_store_names(co):
    """Yield the names stored by STORE_NAME / STORE_GLOBAL."""
    names = co.co_names
    for _, _, op, arg in _unpack_opargs(co.co_code):
        if op in _STORE_OPS:
            yield names[arg]


# ----------------------------------------------------------------------------- Bytecode

class Bytecode:
    """The bytecode of a function, method, string or code object as Instructions.

    Iterating yields Instruction tuples; dis() returns the formatted disassembly."""

    def __init__(self, x, *, first_line=None, current_offset=None, show_caches=False,
                 adaptive=False, show_offsets=False, show_positions=False):
        self.codeobj = co = _get_code_object(x)
        if first_line is None:
            self.first_line = co.co_firstlineno
            self._line_offset = 0
        else:
            self.first_line = first_line
            self._line_offset = first_line - co.co_firstlineno
        self._linestarts = dict(findlinestarts(co))
        self._original_object = x
        self.current_offset = current_offset
        self.exception_entries = _parse_exception_table(co)
        self.show_caches = show_caches
        self.adaptive = adaptive
        self.show_offsets = show_offsets
        self.show_positions = show_positions

    def __iter__(self):
        co = self.codeobj
        resolver = ArgResolver(co_consts=co.co_consts, names=co.co_names,
                               varname_from_oparg=co._varname_from_oparg,
                               labels_map=_make_labels_map(co.co_code))
        return _get_instructions_bytes(_get_code_array(co, self.adaptive), self._linestarts,
                                       self._line_offset, co.co_positions(), co.co_code,
                                       resolver)

    def __repr__(self):
        return "%s(%r)" % (self.__class__.__name__, self._original_object)

    @classmethod
    def from_traceback(cls, tb, *, show_caches=False, adaptive=False):
        """A Bytecode for the innermost frame of a traceback."""
        while tb.tb_next:
            tb = tb.tb_next
        return cls(tb.tb_frame.f_code, current_offset=tb.tb_lasti,
                   show_caches=show_caches, adaptive=adaptive)

    def info(self):
        """Formatted details of the code object."""
        return _format_code_info(self.codeobj)

    def dis(self):
        """The formatted disassembly (as a string)."""
        co = self.codeobj
        offset = -1 if self.current_offset is None else self.current_offset
        labels_map = _make_labels_map(co.co_code, self.exception_entries)
        resolver = ArgResolver(co_consts=co.co_consts, names=co.co_names,
                               varname_from_oparg=co._varname_from_oparg, labels_map=labels_map)
        with io.StringIO() as output:
            _disassemble_bytes(_get_code_array(co, self.adaptive), offset, self._linestarts,
                               line_offset=self._line_offset,
                               exception_entries=self.exception_entries,
                               co_positions=co.co_positions(), original_code=co.co_code,
                               arg_resolver=resolver, show_caches=self.show_caches,
                               show_offsets=self.show_offsets,
                               show_positions=self.show_positions, labels_map=labels_map,
                               file=output)
            return output.getvalue()


def main(args=None):
    import argparse

    parser = argparse.ArgumentParser()
    parser.add_argument('-C', '--show-caches', action='store_true',
                        help='show inline caches')
    parser.add_argument('-O', '--show-offsets', action='store_true',
                        help='show instruction offsets')
    parser.add_argument('-P', '--show-positions', action='store_true',
                        help='show instruction positions')
    parser.add_argument('-S', '--specialized', action='store_true',
                        help='show specialized bytecode')
    parser.add_argument('infile', nargs='?', default='-')
    args = parser.parse_args(args=args)
    if args.infile == '-':
        name = '<stdin>'
        source = sys.stdin.buffer.read()
    else:
        name = args.infile
        with open(args.infile, 'rb') as infile:
            source = infile.read()
    code = compile(source, name, "exec")
    dis(code, show_caches=args.show_caches, adaptive=args.specialized,
        show_offsets=args.show_offsets, show_positions=args.show_positions)


if __name__ == "__main__":
    main()
