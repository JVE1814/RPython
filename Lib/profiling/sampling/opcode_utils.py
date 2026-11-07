"""Opcode naming helpers for the --native/opcode profiling views."""

import opcode

__all__ = ('get_opcode_info', 'format_opcode')


def _base_name(name):
    """The un-specialized opcode a specialized variant derives from."""
    specializations = getattr(opcode, '_specializations', None)
    if specializations:
        for base, variants in specializations.items():
            if name in variants:
                return base
    return name


def get_opcode_info(op):
    """Describe opcode number *op*: its name, its base (un-specialized) name,
    and whether it is a specialized variant."""
    opname = None
    if 0 <= op < len(opcode.opname):
        candidate = opcode.opname[op]
        if not candidate.startswith('<'):
            opname = candidate
    if opname is None:
        specialized = getattr(opcode, '_specialized_opmap', {})
        for name, value in specialized.items():
            if value == op:
                opname = name
                break
    if opname is None:
        opname = f'<{op}>'
        return {'opname': opname, 'base_opname': opname, 'is_specialized': False}
    base = _base_name(opname)
    return {
        'opname': opname,
        'base_opname': base,
        'is_specialized': base != opname,
    }


def format_opcode(op):
    """A human-readable label for opcode *op*; specialized variants show the
    base opcode in parentheses."""
    info = get_opcode_info(op)
    if info['is_specialized']:
        return f"{info['opname']} ({info['base_opname']})"
    return info['opname']
