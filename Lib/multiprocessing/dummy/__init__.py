#
# multiprocessing.dummy -- the multiprocessing API implemented with threads.
#
# "Processes" are threads, the synchronisation primitives and the queue
# come straight from threading and queue, and the manager is this module.
#

__all__ = [
    'Process', 'current_process', 'active_children', 'freeze_support',
    'Lock', 'RLock', 'Semaphore', 'BoundedSemaphore', 'Condition',
    'Event', 'Barrier', 'Queue', 'Manager', 'Pipe', 'Pool', 'JoinableQueue'
    ]

import sys
import threading
import weakref

from .connection import Pipe
from threading import Lock, RLock, Semaphore, BoundedSemaphore
from threading import Event, Condition, Barrier
from queue import Queue


class DummyProcess(threading.Thread):
    """A thread presented as a Process: it has an exitcode and is listed
    by active_children() of the thread that started it."""

    def __init__(self, group=None, target=None, name=None, args=(), kwargs=None):
        super().__init__(group, target, name, args, kwargs)
        self._pid = None
        self._children = weakref.WeakKeyDictionary()
        self._start_called = False
        self._parent = current_process()

    def start(self):
        starter = current_process()
        if self._parent is not starter:
            raise RuntimeError(
                "Parent is {0!r} but current_process is {1!r}".format(
                    self._parent, starter))
        self._start_called = True
        children = getattr(self._parent, '_children', None)
        if children is not None:
            children[self] = None
        super().start()

    @property
    def exitcode(self):
        # 0 once a started thread has finished, None before.
        if self._start_called and not self.is_alive():
            return 0
        return None


Process = DummyProcess
current_process = threading.current_thread
# The main thread plays the main process and records its children too.
current_process()._children = weakref.WeakKeyDictionary()


def active_children():
    """Started child threads of the calling thread that are still alive."""
    children = current_process()._children
    for child in list(children):
        if not child.is_alive():
            children.pop(child, None)
    return list(children)


def freeze_support():
    pass


class Namespace(object):
    """Plain attribute container (what a manager's Namespace() gives)."""

    def __init__(self, /, **kwds):
        self.__dict__.update(kwds)

    def __repr__(self):
        shown = sorted(f'{key}={value!r}' for key, value in self.__dict__.items()
                       if not key.startswith('_'))
        return f"{type(self).__name__}({', '.join(shown)})"


dict = dict
list = list


def Array(typecode, sequence, lock=True):
    import array
    return array.array(typecode, sequence)


class Value(object):
    """A mutable box with a .value attribute (no locking needed)."""

    def __init__(self, typecode, value, lock=True):
        self._typecode = typecode
        self._value = value

    @property
    def value(self):
        return self._value

    @value.setter
    def value(self, value):
        self._value = value

    def __repr__(self):
        return f'<{type(self).__name__}({self._typecode!r}, {self._value!r})>'


def Manager():
    # Threads share memory already: the "manager" is this module.
    return sys.modules[__name__]


def shutdown():
    pass


def Pool(processes=None, initializer=None, initargs=()):
    from ..pool import ThreadPool
    return ThreadPool(processes, initializer, initargs)


JoinableQueue = Queue
