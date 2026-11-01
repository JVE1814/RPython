"""Structures and unions with an explicit byte order."""

import sys
from ctypes import Array, Structure, Union

_ARRAY_META = type(Array)

if sys.byteorder == "little":
    _FOREIGN_ATTR = "__ctype_be__"
else:
    _FOREIGN_ATTR = "__ctype_le__"


def _foreign_order(tp):
    """Return the variant of ``tp`` stored in the non-native byte order."""
    variant = getattr(tp, _FOREIGN_ATTR, None)
    if variant is not None:
        return variant
    if isinstance(tp, _ARRAY_META):
        return _foreign_order(tp._type_) * tp._length_
    if issubclass(tp, (Structure, Union)):
        # nested aggregates carry their own byte order
        return tp
    raise TypeError("This type does not support other endian: %s" % tp)


class _ForeignOrderMeta:
    def __setattr__(cls, name, value):
        if name == "_fields_":
            converted = []
            for entry in value:
                fname, ftype, *extra = entry
                converted.append((fname, _foreign_order(ftype), *extra))
            value = converted
        super().__setattr__(name, value)


class _ForeignStructMeta(_ForeignOrderMeta, type(Structure)):
    pass


class _ForeignUnionMeta(_ForeignOrderMeta, type(Union)):
    pass


if sys.byteorder == "little":
    LittleEndianStructure = Structure
    LittleEndianUnion = Union

    class BigEndianStructure(Structure, metaclass=_ForeignStructMeta):
        """Structure with big endian byte order"""
        __slots__ = ()
        _swappedbytes_ = None

    class BigEndianUnion(Union, metaclass=_ForeignUnionMeta):
        """Union with big endian byte order"""
        __slots__ = ()
        _swappedbytes_ = None

elif sys.byteorder == "big":
    BigEndianStructure = Structure
    BigEndianUnion = Union

    class LittleEndianStructure(Structure, metaclass=_ForeignStructMeta):
        """Structure with little endian byte order"""
        __slots__ = ()
        _swappedbytes_ = None

    class LittleEndianUnion(Union, metaclass=_ForeignUnionMeta):
        """Union with little endian byte order"""
        __slots__ = ()
        _swappedbytes_ = None

else:
    raise RuntimeError("Invalid byteorder")
