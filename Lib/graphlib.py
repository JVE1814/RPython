"""Topological sorting of a graph of hashable nodes (graphlib)."""

from types import GenericAlias

__all__ = ["TopologicalSorter", "CycleError"]

# Per-node state: a non-negative number is the count of predecessors not
# yet marked done; the two negative values mark nodes handed out by
# get_ready() and nodes marked done().
_HANDED_OUT = -1
_FINISHED = -2
_END = object()


class CycleError(ValueError):
    """The graph contains a cycle.

    args[1] holds one such cycle as a list of nodes in which every node
    precedes the next one; the first and last entries are the same node.
    """


class TopologicalSorter:
    """Sorts a graph of hashable nodes topologically.

    Nodes are added with add(node, *predecessors) (or via the optional
    mapping passed to the constructor).  After prepare(), get_ready()
    hands out nodes whose predecessors are all done and done() marks nodes
    as processed; static_order() does all of this in one go.
    """

    def __init__(self, graph=None):
        self._pending = {}       # node -> state (see above)
        self._followers = {}     # node -> list of nodes that depend on it
        self._ready = None       # list of ready nodes once prepared
        self._handed_out = 0
        self._finished = 0
        if graph is not None:
            for node, predecessors in graph.items():
                self.add(node, *predecessors)

    def _register(self, node):
        if node not in self._pending:
            self._pending[node] = 0
            self._followers[node] = []

    def add(self, node, *predecessors):
        """Add `node` with the given predecessors (which are added as nodes
        too).  Repeated calls accumulate dependencies.  Not allowed after
        prepare()."""
        if self._ready is not None:
            raise ValueError("Nodes cannot be added after a call to prepare()")
        self._register(node)
        for pred in predecessors:
            self._register(pred)
        self._pending[node] += len(predecessors)
        for pred in predecessors:
            self._followers[pred].append(node)

    def prepare(self):
        """Freeze the graph and check it for cycles (CycleError).

        get_ready() remains usable after a CycleError for the acyclic
        part of the graph."""
        if self._handed_out:
            raise ValueError("cannot prepare() after starting sort")
        if self._ready is None:
            self._ready = [n for n, count in self._pending.items() if count == 0]
        cycle = self._find_cycle()
        if cycle:
            raise CycleError("nodes are in a cycle", cycle)

    def _require_prepared(self):
        if self._ready is None:
            raise ValueError("prepare() must be called first")

    def get_ready(self):
        """Return a tuple of the nodes that can be processed now (possibly
        empty)."""
        self._require_prepared()
        batch = tuple(self._ready)
        for node in batch:
            self._pending[node] = _HANDED_OUT
        self._ready.clear()
        self._handed_out += len(batch)
        return batch

    def is_active(self):
        """True while nodes remain to be handed out or marked done."""
        self._require_prepared()
        return self._finished < self._handed_out or bool(self._ready)

    def __bool__(self):
        return self.is_active()

    def done(self, *nodes):
        """Mark nodes returned by get_ready() as processed, which may make
        their dependents ready."""
        self._require_prepared()
        for node in nodes:
            if node not in self._pending:
                raise ValueError(f"node {node!r} was not added using add()")
            state = self._pending[node]
            if state == _FINISHED:
                raise ValueError(f"node {node!r} was already marked done")
            if state != _HANDED_OUT:
                raise ValueError(f"node {node!r} was not passed out (still not ready)")
            self._pending[node] = _FINISHED
            for follower in self._followers[node]:
                self._pending[follower] -= 1
                if self._pending[follower] == 0:
                    self._ready.append(follower)
            self._finished += 1

    def _find_cycle(self):
        # Iterative depth-first search along predecessor->follower edges.
        visited = set()
        for start in self._pending:
            if start in visited:
                continue
            path = [start]
            position = {start: 0}
            iters = [iter(self._followers[start])]
            visited.add(start)
            while iters:
                nxt = next(iters[-1], _END)
                if nxt is _END:
                    iters.pop()
                    del position[path.pop()]
                    continue
                if nxt in position:
                    return path[position[nxt]:] + [nxt]
                if nxt in visited:
                    continue
                visited.add(nxt)
                position[nxt] = len(path)
                path.append(nxt)
                iters.append(iter(self._followers[nxt]))
        return None

    def static_order(self):
        """Yield all nodes in a topological order (raises CycleError)."""
        self.prepare()
        while self.is_active():
            batch = self.get_ready()
            yield from batch
            self.done(*batch)

    __class_getitem__ = classmethod(GenericAlias)
