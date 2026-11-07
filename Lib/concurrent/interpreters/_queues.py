"""Queues for passing values between interpreters.

A queue lives in the process, not in an interpreter: every interpreter can
get a Queue object for it (Queue(id)), and Queue objects can themselves be
passed through queues or to Interpreter.prepare_main().  Values put into a
queue are copied (natively shareable values) or pickled.
"""

import queue
import time
import weakref

import _interpqueues as _queues
from . import _crossinterp
from _interpqueues import QueueError, QueueNotFoundError
from ._crossinterp import UNBOUND_ERROR, UNBOUND_REMOVE


__all__ = [
    'UNBOUND', 'UNBOUND_ERROR', 'UNBOUND_REMOVE',
    'create', 'list_all',
    'Queue',
    'QueueError', 'QueueNotFoundError', 'QueueEmpty', 'QueueFull',
    'ItemInterpreterDestroyed',
]


class QueueEmpty(QueueError, queue.Empty):
    """get() found the queue empty (within the timeout)."""


class QueueFull(QueueError, queue.Full):
    """put() found the queue full (within the timeout)."""


class ItemInterpreterDestroyed(QueueError,
                               _crossinterp.ItemInterpreterDestroyed):
    """The item's interpreter was destroyed (and the item put with
    unbounditems=UNBOUND_ERROR)."""


UNBOUND = _crossinterp.UnboundItem.singleton('queue', __name__)


def _flag_of(unbounditems):
    if unbounditems is UNBOUND:
        unbounditems = _crossinterp.UNBOUND
    flag, = _crossinterp.serialize_unbound(unbounditems)
    return flag


def _value_of(flag):
    value = _crossinterp.resolve_unbound(flag, ItemInterpreterDestroyed)
    if value is _crossinterp.UNBOUND:
        value = UNBOUND
    return value


def create(maxsize=0, *, unbounditems=UNBOUND):
    """Return a new cross-interpreter queue.

    "unbounditems" says what get() does with an item whose interpreter was
    destroyed: UNBOUND (the default) hands out UNBOUND instead, UNBOUND_ERROR
    raises ItemInterpreterDestroyed and UNBOUND_REMOVE drops the item.
    """
    flag = _flag_of(unbounditems)
    qid = _queues.create(maxsize, flag, -1)
    self = Queue(qid)
    self._unbound_default = unbounditems
    return self


def list_all():
    """Return all open queues."""
    return [Queue(qid) for qid, _, _ in _queues.list_all()]


# one Queue object per queue in each interpreter
_known = weakref.WeakValueDictionary()

# how long a blocking put()/get() waits between attempts
_POLL_DELAY = 0.005


class Queue:
    """A cross-interpreter queue (like queue.Queue)."""

    def __new__(cls, id, /):
        if isinstance(id, int):
            qid = id
        else:
            raise TypeError(f'id must be an int, got {id!r}')
        try:
            self = _known[qid]
        except KeyError:
            self = super().__new__(cls)
            self._id = qid
            _queues.bind(qid)
            self._bound = True
            _known[qid] = self
        return self

    # Everything __del__ uses is bound when the class is made: the last
    # reference may go while an interpreter is torn down, after this module's
    # globals were cleared.
    def __del__(self, _getattr=getattr, _release=_queues.release,
                _error=QueueError):
        if _getattr(self, '_bound', False):
            self._bound = False
            try:
                _release(self._id)
            except _error:
                pass

    def __repr__(self):
        return f'{type(self).__name__}({self._id})'

    def __hash__(self):
        return hash(self._id)

    def __eq__(self, other):
        if not isinstance(other, Queue):
            return NotImplemented
        return self._id == other._id

    def __reduce__(self):
        return (type(self), (self._id,))

    @property
    def id(self):
        return self._id

    @property
    def maxsize(self):
        try:
            return self._maxsize
        except AttributeError:
            self._maxsize = _queues.get_maxsize(self._id)
            return self._maxsize

    def empty(self):
        return self.qsize() == 0

    def full(self):
        return _queues.is_full(self._id)

    def qsize(self):
        return _queues.get_count(self._id)

    @staticmethod
    def _deadline(timeout):
        if timeout is None:
            return None
        if timeout < 0:
            raise ValueError('timeout value must be non-negative')
        return time.monotonic() + timeout

    def put(self, obj, block=True, timeout=None, *, unbounditems=None):
        """Add the object to the queue, waiting (up to "timeout" seconds) for
        room if the queue has a maxsize and is full.

        Shareable objects are copied; anything else is pickled.
        "unbounditems" overrides the queue's default (see create()).
        """
        flag = -1 if unbounditems is None else _flag_of(unbounditems)
        if not block:
            return _queues.put(self._id, obj, flag, -1)
        deadline = self._deadline(timeout)
        while True:
            try:
                return _queues.put(self._id, obj, flag, -1)
            except QueueFull:
                if deadline is not None and time.monotonic() >= deadline:
                    raise
            time.sleep(_POLL_DELAY)

    def put_nowait(self, obj, *, unbounditems=None):
        return self.put(obj, block=False, unbounditems=unbounditems)

    def get(self, block=True, timeout=None):
        """Remove and return the next item, waiting (up to "timeout" seconds)
        while the queue is empty."""
        if not block:
            return self.get_nowait()
        deadline = self._deadline(timeout)
        while True:
            try:
                obj, flag = _queues.get(self._id)
                break
            except QueueEmpty:
                if deadline is not None and time.monotonic() >= deadline:
                    raise
            time.sleep(_POLL_DELAY)
        if flag is not None:
            return _value_of(flag)
        return obj

    def get_nowait(self):
        """Remove and return the next item; QueueEmpty if there is none."""
        obj, flag = _queues.get(self._id)
        if flag is not None:
            return _value_of(flag)
        return obj


_queues._register_heap_types(Queue, QueueEmpty, QueueFull)
