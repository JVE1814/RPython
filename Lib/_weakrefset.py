"""WeakSet: a set holding weak references to its elements.

This module is imported very early (by abc/_py_abc), so it only depends on
builtin modules.
"""

from _weakref import ref

GenericAlias = type(list[int])

__all__ = ['WeakSet']


class _IterationGuard:
    """Context manager marking a weak container as being iterated.

    While at least one guard is active, removals triggered by dying
    referents are queued instead of mutating the underlying container.
    """

    def __init__(self, weakcontainer):
        self.weakcontainer = ref(weakcontainer)

    def __enter__(self):
        container = self.weakcontainer()
        if container is not None:
            container._iterating.add(self)
        return self

    def __exit__(self, exc_type, exc_value, traceback):
        container = self.weakcontainer()
        if container is not None:
            active = container._iterating
            active.discard(self)
            if not active:
                container._commit_removals()


class WeakSet:
    def __init__(self, data=None):
        self.data = set()

        def _remove(item, selfref=ref(self)):
            owner = selfref()
            if owner is not None:
                if owner._iterating:
                    owner._pending_removals.append(item)
                else:
                    owner.data.discard(item)

        self._remove = _remove
        # Weak references queued for removal while iterating.
        self._pending_removals = []
        self._iterating = set()
        if data is not None:
            self.update(data)

    def _commit_removals(self):
        # Other threads may drain the same list concurrently: pop until it is
        # empty instead of testing first and popping afterwards.
        take = self._pending_removals.pop
        discard = self.data.discard
        while True:
            try:
                ref_ = take()
            except IndexError:
                return
            discard(ref_)

    def __iter__(self):
        with _IterationGuard(self):
            for itemref in self.data:
                item = itemref()
                if item is not None:
                    # Hold a strong reference while the caller consumes it.
                    yield item

    def __len__(self):
        return len(self.data) - len(self._pending_removals)

    def __contains__(self, item):
        try:
            wr = ref(item)
        except TypeError:
            return False
        return wr in self.data

    def __reduce__(self):
        return self.__class__, (list(self),), self.__getstate__()

    def add(self, item):
        if self._pending_removals:
            self._commit_removals()
        self.data.add(ref(item, self._remove))

    def clear(self):
        if self._pending_removals:
            self._commit_removals()
        self.data.clear()

    def copy(self):
        return self.__class__(self)

    def pop(self):
        if self._pending_removals:
            self._commit_removals()
        while True:
            try:
                itemref = self.data.pop()
            except KeyError:
                raise KeyError('pop from empty WeakSet') from None
            item = itemref()
            if item is not None:
                return item

    def remove(self, item):
        if self._pending_removals:
            self._commit_removals()
        self.data.remove(ref(item))

    def discard(self, item):
        if self._pending_removals:
            self._commit_removals()
        self.data.discard(ref(item))

    def update(self, other):
        if self._pending_removals:
            self._commit_removals()
        for element in other:
            self.add(element)

    def __ior__(self, other):
        self.update(other)
        return self

    def difference(self, other):
        result = self.copy()
        result.difference_update(other)
        return result
    __sub__ = difference

    def difference_update(self, other):
        self.__isub__(other)

    def __isub__(self, other):
        if self._pending_removals:
            self._commit_removals()
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
        if self._pending_removals:
            self._commit_removals()
        self.data.intersection_update(ref(item) for item in other)
        return self

    def issubset(self, other):
        return self.data.issubset(ref(item) for item in other)
    __le__ = issubset

    def __lt__(self, other):
        return self.data < set(map(ref, other))

    def issuperset(self, other):
        return self.data.issuperset(ref(item) for item in other)
    __ge__ = issuperset

    def __gt__(self, other):
        return self.data > set(map(ref, other))

    def __eq__(self, other):
        if not isinstance(other, self.__class__):
            return NotImplemented
        return self.data == set(map(ref, other))

    def symmetric_difference(self, other):
        result = self.copy()
        result.symmetric_difference_update(other)
        return result
    __xor__ = symmetric_difference

    def symmetric_difference_update(self, other):
        self.__ixor__(other)

    def __ixor__(self, other):
        if self._pending_removals:
            self._commit_removals()
        if self is other:
            self.data.clear()
        else:
            self.data.symmetric_difference_update(ref(item, self._remove) for item in other)
        return self

    def union(self, other):
        return self.__class__(e for s in (self, other) for e in s)
    __or__ = union

    def isdisjoint(self, other):
        return len(self.intersection(other)) == 0

    def __repr__(self):
        return repr(self.data)

    __hash__ = None

    __class_getitem__ = classmethod(GenericAlias)
