"""Opcode tables of the CPython 3.15 instruction set (names, numbers, argument kinds,
inline-cache layout), as used by dis and other bytecode tools.

The numbers are the public ABI fixed by Include/opcode_ids.h; the tables are provided by
the _opcode builtin module.
"""

__all__ = ["cmp_op", "stack_effect", "hascompare", "opname", "opmap",
           "HAVE_ARGUMENT", "EXTENDED_ARG", "hasarg", "hasconst", "hasname",
           "hasjump", "hasjrel", "hasjabs", "hasfree", "haslocal", "hasexc"]

import _opcode
from _opcode import stack_effect

cmp_op = ('<', '<=', '==', '!=', '>', '>=')


def _build_tables():
    specialized = {}
    for base, names in _opcode._specializations().items():
        for name in names:
            specialized[name] = base
    regular = {}
    special = {}
    for name, number in _opcode._opcodes():
        if name in specialized:
            special[name] = number
        else:
            regular[name] = number
    return regular, special


opmap, _specialized_opmap = _build_tables()
del _build_tables

_specializations = _opcode._specializations()

opname = ['<%r>' % (op,) for op in range(max(opmap.values()) + 1)]
for _name, _op in [*opmap.items(), *_specialized_opmap.items()]:
    opname[_op] = _name
del _name, _op

HAVE_ARGUMENT = 41
EXTENDED_ARG = opmap['EXTENDED_ARG']
MIN_INSTRUMENTED_OPCODE = 233
ENABLE_SPECIALIZATION = _opcode.ENABLE_SPECIALIZATION
ENABLE_SPECIALIZATION_FT = _opcode.ENABLE_SPECIALIZATION_FT

hasarg = [op for op in opmap.values() if _opcode.has_arg(op)]
hasconst = [op for op in opmap.values() if _opcode.has_const(op)]
hasname = [op for op in opmap.values() if _opcode.has_name(op)]
hasjump = [op for op in opmap.values() if _opcode.has_jump(op)]
hasjrel = hasjump  # kept for backwards compatibility
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

_cache_format = {name: dict(fields)
                 for name, fields in _opcode._cache_format().items()}
_inline_cache_entries = {name: sum(fields.values())
                         for name, fields in _cache_format.items()}
