"""Memory layout of ctypes Structures and Unions.

The _ctypes metaclasses call get_layout() whenever ``_fields_`` is assigned.
It returns the field descriptors (CField objects) plus the total size,
alignment and PEP 3118 format string of the new type.

Two layout algorithms are implemented:

* "gcc-sysv": the System V ABI as implemented by GCC/Clang.  Bit fields are
  packed into the running bit position unless they would cross a boundary of
  their declared type's storage unit.
* "ms": the Microsoft Visual C++ rules.  A run of bit fields of the same
  declared size shares storage units of that size; any other field starts a
  new unit.  This is also the default when ``_pack_`` is given.
"""

import sys
import warnings

from _ctypes import CField, buffer_info
import ctypes


class StructUnionLayout:
    __slots__ = ("fields", "size", "align", "format_spec")

    def __init__(self, fields, size, align, format_spec):
        self.fields = fields
        self.size = size
        self.align = align
        self.format_spec = format_spec


def _round_up(value, multiple):
    if multiple <= 1:
        return value
    return -(-value // multiple) * multiple


def _non_negative_int(cls, name):
    value = getattr(cls, name, None)
    if value is None:
        return None
    if not isinstance(value, int) or value < 0:
        raise ValueError(f"{name} must be a non-negative integer")
    return value


_BITFIELD_CODES = frozenset("bBhHiIlLqQ?")


def _bitfield_capable(tp):
    code = getattr(tp, "_type_", None)
    return (isinstance(code, str) and code in _BITFIELD_CODES
            and isinstance(tp, type(ctypes.c_int)))


def _field_format(tp):
    """PEP 3118 fragment for one field of type ``tp``."""
    fmt, ndim, shape = buffer_info(tp)
    if ndim:
        return "(%s)%s" % (",".join(str(n) for n in shape), fmt)
    return fmt


def _padding(nbytes):
    if nbytes <= 0:
        return ""
    if nbytes == 1:
        return "x"
    return f"{nbytes}x"


def get_layout(cls, input_fields, is_struct, base, base_size=0, base_align=0,
               base_length=0):
    """Compute the layout of ``cls`` with ``_fields_ = input_fields``."""
    # Class options are validated before the fields themselves.
    if "_pack_" in cls.__dict__ or hasattr(cls, "_pack_"):
        pack = getattr(cls, "_pack_")
        if not isinstance(pack, int) or pack < 0:
            raise ValueError("_pack_ must be a non-negative integer")
        if pack > 0x7FFFFFFF:
            raise ValueError("_pack_ is too large")
    else:
        pack = None
    align_override = _non_negative_int(cls, "_align_") or 0

    layout = getattr(cls, "_layout_", None)
    if layout is None:
        if pack:
            if sys.platform != "win32":
                warnings.warn(
                    f"Due to '_pack_', the '{cls.__name__}' Structure will use "
                    "memory layout compatible with MSVC (Windows). If this is "
                    "intended, set _layout_ to 'ms'. The implicit default is "
                    "deprecated and slated to become an error in Python 3.19.",
                    DeprecationWarning, stacklevel=2)
            layout = "ms"
        elif sys.platform == "win32":
            layout = "ms"
        else:
            layout = "gcc-sysv"
    elif layout not in ("ms", "gcc-sysv"):
        raise ValueError(f"unknown _layout_: {layout!r}")
    is_ms = layout == "ms"

    swapped = hasattr(cls, "_swappedbytes_")
    big_endian = (sys.byteorder == "big") != swapped

    try:
        input_fields = list(input_fields)
    except TypeError:
        raise TypeError("'_fields_' must be a sequence of (name, C type) pairs")

    # ---- per-field validation
    prepared = []
    for index, desc in enumerate(input_fields):
        if not isinstance(desc, tuple) or len(desc) not in (2, 3):
            raise TypeError("'_fields_' must be a sequence of (name, C type) pairs")
        name, tp = desc[0], desc[1]
        if not isinstance(name, str):
            raise TypeError(f"field name must be str, not {type(name).__name__}")
        if type(name) is not str:
            name = str.__str__(name)
        try:
            size = ctypes.sizeof(tp)
            align = ctypes.alignment(tp)
            if not isinstance(tp, type):
                raise TypeError
        except TypeError:
            raise TypeError(
                f"second item in _fields_ tuple (index {index}) must be a C type")
        bits = None
        if len(desc) == 3:
            bits = desc[2]
            if not isinstance(bits, int) or isinstance(bits, bool) and False:
                raise TypeError("bit size must be an integer")
            if bits <= 0 or bits > size * 8:
                raise ValueError(f"number of bits invalid for bit field {name!r}")
            if not _bitfield_capable(tp):
                raise TypeError(f"bit fields not allowed for type {tp.__name__}")
        prepared.append((name, tp, size, align, bits))

    if not is_struct:
        return _union_layout(cls, prepared, base_size, base_align, base_length,
                             pack, align_override, big_endian)

    # ---- structure layout
    fields = []
    format_parts = []
    has_bitfield = False
    max_align = max(base_align, 1)
    pos_bits = base_size * 8           # next free bit
    fmt_pos = base_size                # bytes described by format_parts
    if base_size:
        format_parts.append(_padding(base_size))
    # MS bit-field run state: (unit_offset_bytes, unit_size, bits_used)
    run = None

    for i, (name, tp, size, align, bits) in enumerate(prepared):
        eff_align = min(align, pack) if pack else align
        if eff_align < 1:
            eff_align = 1
        index = base_length + i
        if bits is None:
            run = None
            offset = _round_up(_round_up(pos_bits, 8) // 8, eff_align)
            fields.append(CField(name=name, type=tp, byte_size=size,
                                 byte_offset=offset, index=index,
                                 _internal_use=True))
            if offset > fmt_pos:
                format_parts.append(_padding(offset - fmt_pos))
            format_parts.append(f"{_field_format(tp)}:{name}:")
            pos_bits = (offset + size) * 8
            fmt_pos = offset + size
            max_align = max(max_align, eff_align)
            continue

        has_bitfield = True
        unit_bits = size * 8
        if is_ms:
            if run is not None and run[1] == size and run[2] + bits <= unit_bits:
                unit_offset, _, used = run
            else:
                unit_offset = _round_up(_round_up(pos_bits, 8) // 8, eff_align)
                used = 0
            pos_in_unit = used
            run = (unit_offset, size, used + bits)
            pos_bits = max(pos_bits, (unit_offset + size) * 8)
            byte_offset = unit_offset
            max_align = max(max_align, eff_align)
        else:
            unit_align_bits = (align if not pack else eff_align) * 8
            start = pos_bits
            if pack is None:
                # do not straddle an aligned unit of the declared type
                if start // unit_bits != (start + bits - 1) // unit_bits:
                    start = _round_up(start, unit_align_bits)
                byte_offset = (start // unit_bits) * size
                if (start - byte_offset * 8) + bits > unit_bits:
                    byte_offset = start // 8
            else:
                byte_offset = start // 8
            pos_in_unit = start - byte_offset * 8
            if pos_in_unit + bits > unit_bits:
                # (only possible for packed structures) start a new unit
                start = _round_up(start, 8)
                byte_offset = start // 8
                pos_in_unit = 0
            pos_bits = start + bits
            max_align = max(max_align, eff_align)
        if big_endian:
            bit_offset = unit_bits - pos_in_unit - bits
        else:
            bit_offset = pos_in_unit
        fields.append(CField(name=name, type=tp, byte_size=size,
                             byte_offset=byte_offset, index=index,
                             bit_size=bits, bit_offset=bit_offset,
                             _internal_use=True))

    align = max(max_align, align_override)
    size = _round_up(_round_up(pos_bits, 8) // 8, align)
    if has_bitfield:
        format_spec = None
    else:
        if size > fmt_pos:
            format_parts.append(_padding(size - fmt_pos))
        format_spec = "T{" + "".join(format_parts) + "}"
    return StructUnionLayout(fields, size, align, format_spec)


def _union_layout(cls, prepared, base_size, base_align, base_length, pack,
                  align_override, big_endian):
    fields = []
    size = base_size
    max_align = max(base_align, 1)
    for i, (name, tp, fsize, align, bits) in enumerate(prepared):
        eff_align = min(align, pack) if pack else align
        max_align = max(max_align, eff_align, 1)
        size = max(size, fsize)
        kw = {}
        if bits is not None:
            kw["bit_size"] = bits
            kw["bit_offset"] = (fsize * 8 - bits) if big_endian else 0
        fields.append(CField(name=name, type=tp, byte_size=fsize, byte_offset=0,
                             index=base_length + i, _internal_use=True, **kw))
    align = max(max_align, align_override)
    size = _round_up(size, align)
    return StructUnionLayout(fields, size, align, "B")
