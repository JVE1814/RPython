
"""
opcode module - potentially shared between dis and other modules which
operate on bytecodes (e.g. peephole optimizers).

This implementation's bytecode is its own (not CPython's): every instruction
is one 32-bit little-endian word, the low 8 bits being the opcode and the
high 24 bits the argument (EXTENDED_ARG prefixes supply higher bits).  Jump
arguments are absolute instruction indices.  There are no inline cache
entries.
"""


__all__ = ["cmp_op", "stack_effect", "hascompare", "opname", "opmap",
           "HAVE_ARGUMENT", "EXTENDED_ARG", "hasarg", "hasconst", "hasname",
           "hasjump", "hasjrel", "hasjabs", "hasfree", "haslocal", "hasexc"]

import _opcode
from _opcode import stack_effect

cmp_op = ('<', '<=', '==', '!=', '>', '>=')

opname = list(_opcode._opnames)
opmap = {name: op for op, name in enumerate(opname)}

# Every opcode is "below" HAVE_ARGUMENT only by convention in CPython; here
# use has_arg() / hasarg instead.
HAVE_ARGUMENT = 0
EXTENDED_ARG = opmap['EXTENDED_ARG']
MIN_INSTRUMENTED_OPCODE = 256

_specializations = {}
_specialized_opmap = {}
_inline_cache_entries = {name: 0 for name in opname}
_cache_format = {}

hasarg = [op for op in opmap.values() if _opcode.has_arg(op)]
hasconst = [op for op in opmap.values() if _opcode.has_const(op)]
hasname = [op for op in opmap.values() if _opcode.has_name(op)]
hasjump = [op for op in opmap.values() if _opcode.has_jump(op)]
hasjrel = hasjump  # for backward compatibility
hasjabs = []
hasfree = [op for op in opmap.values() if _opcode.has_free(op)]
haslocal = [op for op in opmap.values() if _opcode.has_local(op)]
hasexc = [op for op in opmap.values() if _opcode.has_exc(op)]
hascompare = [opmap["COMPARE_OP"]]

_intrinsic1_descs = _opcode.get_intrinsic1_descs()
_intrinsic2_descs = _opcode.get_intrinsic2_descs()
_special_method_names = _opcode.get_special_method_names()
_common_constants = _opcode.get_common_constants()
_nb_ops = _opcode.get_nb_ops()

# Instruction size in bytes (co_code offsets).
_INSTRUCTION_SIZE = 4
