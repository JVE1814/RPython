'''Synchronized queue classes for multi-producer, multi-consumer use.'''

import threading
import types
from collections import deque
from heapq import heappush, heappop
from time import monotonic as time
try:
    from _queue import SimpleQueue
except ImportError:
    SimpleQueue = None

__all__ = [
    'Empty',
    'Full',
    'ShutDown',
    'Queue',
    'PriorityQueue',
    'LifoQueue',
    'SimpleQueue',
]


try:
    from _queue import Empty
except ImportError:
    class Empty(Exception):
        'Exception raised by Queue.get(block=0)/get_nowait().'
        pass

class Full(Exception):
    'Exception raised by Queue.put(block=0)/put_nowait().'
    pass


class ShutDown(Exception):
    '''Raised when put/get is called on a Queue that has been shut down.'''


class Queue:
    '''A FIFO queue; *maxsize* <= 0 means unbounded.'''

    def __init__(self, maxsize=0):
        self.maxsize = maxsize
        self._init(maxsize)

        # One mutex guards the queue state; the three conditions share it.
        self.mutex = threading.Lock()
        self.not_empty = threading.Condition(self.mutex)
        self.not_full = threading.Condition(self.mutex)
        self.all_tasks_done = threading.Condition(self.mutex)
        self.unfinished_tasks = 0
        self.is_shutdown = False

    def task_done(self):
        '''Mark a previously fetched item as processed; join() returns when
        every put() item has been marked.  ValueError if called more times
        than there were items.'''
        with self.all_tasks_done:
            unfinished = self.unfinished_tasks - 1
            if unfinished <= 0:
                if unfinished < 0:
                    raise ValueError('task_done() called too many times')
                self.all_tasks_done.notify_all()
            self.unfinished_tasks = unfinished

    def join(self):
        '''Block until every item put into the queue has been fetched and
        marked done with task_done().'''
        with self.all_tasks_done:
            while self.unfinished_tasks:
                self.all_tasks_done.wait()

    def qsize(self):
        '''Approximate size of the queue (not reliable).'''
        with self.mutex:
            return self._qsize()

    def empty(self):
        '''True if the queue is empty right now (not reliable).'''
        with self.mutex:
            return not self._qsize()

    def full(self):
        '''True if the queue is full right now (not reliable).'''
        with self.mutex:
            return 0 < self.maxsize <= self._qsize()

    def put(self, item, block=True, timeout=None):
        '''Put an item into the queue, waiting for a free slot if needed
        (forever, or at most *timeout* seconds; raises Full).  Raises
        ShutDown once the queue has been shut down.'''
        with self.not_full:
            if self.is_shutdown:
                raise ShutDown
            if self.maxsize > 0:
                if not block:
                    if self._qsize() >= self.maxsize:
                        raise Full
                elif timeout is None:
                    while self._qsize() >= self.maxsize:
                        self.not_full.wait()
                        if self.is_shutdown:
                            raise ShutDown
                elif timeout < 0:
                    raise ValueError("'timeout' must be a non-negative number")
                else:
                    endtime = time() + timeout
                    while self._qsize() >= self.maxsize:
                        remaining = endtime - time()
                        if remaining <= 0.0:
                            raise Full
                        self.not_full.wait(remaining)
                        if self.is_shutdown:
                            raise ShutDown
            self._put(item)
            self.unfinished_tasks += 1
            self.not_empty.notify()

    def get(self, block=True, timeout=None):
        '''Remove and return an item, waiting if needed (forever, or at
        most *timeout* seconds; raises Empty).  After an immediate shutdown,
        or once a shut-down queue is empty, raises ShutDown.'''
        with self.not_empty:
            if self.is_shutdown and not self._qsize():
                raise ShutDown
            if not block:
                if not self._qsize():
                    raise Empty
            elif timeout is None:
                while not self._qsize():
                    self.not_empty.wait()
                    if self.is_shutdown and not self._qsize():
                        raise ShutDown
            elif timeout < 0:
                raise ValueError("'timeout' must be a non-negative number")
            else:
                endtime = time() + timeout
                while not self._qsize():
                    remaining = endtime - time()
                    if remaining <= 0.0:
                        raise Empty
                    self.not_empty.wait(remaining)
                    if self.is_shutdown and not self._qsize():
                        raise ShutDown
            item = self._get()
            self.not_full.notify()
            return item

    def put_nowait(self, item):
        '''put(item, block=False).'''
        return self.put(item, block=False)

    def get_nowait(self):
        '''get(block=False).'''
        return self.get(block=False)

    def shutdown(self, immediate=False):
        '''Shut the queue down: put() raises ShutDown, and get() raises it
        once the queue is empty.  With *immediate*, the queue is drained
        first and pending join() calls are released.'''
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

    # Storage hooks; subclasses override these to change the ordering.

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
    '''Queue returning the lowest entry first (entries are usually tuples
    (priority, data)).'''

    def _init(self, maxsize):
        self.queue = []

    def _qsize(self):
        return len(self.queue)

    def _put(self, item):
        heappush(self.queue, item)

    def _get(self):
        return heappop(self.queue)


class LifoQueue(Queue):
    '''Queue returning the most recently added entry first.'''

    def _init(self, maxsize):
        self.queue = []

    def _qsize(self):
        return len(self.queue)

    def _put(self, item):
        self.queue.append(item)

    def _get(self):
        return self.queue.pop()


class _PySimpleQueue:
    '''Simple, unbounded FIFO queue (pure Python version of
    _queue.SimpleQueue).'''

    def __init__(self):
        self._queue = deque()
        self._count = threading.Semaphore(0)

    def put(self, item, block=True, timeout=None):
        '''Put the item on the queue (never blocks; the arguments exist for
        compatibility with Queue.put()).'''
        self._queue.append(item)
        self._count.release()

    def get(self, block=True, timeout=None):
        '''Remove and return an item, waiting if needed (forever, or at
        most *timeout* seconds; raises Empty).'''
        if block and timeout is not None and timeout < 0:
            raise ValueError("'timeout' must be a non-negative number")
        if not self._count.acquire(block, timeout if block else None):
            raise Empty
        return self._queue.popleft()

    def put_nowait(self, item):
        '''put(item, block=False).'''
        return self.put(item, block=False)

    def get_nowait(self):
        '''get(block=False).'''
        return self.get(False)

    def empty(self):
        '''True if the queue is empty right now (not reliable).'''
        return len(self._queue) == 0

    def qsize(self):
        '''Approximate size of the queue (not reliable).'''
        return len(self._queue)

    __class_getitem__ = classmethod(types.GenericAlias)


if SimpleQueue is None:
    SimpleQueue = _PySimpleQueue
