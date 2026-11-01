"""Turn bytecode into a readable listing.

The bytecode of this implementation is not CPython's: every instruction is
one 32-bit little-endian word holding the opcode in its low byte and the
argument in the upper 24 bits (EXTENDED_ARG prefixes contribute further
high bits).  Offsets therefore grow in steps of 4, jump arguments are
absolute instruction indices, and there are neither inline caches nor
specialised instructions.
"""

import collections
import io
import sys
import types

from opcode import *
from opcode import (
    __all__ as _opcode_names,
    _intrinsic1_descs,
    _intrinsic2_descs,
    _special_method_names,
    _common_constants,
    _nb_ops,
    _INSTRUCTION_SIZE,
)
import _opcode
from _opcode import stack_effect

__all__ = ["code_info", "dis", "disassemble", "distb", "disco",
           "findlinestarts", "findlabels", "show_code",
           "get_instructions", "Instruction", "Bytecode"] + _opcode_names
del _opcode_names

# no specialised instructions: nothing to map back
deoptmap = {}

FUNCTION_ATTR_FLAGS = ('defaults', 'kwdefaults', 'annotations', 'closure',
                       'annotate')

_OPNAME_WIDTH = 20
_OPARG_WIDTH = 5
_NO_LINENO = '  --'

# co_flags bits and their names (also used by inspect for its CO_* names)
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
    """The names of the bits set in `flags`, comma separated (unknown bits
    in hex)."""
    parts = []
    bit = 1
    remaining = flags
    while remaining and bit <= 1 << 31:
        if remaining & bit:
            parts.append(COMPILER_FLAG_NAMES.get(bit, hex(bit)))
            remaining &= ~bit
        bit <<= 1
    if remaining or not parts:
        parts.append(hex(remaining))
    return ", ".join(parts)


class _UnknownValue:
    """Placeholder for argument values that cannot be determined."""

    def __repr__(self):
        return "<unknown>"


UNKNOWN = _UnknownValue()


# ---------------------------------------------------------------- records

Positions = collections.namedtuple(
    'Positions', ['lineno', 'end_lineno', 'col_offset', 'end_col_offset'],
    defaults=[None, None, None, None])

_InstructionFields = collections.namedtuple(
    '_InstructionFields',
    ['opname', 'opcode', 'arg', 'argval', 'argrepr', 'offset',
     'start_offset', 'starts_line', 'line_number', 'label', 'positions',
     'cache_info'],
    defaults=[None, None, None])


class Instruction(_InstructionFields):
    """One decoded instruction.

    opname/opcode name the operation; arg is the raw argument (None for
    operations without one), argval its meaning (a constant, name, jump
    target offset, ...) and argrepr a readable rendering of it.  offset is
    the byte offset of the instruction and start_offset that of its first
    EXTENDED_ARG prefix (if any).  starts_line tells whether a new source
    line begins here and line_number gives that line; label numbers jump
    targets; positions is a Positions tuple; cache_info is always None
    (there are no inline caches).
    """

    @property
    def oparg(self):
        return self.arg

    @property
    def baseopcode(self):
        return self.opcode

    @property
    def baseopname(self):
        return opname[self.opcode]

    @property
    def cache_offset(self):
        return self.offset + _INSTRUCTION_SIZE

    @property
    def end_offset(self):
        return self.offset + _INSTRUCTION_SIZE

    @property
    def jump_target(self):
        return _jump_destination(self.opcode, self.arg)

    @property
    def is_jump_target(self):
        return self.label is not None

    def __str__(self):
        buffer = io.StringIO()
        Formatter(file=buffer).print_instruction(self, False)
        return buffer.getvalue()


_ExceptionTableEntryBase = collections.namedtuple(
    '_ExceptionTableEntryBase', 'start end target depth lasti')


class _ExceptionTableEntry(_ExceptionTableEntryBase):
    # the label numbers are attached by _make_labels_map()
    start_label = end_label = target_label = None


# ---------------------------------------------------------------- decoding

def _jump_destination(op, arg):
    if arg is not None and op in hasjump:
        return arg * _INSTRUCTION_SIZE
    return None


