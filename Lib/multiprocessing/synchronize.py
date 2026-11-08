#
# multiprocessing.synchronize -- synchronisation primitives that work
# across processes.  Every one of them is built from named POSIX
# semaphores (_multiprocessing.SemLock); the Barrier keeps its counters in
# shared memory.
#

__all__ = [
    'Lock', 'RLock', 'Semaphore', 'BoundedSemaphore', 'Condition', 'Event'
    ]

import os
import sys
import threading
import time

from . import context
from . import process
from . import util

try:
    from _multiprocessing import SemLock as _SemLockType, sem_unlink
except ImportError:
    raise ImportError("This platform lacks a functioning sem_open"
                      " implementation")

# The two kinds of _multiprocessing.SemLock.
RECURSIVE_MUTEX = 0
SEMAPHORE = 1

SEM_VALUE_MAX = _SemLockType.SEM_VALUE_MAX

# Attempts at finding an unused semaphore name before giving up.
_NAME_ATTEMPTS = 100


def _drop_after_fork_state(lock):
    lock._semlock._after_fork()


class SemLock(object):
    """Owner of one _multiprocessing.SemLock.

    With the fork start method children inherit the semaphore itself, so
    its name is removed straight away.  With the other methods a child
    reopens it by name when unpickling; the name is then removed by a
    finalizer here, or by the resource tracker if this process dies."""

    def __init__(self, kind, value, maxvalue, *, ctx):
        if ctx is None:
            ctx = context._default_context.get_context()
        self._is_fork_ctx = ctx.get_start_method() == 'fork'
        unlink_now = self._is_fork_ctx or sys.platform == 'win32'
        self._semlock = self._create(kind, value, maxvalue, unlink_now)
        util.debug('created semlock with handle %s' % self._semlock.handle)
        self._make_methods()
        if sys.platform != 'win32':
            util.register_after_fork(self, _drop_after_fork_state)
        name = self._semlock.name
        if name is not None:
            from .resource_tracker import register
            register(name, "semaphore")
            util.Finalize(self, SemLock._cleanup, (name,), exitpriority=0)

    @classmethod
    def _create(cls, kind, value, maxvalue, unlink_now):
        for _ in range(_NAME_ATTEMPTS):
            try:
                return _SemLockType(kind, value, maxvalue, cls._make_name(),
                                    unlink_now)
            except FileExistsError:
                pass
        raise FileExistsError('cannot find name for semaphore')

    @staticmethod
    def _make_name():
        prefix = process.current_process()._config['semprefix']
        return f'{prefix}-{os.urandom(6).hex()}'

    @staticmethod
    def _cleanup(name):
        from .resource_tracker import unregister
        sem_unlink(name)
        unregister(name, "semaphore")

    def _make_methods(self):
        # Bound methods of the native object: no extra Python frame per call.
        self.acquire = self._semlock.acquire
        self.release = self._semlock.release

    def __enter__(self):
        return self._semlock.__enter__()

    def __exit__(self, *exc_info):
        return self._semlock.__exit__(*exc_info)

    def locked(self):
        """True if acquire() would block right now."""
        return self._semlock._is_zero()

    def __getstate__(self):
        context.assert_spawning(self)
        if self._is_fork_ctx:
            raise RuntimeError('A SemLock created in a fork context is being '
                               'shared with a process in a spawn context. This is '
                               'not supported. Please use the same context to create '
                               'multiprocessing objects and Process.')
        native = self._semlock
        return (native.handle, native.kind, native.maxvalue, native.name)

    def __setstate__(self, state):
        self._semlock = _SemLockType._rebuild(*state)
        util.debug('recreated blocker with handle %r' % state[0])
        self._make_methods()
        # Whatever context made it, this copy was not inherited by fork.
        self._is_fork_ctx = False


def _value_text(native):
    try:
        return native._get_value()
    except Exception:
        return 'unknown'


class Semaphore(SemLock):

    def __init__(self, value=1, *, ctx):
        super().__init__(SEMAPHORE, value, SEM_VALUE_MAX, ctx=ctx)

    def get_value(self):
        return self._semlock._get_value()

    def __repr__(self):
        return f'<{type(self).__name__}(value={_value_text(self._semlock)})>'


class BoundedSemaphore(Semaphore):

    def __init__(self, value=1, *, ctx):
        SemLock.__init__(self, SEMAPHORE, value, value, ctx=ctx)

    def __repr__(self):
        return (f'<{type(self).__name__}(value={_value_text(self._semlock)}, '
                f'maxvalue={self._semlock.maxvalue})>')


def _holder_description():
    # "ProcessName" from the main thread, "ProcessName|ThreadName" otherwise.
    label = process.current_process().name
    thread = threading.current_thread().name
    return label if thread == 'MainThread' else f'{label}|{thread}'


def _foreign_holder(native):
    # Who holds a lock that is not ours: a thread of this process (the
    # native count is per process) or another process.
    return 'SomeOtherThread' if native._count() > 0 else 'SomeOtherProcess'


class Lock(SemLock):
    """A non-recursive lock shared between processes."""

    def __init__(self, *, ctx):
        super().__init__(SEMAPHORE, 1, 1, ctx=ctx)

    def __repr__(self):
        native = self._semlock
        try:
            if native._is_mine():
                owner = _holder_description()
            elif native._is_zero():
                owner = _foreign_holder(native)
            else:
                owner = 'None'
        except Exception:
            owner = 'unknown'
        return f'<{type(self).__name__}(owner={owner})>'


