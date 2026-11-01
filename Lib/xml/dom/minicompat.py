"""Helpers for minidom: node lists and read-only properties.

NodeList, EmptyNodeList -- the DOM NodeList interface on top of list/tuple
StringTypes -- the types accepted as DOM strings
defproperty(klass, name, doc) -- a read-only property backed by _get_<name>
"""

__all__ = ["NodeList", "EmptyNodeList", "StringTypes", "defproperty"]

import xml.dom

StringTypes = (str,)


class NodeList(list):
    __slots__ = ()

    def item(self, index):
        if 0 <= index < len(self):
            return self[index]

    def _get_length(self):
        return len(self)

    def _set_length(self, value):
        raise xml.dom.NoModificationAllowedErr(
            "attempt to modify read-only attribute 'length'")

    length = property(_get_length, _set_length,
                      doc="The number of nodes in the NodeList.")

    def __setstate__(self, state):
        if state is None:
            state = []
        self[:] = state


class EmptyNodeList(tuple):
    __slots__ = ()

    def __add__(self, other):
        result = NodeList()
        result.extend(other)
        return result

    def __radd__(self, other):
        result = NodeList()
        result.extend(other)
        return result

    def item(self, index):
        return None

    def _get_length(self):
        return 0

    def _set_length(self, value):
        raise xml.dom.NoModificationAllowedErr(
            "attempt to modify read-only attribute 'length'")

    length = property(_get_length, _set_length,
                      doc="The number of nodes in the NodeList.")


def defproperty(klass, name, doc):
    getter = getattr(klass, "_get_" + name)

    def setter(self, value, name=name):
        raise xml.dom.NoModificationAllowedErr(
            "attempt to modify read-only attribute " + repr(name))

    assert not hasattr(klass, "_set_" + name), \
        "expected not to find _set_" + name
    setattr(klass, name, property(getter, setter, doc=doc))