def _decode(code):
    """Yield (offset, start_offset, opcode, arg) for each word of `code`."""
    size = _INSTRUCTION_SIZE
    usable = len(code) - len(code) % size
    nop = opmap['NOP']
    carried = 0
    prefix_start = None
    for offset in range(0, usable, size):
        word = int.from_bytes(code[offset:offset + size], 'little')
        op = word & 0xFF
        if op >= len(opname):
            op = nop
        start = offset if prefix_start is None else prefix_start
        if op in hasarg:
            arg = (word >> 8) | carried
        else:
            arg = None
        if op == EXTENDED_ARG:
            carried = arg << 24
            if prefix_start is None:
                prefix_start = offset
        else:
            carried = 0
            prefix_start = None
        yield offset, start, op, arg


def findlabels(code):
    """Offsets in the raw bytecode `code` that are targets of jumps."""
    found = []
    for _, _, op, arg in _decode(code):
        target = _jump_destination(op, arg)
        if target is not None and target not in found:
            found.append(target)
    return found


def findlinestarts(code):
    """Yield (offset, line) where the source line changes (line may be
    None for instructions without a line)."""
    previous = object()
    for start, _end, line in code.co_lines():
        if line != previous or (line is None) != (previous is None):
            previous = line
            yield start, line


def _parse_exception_table(code):
    try:
        raw = _opcode._exception_table(code)
    except TypeError:
        return []
    size = _INSTRUCTION_SIZE
    return [_ExceptionTableEntry(start * size, end * size, target * size,
                                 depth, lasti)
            for start, end, target, depth, lasti in raw]


def _make_labels_map(original_code, exception_entries=()):
    """Number the jump targets and exception-table boundaries in offset
    order (L1, L2, ...); returns {offset: label}."""
    marked = set(findlabels(original_code))
    for entry in exception_entries:
        marked.update((entry.start, entry.end, entry.target))
    labels = {offset: number for number, offset in enumerate(sorted(marked), 1)}
    for entry in exception_entries:
        entry.start_label = labels[entry.start]
        entry.end_label = labels[entry.end]
        entry.target_label = labels[entry.target]
    return labels


def _localsplus_lookup(co):
    try:
        names = _opcode._localsplusnames(co)
    except TypeError:
        names = co.co_varnames + co.co_cellvars + co.co_freevars
    return names.__getitem__


# ---------------------------------------------------------------- arguments

def _cmp_description(arg):
    index = arg & 15
    name = cmp_op[index] if index < len(cmp_op) else arg
    shown = "bool(%s)" % name if arg & 16 else name
    return name, shown


class ArgResolver:
    """Computes argval/argrepr of instructions from the tables of a code
    object (any of which may be missing)."""

    def __init__(self, co_consts=None, names=None, varname_from_oparg=None,
                 labels_map=None):
        self.co_consts = co_consts
        self.names = names
        self.varname_from_oparg = varname_from_oparg
        self.labels_map = labels_map or {}

    def offset_from_jump_arg(self, op, arg, offset):
        return _jump_destination(op, arg)

    def get_label_for_offset(self, offset):
        return self.labels_map.get(offset)

    def _name(self, index):
        if self.names is None:
            return UNKNOWN, ''
        value = self.names[index]
        return value, value

    def get_argval_argrepr(self, op, arg, offset):
        if arg is None:
            return None, ''
        if op in hasconst:
            if self.co_consts is None:
                return UNKNOWN, ''
            value = self.co_consts[arg]
            return value, repr(value)
        if op in hasname:
            return self._describe_name(op, arg)
        if op in hasjump:
            target = self.offset_from_jump_arg(op, arg, offset)
            return target, "to L%s" % (self.get_label_for_offset(target),)
        if op in haslocal or op in hasfree:
            if self.varname_from_oparg is None:
                return arg, ''
            value = self.varname_from_oparg(arg)
            return value, value
        describe = _ARG_DESCRIBERS.get(opname[op])
        if describe is not None:
            return describe(arg)
        return arg, ''

    def _describe_name(self, op, arg):
        name = opname[op]
        if name == 'LOAD_SUPER_ATTR':
            value, shown = self._name(arg >> 2)
            if arg & 1 and shown:
                shown += " + NULL|self"
            return value, shown
        if name == 'IMPORT_NAME':
            value, shown = self._name(arg & ((1 << 23) - 1))
            if (arg >> 23) & 1 and shown:
                shown += " + lazy"
            return value, shown
        return self._name(arg)


def _table_entry(table, arg, default=''):
    return table[arg] if 0 <= arg < len(table) else default


