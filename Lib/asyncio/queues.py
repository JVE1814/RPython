"""Queues for passing data between coroutines (FIFO, LIFO, priority)."""

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
    """get_nowait() found the queue empty."""


class QueueFull(Exception):
    """put_nowait() found the queue full."""


class QueueShutDown(Exception):
    """The queue was shut down."""


def _wake_one(waiters):
    while waiters:
        fut = waiters.popleft()
        if not fut.done():
            fut.set_result(None)
            return


def _wake_all(waiters):
    while waiters:
        fut = waiters.popleft()
        if not fut.done():
            fut.set_result(None)


class Queue(mixins._LoopBoundMixin):
    """FIFO queue; maxsize <= 0 means unbounded."""

    def __init__(self, maxsize=0):
        self._maxsize = maxsize
        self._getters = collections.deque()
        self._putters = collections.deque()
        self._unfinished_tasks = 0
        self._finished = locks.Event()
        self._finished.set()
        self._is_shutdown = False
        self._init(maxsize)

    # storage strategy (overridden by the subclasses)
    def _init(self, maxsize):
        self._queue = collections.deque()

    def _put(self, item):
        self._queue.append(item)

    def _get(self):
        return self._queue.popleft()

    __class_getitem__ = classmethod(GenericAlias)

    def _format(self):
        text = f'maxsize={self._maxsize!r}'
        if getattr(self, '_queue', None):
            text += f' _queue={list(self._queue)!r}'
        for label, waiters in (('_getters', self._getters),
                               ('_putters', self._putters)):
            if waiters:
                text += f' {label}[{len(waiters)}]'
        if self._unfinished_tasks:
            text += f' tasks={self._unfinished_tasks}'
        if self._is_shutdown:
            text += ' shutdown'
        return text

    def __repr__(self):
        return f'<{type(self).__name__} at {id(self):#x} {self._format()}>'

    def __str__(self):
        return f'<{type(self).__name__} {self._format()}>'

    @property
    def maxsize(self):
        return self._maxsize

    def qsize(self):
        return len(self._queue)

    def empty(self):
        return not self._queue

    def full(self):
        return 0 < self._maxsize <= self.qsize()

    def _unpark(self, fut, waiters, still_blocked):
        # Called when a parked put()/get() fails (e.g. is cancelled): if it
        # had been woken, the wake-up is passed on to the next waiter.
        fut.cancel()
        try:
            waiters.remove(fut)
        except ValueError:
            pass   # already popped by a waker
        if not still_blocked() and not fut.cancelled():
            _wake_one(waiters)

    async def put(self, item):
        """Put *item*, waiting while the queue is full."""
        while self.full():
            if self._is_shutdown:
                raise QueueShutDown
            fut = self._get_loop().create_future()
            self._putters.append(fut)
            try:
                await fut
            except:
                self._unpark(fut, self._putters, self.full)
                raise
        return self.put_nowait(item)

    def put_nowait(self, item):
        if self._is_shutdown:
            raise QueueShutDown
        if self.full():
            raise QueueFull
        self._put(item)
        self._unfinished_tasks += 1
        self._finished.clear()
        _wake_one(self._getters)

    async def get(self):
        """Remove and return an item, waiting while the queue is empty."""
        while self.empty():
            if self._is_shutdown:
                raise QueueShutDown
            fut = self._get_loop().create_future()
            self._getters.append(fut)
            try:
                await fut
            except:
                self._unpark(fut, self._getters, self.empty)
                raise
        return self.get_nowait()

    def get_nowait(self):
        if self.empty():
            raise QueueShutDown if self._is_shutdown else QueueEmpty
        item = self._get()
        _wake_one(self._putters)
        return item

    def task_done(self):
        """Mark one retrieved item as processed (see join())."""
        if self._unfinished_tasks <= 0:
            raise ValueError('task_done() called too many times')
        self._unfinished_tasks -= 1
        if not self._unfinished_tasks:
            self._finished.set()

    async def join(self):
        """Wait until every item put has been marked done."""
        if self._unfinished_tasks:
            await self._finished.wait()

    def shutdown(self, immediate=False):
        """Refuse further put()s.  immediate=True also discards the
        remaining items so that get() fails right away."""
        self._is_shutdown = True
        if immediate:
            while not self.empty():
                self._get()
                if self._unfinished_tasks:
                    self._unfinished_tasks -= 1
            if not self._unfinished_tasks:
                self._finished.set()
        _wake_all(self._getters)
        _wake_all(self._putters)


class PriorityQueue(Queue):
    """Queue returning the smallest item first."""

    def _init(self, maxsize):
        self._queue = []

    def _put(self, item):
        heapq.heappush(self._queue, item)

    def _get(self):
        return heapq.heappop(self._queue)


class LifoQueue(Queue):
    """Queue returning the most recently added item first."""

    def _init(self, maxsize):
        self._queue = []

    def _put(self, item):
        self._queue.append(item)

    def _get(self):
        return self._queue.pop()
