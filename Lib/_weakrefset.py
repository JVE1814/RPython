"""WeakSet: a set that refers to its elements through weak references.

An element disappears from the set when the last strong reference to it
goes away.  This module is imported very early (abc uses it), so it only
imports builtin modules.
"""

from _weakref import ref

GenericAlias = type(list[int])

__all__ = ['WeakSet']


class _Iterating:
    """Context manager telling a weak container that an iteration over it
    is in progress; the container (which has an ``_iterating`` set and a
    ``_commit_removals()`` method) postpones removing dead entries until
    the last such iteration ends."""

    __slots__ = ('_container', '__weakref__')

    def __init__(self, container):
        self._container = ref(container)

    def __enter__(self):
        container = self._container()
        if container is not None:
            container._iterating.add(self)
        return self

    def __exit__(self, *exc_info):
        container = self._container()
        if container is not None:
            running = container._iterating
            running.discard(self)
            if not running:
                container._commit_removals()


# weakref imports the guard under this name
_IterationGuard = _Iterating


def _dead_ref_callback(selfref):
    # The callback of the element references.  It holds the set only
    # weakly, so a WeakSet can be collected while elements are alive.
    def element_died(wr, selfref=selfref):
        owner = selfref()
        if owner is None:
            return
        if owner._iterating:
            owner._pending_removals.append(wr)
        else:
            owner.data.discard(wr)
    return element_died


class WeakSet:
    """A set of weakly referenced (hashable, weak-referenceable) objects."""

    def __init__(self, data=None):
        self.data = set()                   # weak references to elements
        self._pending_removals = []         # died during an iteration
        self._iterating = set()             # active _Iterating guards
        self._remove = _dead_ref_callback(ref(self))
        if data is not None:
            self.update(data)

    def _commit_removals(self):
        # pop one at a time: another thread may be emptying the list too
        pending = self._pending_removals
        discard = self.data.discard
        while pending:
            try:
                discard(pending.pop())
            except IndexError:
                break

    def _settle(self):
        if self._pending_removals:
            self._commit_removals()

    def _ref(self, item):
        return ref(item, self._remove)

    # -- container protocol ---------------------------------------------------

    def __iter__(self):
        with _Iterating(self):
            for wr in self.data:
                item = wr()
                if item is not None:
                    yield item              # a strong reference while in use

    def __len__(self):
        return len(self.data) - len(self._pending_removals)

    def __contains__(self, item):
        try:
            wr = ref(item)
        except TypeError:
            return False                    # cannot be an element
        return wr in self.data

    def __repr__(self):
        return repr(self.data)

    __hash__ = None
    __class_getitem__ = classmethod(GenericAlias)

    def __reduce__(self):
        # The whole instance state goes along, including ``data``: a deep
        # copy therefore refers to the same (not copied) elements, which
        # would otherwise die as soon as the copy is made.
        return self.__class__, (list(self),), self.__getstate__()

    # -- mutation ------------------------------------------------------------------

    def add(self, item):
        self._settle()
        self.data.add(self._ref(item))

    def clear(self):
        self._settle()
        self.data.clear()

    def copy(self):
        return self.__class__(self)

    def pop(self):
        self._settle()
        while True:
            try:
                wr = self.data.pop()
            except KeyError:
                raise KeyError('pop from empty WeakSet') from None
            item = wr()
            if item is not None:
                return item

    def remove(self, item):
        self._settle()
        self.data.remove(ref(item))

    def discard(self, item):
        self._settle()
        self.data.discard(ref(item))

    def update(self, other):
        self._settle()
        for element in other:
            self.add(element)

    def __ior__(self, other):
        self.update(other)
        return self

    # -- set algebra (other may be any iterable of weak-referenceable
    # objects) -------------------------------------------------------------------

    def difference(self, other):
        result = self.copy()
        result.difference_update(other)
        return result

    __sub__ = difference

    def difference_update(self, other):
        self.__isub__(other)

    def __isub__(self, other):
        self._settle()
        if self is other:
            self.data.clear()
        else:
            self.data.difference_update(ref(item) for item in other)
        return self

    def intersection(self, other):
        return self.__class__(item for item in other if item in self)

    __and__ = intersection

    def intersection_update(self, other):
        self.__iand__(other)

    def __iand__(self, other):
        self._settle()
        self.data.intersection_update(ref(item) for item in other)
        return self

    def symmetric_difference(self, other):
        result = self.copy()
        result.symmetric_difference_update(other)
        return result

    __xor__ = symmetric_difference

    def symmetric_difference_update(self, other):
        self.__ixor__(other)

    def __ixor__(self, other):
        self._settle()
        if self is other:
            self.data.clear()
        else:
            # new members need the callback that removes them again
            self.data.symmetric_difference_update(self._ref(item)
                                                  for item in other)
        return self

    def union(self, other):
        return self.__class__(item for group in (self, other)
                              for item in group)

    __or__ = union

    def isdisjoint(self, other):
        return len(self.intersection(other)) == 0

    def issubset(self, other):
        return self.data.issubset(ref(item) for item in other)

    __le__ = issubset

    def issuperset(self, other):
        return self.data.issuperset(ref(item) for item in other)

    __ge__ = issuperset

    def __lt__(self, other):
        return self.data < set(map(ref, other))

    def __gt__(self, other):
        return self.data > set(map(ref, other))

    def __eq__(self, other):
        if not isinstance(other, self.__class__):
            return NotImplemented
        return self.data == set(map(ref, other))