_ARG_DESCRIBERS = {
    'COMPARE_OP': _cmp_description,
    'IS_OP': lambda arg: (arg, 'is not' if arg else 'is'),
    'CONTAINS_OP': lambda arg: (arg, 'not in' if arg else 'in'),
    'CONVERT_VALUE': lambda arg: (
        _table_entry((None, str, repr, ascii), arg, arg),
        _table_entry(('', 'str', 'repr', 'ascii'), arg)),
    'SET_FUNCTION_ATTRIBUTE': lambda arg: (
        arg, ', '.join(name for bit, name in enumerate(FUNCTION_ATTR_FLAGS)
                       if arg & (1 << bit))),
    'BINARY_OP': lambda arg: (
        arg, _table_entry(_nb_ops, arg, (None, ''))[1]),
    'CALL_INTRINSIC_1': lambda arg: (arg, _table_entry(_intrinsic1_descs, arg)),
    'CALL_INTRINSIC_2': lambda arg: (arg, _table_entry(_intrinsic2_descs, arg)),
    'LOAD_COMMON_CONSTANT': lambda arg: (
        _table_entry(_common_constants, arg, arg),
        str(_table_entry(_common_constants, arg, arg))),
}


# ---------------------------------------------------------------- instructions

def _get_instructions_bytes(code, linestarts=None, line_offset=0,
                            co_positions=None, original_code=None,
                            arg_resolver=None):
    """Yield Instruction records for the raw bytecode `code`."""
    code = original_code or code
    spans = list(co_positions) if co_positions is not None else []
    # co_positions() reports one entry per two bytes of code
    per_instruction = _INSTRUCTION_SIZE // 2
    line = None
    for offset, start, op, arg in _decode(code):
        starts_line = False
        if linestarts is not None:
            if offset in linestarts:
                starts_line = True
                line = linestarts[offset]
            line_number = None if line is None else line + line_offset
        else:
            line_number = None
        index = offset // _INSTRUCTION_SIZE * per_instruction
        positions = Positions(*spans[index]) if index < len(spans) else Positions()
        if arg_resolver is not None:
            argval, argrepr = arg_resolver.get_argval_argrepr(op, arg, offset)
            label = arg_resolver.get_label_for_offset(offset)
        else:
            argval, argrepr, label = arg, repr(arg) if arg is not None else '', None
        yield Instruction(opname[op], op, arg, argval, argrepr, offset, start,
                          starts_line, line_number, label, positions, None)


def _code_listing(co, line_offset=0, exception_entries=None):
    """(instructions, exception entries, labels) of a code object."""
    if exception_entries is None:
        exception_entries = _parse_exception_table(co)
    labels = _make_labels_map(co.co_code, exception_entries)
    resolver = ArgResolver(co.co_consts, co.co_names, _localsplus_lookup(co),
                           labels)
    instructions = _get_instructions_bytes(
        co.co_code, linestarts=dict(findlinestarts(co)),
        line_offset=line_offset, co_positions=co.co_positions(),
        arg_resolver=resolver)
    return instructions, exception_entries, labels


def get_instructions(x, *, first_line=None, show_caches=None, adaptive=False):
    """Iterate over the Instructions of a function, method, code object or
    source string.  `first_line` renumbers the lines so that the code
    starts at that line."""
    co = _get_code_object(x)
    offset = 0 if first_line is None else first_line - co.co_firstlineno
    # only jump targets are labelled here (no exception table boundaries)
    instructions, _, _ = _code_listing(co, offset, ())
    return instructions


def _find_imports(co):
    """Yield (name, level, fromlist) for the imports of code object `co`."""
    pending = []
    for instr in _get_instructions_bytes(
            co.co_code, arg_resolver=ArgResolver(co.co_consts, co.co_names)):
        if instr.opname == 'IMPORT_NAME':
            if len(pending) >= 2:
                level, fromlist = pending[-2], pending[-1]
                yield instr.argval, level, fromlist
        if instr.opname in ('LOAD_CONST', 'LOAD_SMALL_INT'):
            pending.append(instr.argval)
        elif instr.opname != 'EXTENDED_ARG':
            pending = []


def _find_store_names(co):
    """Yield the global/module names stored by code object `co`."""
    for instr in _get_instructions_bytes(
            co.co_code, arg_resolver=ArgResolver(co.co_consts, co.co_names)):
        if instr.opname in ('STORE_NAME', 'STORE_GLOBAL'):
            yield instr.argval


