__all__ = (
    'Queue',
    'PriorityQueue',
    'LifoQueue',
    'QueueFull',
    'QueueEmpty',
    'QueueShutDown',
)

import collections
import heapq
from types import GenericAlias

from . import locks
from . import mixins


class QueueEmpty(Exception):
    """Raised when Queue.get_nowait() is called on an empty Queue."""
    pass


class QueueFull(Exception):
    """Raised when the Queue.put_nowait() method is called on a full Queue."""
    pass


class QueueShutDown(Exception):
    """Raised when putting on to or getting from a shut-down Queue."""
    pass


class Queue(mixins._LoopBoundMixin):
    """A FIFO queue for coroutines.

    maxsize <= 0 means unbounded; otherwise put() blocks while the queue
    holds maxsize items.  Not thread-safe.
    """

    def __init__(self, maxsize=0):
        self._maxsize = maxsize

        # Futures of blocked get() and put() calls.
        self._getters = collections.deque()
        self._putters = collections.deque()
        self._unfinished_tasks = 0
        self._finished = locks.Event()
        self._finished.set()
        self._init(maxsize)
        self._is_shutdown = False

    # Storage hooks overridden by the subclasses.

    def _init(self, maxsize):
        self._queue = collections.deque()

    def _get(self):
        return self._queue.popleft()

    def _put(self, item):
        self._queue.append(item)

    def _wakeup_next(self, waiters):
        # Wake up the next waiter (if any) that isn't cancelled.
        while waiters:
            waiter = waiters.popleft()
            if not waiter.done():
                waiter.set_result(None)
                break

    def __repr__(self):
        return f'<{type(self).__name__} at {id(self):#x} {self._format()}>'

    def __str__(self):
        return f'<{type(self).__name__} {self._format()}>'

    __class_getitem__ = classmethod(GenericAlias)

    def _format(self):
        result = f'maxsize={self._maxsize!r}'
        if getattr(self, '_queue', None):
            result += f' _queue={list(self._queue)!r}'
        if self._getters:
            result += f' _getters[{len(self._getters)}]'
        if self._putters:
            result += f' _putters[{len(self._putters)}]'
        if self._unfinished_tasks:
            result += f' tasks={self._unfinished_tasks}'
        if self._is_shutdown:
            result += ' shutdown'
        return result

    def qsize(self):
        """Number of items in the queue."""
        return len(self._queue)

    @property
    def maxsize(self):
        """Number of items allowed in the queue."""
        return self._maxsize

    def empty(self):
        """Return True if the queue is empty, False otherwise."""
        return not self._queue

    def full(self):
        """Return True if there are maxsize items in the queue."""
        if self._maxsize <= 0:
            return False
        else:
            return self.qsize() >= self._maxsize

    async def put(self, item):
        """Put an item into the queue, waiting for a free slot if needed.

        Raises QueueShutDown if the queue has been shut down.
        """
        while self.full():
            if self._is_shutdown:
                raise QueueShutDown
            putter = self._get_loop().create_future()
            self._putters.append(putter)
            try:
                await putter
            except:
                putter.cancel()  # may still be pending
                try:
                    self._putters.remove(putter)
                except ValueError:
                    # already removed by get_nowait() or shutdown()
                    pass
                if not self.full() and not putter.cancelled():
                    # we were woken up but cannot use the slot: pass it on
                    self._wakeup_next(self._putters)
                raise
        return self.put_nowait(item)

    def put_nowait(self, item):
        """Put an item without blocking; QueueFull if there is no slot."""
        if self._is_shutdown:
            raise QueueShutDown
        if self.full():
            raise QueueFull
        self._put(item)
        self._unfinished_tasks += 1
        self._finished.clear()
        self._wakeup_next(self._getters)

    async def get(self):
        """Remove and return an item, waiting until one is available.

        Raises QueueShutDown if the queue was shut down and is empty (or
        shut down immediately).
        """
        while self.empty():
            if self._is_shutdown and self.empty():
                raise QueueShutDown
            getter = self._get_loop().create_future()
            self._getters.append(getter)
            try:
                await getter
            except:
                getter.cancel()  # may still be pending
                try:
                    self._getters.remove(getter)
                except ValueError:
                    # already removed by put_nowait() or shutdown()
                    pass
                if not self.empty() and not getter.cancelled():
                    # we were woken up but cannot take the item: pass it on
                    self._wakeup_next(self._getters)
                raise
        return self.get_nowait()

    def get_nowait(self):
        """Remove and return an item if one is available, else QueueEmpty."""
        if self.empty():
            if self._is_shutdown:
                raise QueueShutDown
            raise QueueEmpty
        item = self._get()
        self._wakeup_next(self._putters)
        return item

    def task_done(self):
        """Indicate that a formerly enqueued task is complete (see join())."""
        if self._unfinished_tasks <= 0:
            raise ValueError('task_done() called too many times')
        self._unfinished_tasks -= 1
        if self._unfinished_tasks == 0:
            self._finished.set()

    async def join(self):
        """Block until every item put in the queue has been processed."""
        if self._unfinished_tasks > 0:
            await self._finished.wait()

    def shutdown(self, immediate=False):
        """Shut down the queue: put() raises QueueShutDown from now on and
        blocked callers are woken up.  With immediate=True the queue is
        also emptied (get() raises QueueShutDown right away)."""
        self._is_shutdown = True
        if immediate:
            while not self.empty():
                self._get()
                if self._unfinished_tasks > 0:
                    self._unfinished_tasks -= 1
            # release all blocked join() calls
            if self._unfinished_tasks == 0:
                self._finished.set()
        # wake everybody up so they re-check the state
        while self._getters:
            getter = self._getters.popleft()
            if not getter.done():
                getter.set_result(None)
        while self._putters:
            putter = self._putters.popleft()
            if not putter.done():
                putter.set_result(None)


class PriorityQueue(Queue):
    """A queue retrieving entries in priority order (lowest first)."""

    def _init(self, maxsize):
        self._queue = []

    def _put(self, item, heappush=heapq.heappush):
        heappush(self._queue, item)

    def _get(self, heappop=heapq.heappop):
        return heappop(self._queue)


class LifoQueue(Queue):
    """A queue retrieving the most recently added entries first."""

    def _init(self, maxsize):
        self._queue = []

    def _put(self, item):
        self._queue.append(item)

    def _get(self):
        return self._queue.pop()