class RLock(SemLock):
    """A recursive lock shared between processes."""

    def __init__(self, *, ctx):
        super().__init__(RECURSIVE_MUTEX, 1, 1, ctx=ctx)

    def __repr__(self):
        native = self._semlock
        try:
            if native._is_mine():
                owner, depth = _holder_description(), native._count()
            elif native._is_zero():
                owner, depth = _foreign_holder(native), 'nonzero'
            else:
                owner, depth = 'None', 0
        except Exception:
            owner = depth = 'unknown'
        return f'<{type(self).__name__}({owner}, {depth})>'


class Condition(object):
    """A condition variable for processes.

    Three semaphores do the bookkeeping: a waiter posts _sleeping_count
    before it blocks on _wait_semaphore and posts _woken_count when it
    resumes, for whatever reason.  notify() first cancels sleepers that
    have already resumed on their own (timeouts), then hands out wake-ups
    and waits until each one has been taken."""

    def __init__(self, lock=None, *, ctx):
        self._lock = lock or ctx.RLock()
        self._sleeping_count = ctx.Semaphore(0)
        self._woken_count = ctx.Semaphore(0)
        self._wait_semaphore = ctx.Semaphore(0)
        self._make_methods()

    def _parts(self):
        return (self._lock, self._sleeping_count, self._woken_count,
                self._wait_semaphore)

    def __getstate__(self):
        context.assert_spawning(self)
        return self._parts()

    def __setstate__(self, state):
        (self._lock, self._sleeping_count, self._woken_count,
         self._wait_semaphore) = state
        self._make_methods()

    def _make_methods(self):
        self.acquire = self._lock.acquire
        self.release = self._lock.release

    def __enter__(self):
        return self._lock.__enter__()

    def __exit__(self, *exc_info):
        return self._lock.__exit__(*exc_info)

    def __repr__(self):
        try:
            waiting = (self._sleeping_count._semlock._get_value()
                       - self._woken_count._semlock._get_value())
        except Exception:
            waiting = 'unknown'
        return f'<{type(self).__name__}({self._lock}, {waiting})>'

    def wait(self, timeout=None):
        assert self._lock._semlock._is_mine(), \
            'must acquire() condition before using wait()'
        self._sleeping_count.release()
        # Give the lock up completely, however deep a recursive lock is held,
        # and take it back to the same depth afterwards.
        depth = self._lock._semlock._count()
        for _ in range(depth):
            self._lock.release()
        try:
            return self._wait_semaphore.acquire(True, timeout)
        finally:
            self._woken_count.release()
            for _ in range(depth):
                self._lock.acquire()

    def notify(self, n=1):
        assert self._lock._semlock._is_mine(), 'lock is not owned'
        assert not self._wait_semaphore.acquire(False)
        sleeping = self._sleeping_count
        woken = self._woken_count
        wakeup = self._wait_semaphore
        # Each pending "woken" without a matching wake-up from us is a
        # waiter that timed out: remove it from the sleepers.
        while woken.acquire(False):
            cancelled = sleeping.acquire(False)
            assert cancelled, ('notify: Bug in sleeping_count.acquire'
                               + '- res should not be False')
        handed_out = 0
        while handed_out < n and sleeping.acquire(False):
            wakeup.release()
            handed_out += 1
        if not handed_out:
            return
        for _ in range(handed_out):
            woken.acquire()
        # A waiter may have timed out after being counted; take back the
        # wake-ups nobody consumed.
        while wakeup.acquire(False):
            pass

    def notify_all(self):
        self.notify(n=sys.maxsize)

    def wait_for(self, predicate, timeout=None):
        result = predicate()
        if result:
            return result
        deadline = None if timeout is None else time.monotonic() + timeout
        remaining = None
        while not result:
            if deadline is not None:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    break
            self.wait(remaining)
            result = predicate()
        return result


class Event(object):
    """A flag that processes can set, clear and wait for.  The flag is a
    semaphore holding 0 or 1, changed only under the condition's lock."""

    def __init__(self, *, ctx):
        self._cond = ctx.Condition(ctx.Lock())
        self._flag = ctx.Semaphore(0)

    def _peek(self):
        # Caller holds the condition.  Read the flag without changing it.
        if self._flag.acquire(False):
            self._flag.release()
            return True
        return False

    def is_set(self):
        with self._cond:
            return self._peek()

    def set(self):
        with self._cond:
            self._flag.acquire(False)
            self._flag.release()
            self._cond.notify_all()

    def clear(self):
        with self._cond:
            self._flag.acquire(False)

    def wait(self, timeout=None):
        with self._cond:
            if not self._peek():
                self._cond.wait(timeout)
            return self._peek()

    def __repr__(self):
        state = 'set' if self.is_set() else 'unset'
        return f"<{type(self).__qualname__} at {id(self):#x} {state}>"


class Barrier(threading.Barrier):
    """threading.Barrier with its state and count in shared memory, so all
    processes see the same barrier."""

    def __init__(self, parties, action=None, timeout=None, *, ctx):
        import struct
        from .heap import BufferWrapper
        shared = BufferWrapper(struct.calcsize('i') * 2)
        self.__setstate__((parties, action, timeout, ctx.Condition(), shared))
        self._state = 0
        self._count = 0

    def __getstate__(self):
        return (self._parties, self._action, self._timeout,
                self._cond, self._wrapper)

    def __setstate__(self, state):
        (self._parties, self._action, self._timeout,
         self._cond, self._wrapper) = state
        self._array = self._wrapper.create_memoryview().cast('i')

    # threading.Barrier keeps these two as plain attributes; here they are
    # views on the shared integers.
    _state = property(lambda self: self._array[0],
                      lambda self, value: self._array.__setitem__(0, value))
    _count = property(lambda self: self._array[1],
                      lambda self, value: self._array.__setitem__(1, value))