# ---------------------------------------------------------------- formatting

class Formatter:
    """Writes Instructions (and exception tables) as listing lines.

    The widths of the line-number (or position), label and offset columns
    are given by the caller; 0 leaves a column out."""

    def __init__(self, file=None, lineno_width=0, offset_width=0, label_width=0,
                 line_offset=0, show_caches=False, *, show_positions=False):
        self.file = file
        self.lineno_width = lineno_width
        self.offset_width = offset_width
        self.label_width = label_width
        self.show_caches = show_caches
        self.show_positions = show_positions

    def _location_column(self, instr):
        width = self.lineno_width
        if self.show_positions:
            spans = instr.positions
            if not spans:
                return ' ' * width
            if all(value is None for value in spans):
                text = _NO_LINENO
            else:
                shown = ['?' if value is None else value for value in spans]
                text = "%s:%s-%s:%s" % (shown[0], shown[2], shown[1], shown[3])
            return text.ljust(width)
        if not instr.starts_line:
            return ' ' * width
        if instr.line_number is None:
            return '--'.rjust(width)
        return str(instr.line_number).rjust(width)

    def print_instruction(self, instr, mark_as_current=False):
        self.print_instruction_line(instr, mark_as_current)

    def print_instruction_line(self, instr, mark_as_current):
        if self.lineno_width and instr.starts_line and instr.offset > 0:
            print(file=self.file)
        columns = []
        if self.lineno_width:
            columns.append(self._location_column(instr))
        if instr.label is not None:
            columns.append(("L%d:" % instr.label).rjust(self.label_width))
        else:
            columns.append(' ' * self.label_width)
        if self.offset_width > 0:
            columns.append(repr(instr.offset).rjust(self.offset_width) + '  ')
        columns.append('-->' if mark_as_current else '   ')
        columns.append(instr.opname.ljust(_OPNAME_WIDTH))
        if instr.arg is not None:
            overflow = max(0, len(instr.opname) - _OPNAME_WIDTH)
            columns.append(repr(instr.arg).rjust(_OPARG_WIDTH - overflow))
            if instr.argrepr:
                columns.append('(%s)' % instr.argrepr)
        print(' '.join(columns).rstrip(), file=self.file)

    def print_exception_table(self, exception_entries):
        if not exception_entries:
            return
        print("ExceptionTable:", file=self.file)
        for entry in exception_entries:
            suffix = " lasti" if entry.lasti else ""
            print("  L%s to L%s -> L%s [%d]%s" % (
                entry.start_label, entry.end_label, entry.target_label,
                entry.depth, suffix), file=self.file)


def _lineno_width(linestarts):
    lines = [line for line in linestarts.values() if line is not None]
    if not lines:
        return 0
    width = max(3, len(str(max(lines))))
    if None in linestarts.values():
        width = max(width, len(_NO_LINENO))
    return width


def _positions_width(co):
    widest = 0
    any_known = False
    for spans in co.co_positions():
        if any(isinstance(value, int) for value in spans):
            any_known = True
        widest = max(widest, sum(1 if value is None else len(str(value))
                                 for value in spans))
    if not any_known:
        return 0
    # "L:C-L:C" has three separators; one more column of padding
    return 1 + max(len(_NO_LINENO), widest + 3)


def _offset_width(code, show_offsets):
    if not show_offsets:
        return 0
    return len(str(max(len(code) - 2, 9999)))


def _render(co, file, lasti=-1, line_offset=0, show_caches=False,
            show_offsets=False, show_positions=False):
    instructions, entries, labels = _code_listing(co, line_offset)
    if show_positions:
        location_width = _positions_width(co)
    else:
        location_width = _lineno_width(dict(findlinestarts(co)))
    formatter = Formatter(file=file, lineno_width=location_width,
                          offset_width=_offset_width(co.co_code, show_offsets),
                          label_width=4 + len(str(len(labels))),
                          line_offset=line_offset, show_caches=show_caches,
                          show_positions=show_positions)
    print_instructions(instructions, entries, formatter, lasti)


def print_instructions(instrs, exception_entries, formatter, lasti=-1):
    for instr in instrs:
        current = instr.offset <= lasti < instr.offset + _INSTRUCTION_SIZE
        formatter.print_instruction(instr, current)
    formatter.print_exception_table(exception_entries)


