"""Thread-safe queues: Queue (FIFO), LifoQueue, PriorityQueue and the
lightweight SimpleQueue."""

import threading
import types
from collections import deque
from heapq import heappush, heappop
from time import monotonic as _clock

try:
    from _queue import SimpleQueue
except ImportError:
    SimpleQueue = None

__all__ = ['Empty', 'Full', 'ShutDown', 'Queue', 'PriorityQueue', 'LifoQueue',
           'SimpleQueue']


try:
    from _queue import Empty
except ImportError:
    class Empty(Exception):
        """Raised by a non-blocking or timed get() when no item arrives."""


class Full(Exception):
    """Raised by a non-blocking or timed put() when no slot frees up."""


class ShutDown(Exception):
    """Raised by put() or get() on a queue that has been shut down."""


def _check_timeout(timeout):
    if timeout is not None and timeout < 0:
        raise ValueError("'timeout' must be a non-negative number")


class Queue:
    """FIFO queue for passing items between threads.

    maxsize <= 0 means unbounded.  Subclasses change the ordering by
    overriding the storage hooks _init(), _qsize(), _put() and _get(), which
    are always called with the mutex held.
    """

    def __init__(self, maxsize=0):
        self.maxsize = maxsize
        self._init(maxsize)
        # One lock guards the storage; the three conditions share it.
        self.mutex = threading.Lock()
        self.not_empty = threading.Condition(self.mutex)
        self.not_full = threading.Condition(self.mutex)
        self.all_tasks_done = threading.Condition(self.mutex)
        self.unfinished_tasks = 0
        self.is_shutdown = False

    # -- task accounting ----------------------------------------------------

    def task_done(self):
        """Report that one item fetched with get() has been processed.

        join() returns once every item put into the queue was reported.
        Calling this more often than items were put raises ValueError.
        """
        with self.all_tasks_done:
            remaining = self.unfinished_tasks - 1
            if remaining < 0:
                raise ValueError('task_done() called too many times')
            self.unfinished_tasks = remaining
            if remaining == 0:
                self.all_tasks_done.notify_all()

    def join(self):
        """Wait until every item put into the queue has been processed
        (see task_done())."""
        with self.all_tasks_done:
            while self.unfinished_tasks:
                self.all_tasks_done.wait()

    # -- size queries (approximate: other threads may act meanwhile) ----------

    def qsize(self):
        """Approximate number of items in the queue."""
        with self.mutex:
            return self._qsize()

    def empty(self):
        """True if the queue looks empty (no guarantee for the next get)."""
        with self.mutex:
            return not self._qsize()

    def full(self):
        """True if the queue looks full (no guarantee for the next put)."""
        with self.mutex:
            return 0 < self.maxsize <= self._qsize()

    # -- put / get ----------------------------------------------------------

    def _wait_until(self, condition, ready, block, timeout, failure):
        # Slow path of put()/get(), entered with condition's lock held and
        # ready() false: wait until ready() holds.  Raises failure when not
        # blocking or when the timeout runs out, ShutDown when the queue is
        # shut down meanwhile.
        if not block:
            raise failure
        deadline = None if timeout is None else _clock() + timeout
        while not ready():
            if self.is_shutdown:
                raise ShutDown
            if deadline is None:
                condition.wait()
            else:
                left = deadline - _clock()
                if left <= 0.0:
                    raise failure
                condition.wait(left)

    def _has_room(self):
        return self._qsize() < self.maxsize

    def put(self, item, block=True, timeout=None):
        """Add *item* to the queue.

        When the queue is full: with block false raise Full at once;
        otherwise wait for a free slot, at most *timeout* seconds if given
        (then raise Full).  Raises ShutDown if the queue is shut down.
        """
        if block:
            _check_timeout(timeout)
        with self.not_full:
            if self.is_shutdown:
                raise ShutDown
            if 0 < self.maxsize <= self._qsize():
                self._wait_until(self.not_full, self._has_room, block,
                                 timeout, Full)
            self._put(item)
            self.unfinished_tasks += 1
            self.not_empty.notify()

    def get(self, block=True, timeout=None):
        """Remove and return the next item.

        When the queue is empty: with block false raise Empty at once;
        otherwise wait for an item, at most *timeout* seconds if given
        (then raise Empty).  After shutdown() the remaining items can still
        be fetched; then ShutDown is raised.
        """
        if block:
            _check_timeout(timeout)
        with self.not_empty:
            if not self._qsize():
                if self.is_shutdown:
                    raise ShutDown
                self._wait_until(self.not_empty, self._qsize, block,
                                 timeout, Empty)
            item = self._get()
            self.not_full.notify()
            return item

    def put_nowait(self, item):
        """put(item, block=False)"""
        return self.put(item, block=False)

    def get_nowait(self):
        """get(block=False)"""
        return self.get(block=False)

    def shutdown(self, immediate=False):
        """Stop the queue: later put() calls raise ShutDown, and so do get()
        calls once the queue is empty; blocked callers are woken up.

        With *immediate*, the remaining items are discarded (counting as
        done for join()).
        """
        with self.mutex:
            self.is_shutdown = True
            if immediate:
                while self._qsize():
                    self._get()
                    if self.unfinished_tasks > 0:
                        self.unfinished_tasks -= 1
                self.all_tasks_done.notify_all()
            self.not_empty.notify_all()
            self.not_full.notify_all()

    # -- storage hooks ------------------------------------------------------

    def _init(self, maxsize):
        self.queue = deque()

    def _qsize(self):
        return len(self.queue)

    def _put(self, item):
        self.queue.append(item)

    def _get(self):
        return self.queue.popleft()

    __class_getitem__ = classmethod(types.GenericAlias)


class PriorityQueue(Queue):
    """Queue handing out the smallest item first (entries are often
    (priority, data) tuples)."""

    def _init(self, maxsize):
        self.queue = []

    def _qsize(self):
        return len(self.queue)

    def _put(self, item):
        heappush(self.queue, item)

    def _get(self):
        return heappop(self.queue)


class LifoQueue(Queue):
    """Queue handing out the most recently added item first (a stack)."""

    def _init(self, maxsize):
        self.queue = []

    def _qsize(self):
        return len(self.queue)

    def _put(self, item):
        self.queue.append(item)

    def _get(self):
        return self.queue.pop()


class _PySimpleQueue:
    """Unbounded FIFO queue without task tracking (Python fallback for
    _queue.SimpleQueue).  put() never blocks."""

    def __init__(self):
        self._items = deque()
        self._available = threading.Semaphore(0)

    def put(self, item, block=True, timeout=None):
        """Append *item*; *block* and *timeout* exist for compatibility with
        Queue.put and are ignored."""
        self._items.append(item)
        self._available.release()

    def get(self, block=True, timeout=None):
        """Remove and return the oldest item, waiting like Queue.get."""
        if block:
            _check_timeout(timeout)
            acquired = self._available.acquire(timeout=timeout)
        else:
            acquired = self._available.acquire(False)
        if not acquired:
            raise Empty
        return self._items.popleft()

    def put_nowait(self, item):
        """put(item, block=False)"""
        return self.put(item, block=False)

    def get_nowait(self):
        """get(block=False)"""
        return self.get(block=False)

    def empty(self):
        """True if the queue looks empty."""
        return not self._items

    def qsize(self):
        """Approximate number of items in the queue."""
        return len(self._items)

    __class_getitem__ = classmethod(types.GenericAlias)


if SimpleQueue is None:
    SimpleQueue = _PySimpleQueue