# ---------------------------------------------------------------- entry points

def _try_compile(source, name):
    """Compile `source` as an expression if possible, else as statements."""
    try:
        return compile(source, name, 'eval')
    except SyntaxError:
        pass
    return compile(source, name, 'exec')


def _unwrap(x):
    """The code-bearing object behind methods, functions, generators and
    coroutines (x itself otherwise)."""
    x = getattr(x, '__func__', x)
    for attribute in ('__code__', 'gi_code', 'ag_code', 'cr_code'):
        inner = getattr(x, attribute, None)
        if inner is not None:
            return inner
    return x


def _get_code_object(x):
    x = _unwrap(x)
    if isinstance(x, str):
        x = _try_compile(x, "<disassembly>")
    if hasattr(x, 'co_code'):
        return x
    raise TypeError("don't know how to disassemble %s objects"
                    % type(x).__name__)


def disassemble(co, lasti=-1, *, file=None, show_caches=False, adaptive=False,
                show_offsets=False, show_positions=False):
    """Print the listing of code object `co`, marking the instruction at
    byte offset `lasti`."""
    _render(co, file, lasti, show_caches=show_caches,
            show_offsets=show_offsets, show_positions=show_positions)


disco = disassemble


def _disassemble_recursive(co, *, file=None, depth=None, show_caches=False,
                           adaptive=False, show_offsets=False,
                           show_positions=False):
    disassemble(co, file=file, show_caches=show_caches,
                show_offsets=show_offsets, show_positions=show_positions)
    if depth is not None:
        if depth <= 0:
            return
        depth -= 1
    for const in co.co_consts:
        if hasattr(const, 'co_code'):
            print(file=file)
            print("Disassembly of %r:" % (const,), file=file)
            _disassemble_recursive(const, file=file, depth=depth,
                                   show_caches=show_caches,
                                   show_offsets=show_offsets,
                                   show_positions=show_positions)


_HAS_CODE = (types.MethodType, types.FunctionType, types.CodeType,
             classmethod, staticmethod, type)


def dis(x=None, *, file=None, depth=None, show_caches=False, adaptive=False,
        show_offsets=False, show_positions=False):
    """Disassemble `x`: a function, method, class or module (each of its
    functions), code object, generator/coroutine, source string or raw
    bytecode.  Without `x`, the last traceback is disassembled."""
    options = dict(show_caches=show_caches, adaptive=adaptive,
                   show_offsets=show_offsets, show_positions=show_positions)
    if x is None:
        distb(file=file, **options)
        return
    x = _unwrap(x)
    if hasattr(x, '__dict__'):
        for name, member in sorted(x.__dict__.items()):
            if isinstance(member, _HAS_CODE):
                print("Disassembly of %s:" % name, file=file)
                try:
                    dis(member, file=file, depth=depth, **options)
                except TypeError as exc:
                    print("Sorry:", exc, file=file)
                print(file=file)
    elif hasattr(x, 'co_code'):
        _disassemble_recursive(x, file=file, depth=depth, **options)
    elif isinstance(x, (bytes, bytearray)):
        labels = _make_labels_map(x)
        formatter = Formatter(file=file,
                              offset_width=_offset_width(x, show_offsets),
                              label_width=4 + len(str(len(labels))),
                              show_caches=show_caches)
        instructions = _get_instructions_bytes(
            x, arg_resolver=ArgResolver(labels_map=labels))
        print_instructions(instructions, (), formatter)
    elif isinstance(x, str):
        _disassemble_recursive(_try_compile(x, '<dis>'), file=file,
                               depth=depth, **options)
    else:
        raise TypeError("don't know how to disassemble %s objects"
                        % type(x).__name__)


def distb(tb=None, *, file=None, show_caches=False, adaptive=False,
          show_offsets=False, show_positions=False):
    """Disassemble the innermost frame of traceback `tb` (default: the last
    unhandled exception), marking the failing instruction."""
    if tb is None:
        exc = getattr(sys, 'last_exc', None)
        if exc is not None:
            tb = exc.__traceback__
        else:
            tb = getattr(sys, 'last_traceback', None)
        if tb is None:
            raise RuntimeError("no last traceback to disassemble")
    while tb.tb_next:
        tb = tb.tb_next
    disassemble(tb.tb_frame.f_code, tb.tb_lasti, file=file,
                show_caches=show_caches, show_offsets=show_offsets,
                show_positions=show_positions)


_INFO_FIELDS = (
    ("Name:              %s", 'co_name'),
    ("Filename:          %s", 'co_filename'),
    ("Argument count:    %s", 'co_argcount'),
    ("Positional-only arguments: %s", 'co_posonlyargcount'),
    ("Kw-only arguments: %s", 'co_kwonlyargcount'),
    ("Number of locals:  %s", 'co_nlocals'),
    ("Stack size:        %s", 'co_stacksize'),
)

_INFO_TABLES = (
    ("Constants:", 'co_consts', "%4d: %r"),
    ("Names:", 'co_names', "%4d: %s"),
    ("Variable names:", 'co_varnames', "%4d: %s"),
    ("Free variables:", 'co_freevars', "%4d: %s"),
    ("Cell variables:", 'co_cellvars', "%4d: %s"),
)


def _format_code_info(co):
    lines = [template % (getattr(co, attribute),)
             for template, attribute in _INFO_FIELDS]
    lines.append("Flags:             %s" % pretty_flags(co.co_flags))
    for title, attribute, row in _INFO_TABLES:
        values = getattr(co, attribute)
        if values:
            lines.append(title)
            lines.extend(row % (index, value) for index, value in enumerate(values))
    return "\n".join(lines)


def code_info(x):
    """A multi-line description of the code object behind `x`."""
    return _format_code_info(_get_code_object(x))


def show_code(co, *, file=None):
    """Print code_info(co) to `file` (default stdout)."""
    print(code_info(co), file=file)


class Bytecode:
    """The instructions of a function, method, code object or source string.

    Iterating yields Instruction records; dis() returns the listing as a
    string and info() the code_info() text."""

    def __init__(self, x, *, first_line=None, current_offset=None,
                 show_caches=False, adaptive=False, show_offsets=False,
                 show_positions=False):
        self.codeobj = co = _get_code_object(x)
        self._original_object = x
        if first_line is None:
            self.first_line = co.co_firstlineno
            self._line_offset = 0
        else:
            self.first_line = first_line
            self._line_offset = first_line - co.co_firstlineno
        self.current_offset = current_offset
        self.exception_entries = _parse_exception_table(co)
        self.show_caches = show_caches
        self.adaptive = adaptive
        self.show_offsets = show_offsets
        self.show_positions = show_positions

    def __iter__(self):
        instructions, _, _ = _code_listing(self.codeobj, self._line_offset,
                                           self.exception_entries)
        return instructions

    def __repr__(self):
        return "%s(%r)" % (type(self).__name__, self._original_object)

    @classmethod
    def from_traceback(cls, tb, *, show_caches=False, adaptive=False):
        """A Bytecode for the innermost frame of `tb`, marking its current
        instruction."""
        while tb.tb_next:
            tb = tb.tb_next
        return cls(tb.tb_frame.f_code, current_offset=tb.tb_lasti,
                   show_caches=show_caches, adaptive=adaptive)

    def info(self):
        return _format_code_info(self.codeobj)

    def dis(self):
        lasti = -1 if self.current_offset is None else self.current_offset
        with io.StringIO() as output:
            _render(self.codeobj, output, lasti, self._line_offset,
                    show_caches=self.show_caches,
                    show_offsets=self.show_offsets,
                    show_positions=self.show_positions)
            return output.getvalue()


def main(args=None):
    import argparse
    parser = argparse.ArgumentParser(color=True)
    parser.add_argument('-C', '--show-caches', action='store_true',
                        help='show inline caches')
    parser.add_argument('-O', '--show-offsets', action='store_true',
                        help='show instruction offsets')
    parser.add_argument('-P', '--show-positions', action='store_true',
                        help='show instruction positions')
    parser.add_argument('-S', '--specialized', action='store_true',
                        help='show specialized bytecode')
    parser.add_argument('infile', nargs='?', default='-')
    options = parser.parse_args(args=args)
    if options.infile == '-':
        name, source = '<stdin>', sys.stdin.buffer.read()
    else:
        name = options.infile
        with open(name, 'rb') as stream:
            source = stream.read()
    dis(compile(source, name, "exec"), show_caches=options.show_caches,
        adaptive=options.specialized, show_offsets=options.show_offsets,
        show_positions=options.show_positions)


if __name__ == "__main__":
    main()
