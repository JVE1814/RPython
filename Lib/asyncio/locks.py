"""Synchronization primitives."""

__all__ = ('Lock', 'Event', 'Condition', 'Semaphore',
           'BoundedSemaphore', 'Barrier')

import collections
import enum

from . import exceptions
from . import mixins


class _ContextManagerMixin:
    async def __aenter__(self):
        await self.acquire()
        # Returning None (not self) matches threading primitives.
        return None

    async def __aexit__(self, exc_type, exc, tb):
        self.release()


class Lock(_ContextManagerMixin, mixins._LoopBoundMixin):
    """Primitive lock: acquire() is a coroutine, release() is not.

    Fairness: waiters acquire the lock in FIFO order; a released lock is
    handed to the first waiter that is still waiting.
    """

    def __init__(self):
        self._waiters = None
        self._locked = False

    def __repr__(self):
        res = super().__repr__()
        extra = 'locked' if self._locked else 'unlocked'
        if self._waiters:
            extra = f'{extra}, waiters:{len(self._waiters)}'
        return f'<{res[1:-1]} [{extra}]>'

    def locked(self):
        """Return True if lock is acquired."""
        return self._locked

    async def acquire(self):
        """Acquire the lock (blocking until it is available); return True."""
        # fast path: free and nobody waiting (all waiters cancelled is fine)
        if (not self._locked and (self._waiters is None or
                all(w.cancelled() for w in self._waiters))):
            self._locked = True
            return True

        if self._waiters is None:
            self._waiters = collections.deque()
        fut = self._get_loop().create_future()
        self._waiters.append(fut)

        try:
            try:
                await fut
            finally:
                self._waiters.remove(fut)
        except exceptions.CancelledError:
            # we may have been woken up just before the cancellation:
            # pass the wakeup on to the next waiter
            if not self._locked:
                self._wake_up_first()
            raise

        self._locked = True
        return True

    def release(self):
        """Release the lock; RuntimeError if it is not locked."""
        if self._locked:
            self._locked = False
            self._wake_up_first()
        else:
            raise RuntimeError('Lock is not acquired.')

    def _wake_up_first(self):
        """Ensure that the first waiter will wake up."""
        if not self._waiters:
            return
        try:
            fut = next(iter(self._waiters))
        except StopIteration:
            return

        # .done() means the waiter was already woken up or cancelled; it
        # will retry by itself.
        if not fut.done():
            fut.set_result(True)


class Event(mixins._LoopBoundMixin):
    """Asynchronous equivalent of threading.Event."""

    def __init__(self):
        self._waiters = collections.deque()
        self._value = False

    def __repr__(self):
        res = super().__repr__()
        extra = 'set' if self._value else 'unset'
        if self._waiters:
            extra = f'{extra}, waiters:{len(self._waiters)}'
        return f'<{res[1:-1]} [{extra}]>'

    def is_set(self):
        """Return True if the internal flag is true."""
        return self._value

    def set(self):
        """Set the flag and wake up all waiters."""
        if not self._value:
            self._value = True

            for fut in self._waiters:
                if not fut.done():
                    fut.set_result(True)

    def clear(self):
        """Reset the internal flag to false."""
        self._value = False

    async def wait(self):
        """Block until the flag is true; return True."""
        if self._value:
            return True

        fut = self._get_loop().create_future()
        self._waiters.append(fut)
        try:
            await fut
            return True
        finally:
            self._waiters.remove(fut)


class Condition(_ContextManagerMixin, mixins._LoopBoundMixin):
    """Asynchronous equivalent of threading.Condition.

    The underlying lock may be passed in (it must be an asyncio Lock).
    """

    def __init__(self, lock=None):
        if lock is None:
            lock = Lock()

        self._lock = lock
        # Export the lock's locked(), acquire() and release() methods.
        self.locked = lock.locked
        self.acquire = lock.acquire
        self.release = lock.release

        self._waiters = collections.deque()

    def __repr__(self):
        res = super().__repr__()
        extra = 'locked' if self.locked() else 'unlocked'
        if self._waiters:
            extra = f'{extra}, waiters:{len(self._waiters)}'
        return f'<{res[1:-1]} [{extra}]>'

    async def wait(self):
        """Release the lock, wait for a notification, re-acquire the lock.

        Returns True.  The lock is re-acquired even if the wait is
        cancelled; the CancelledError is then re-raised.
        """
        if not self.locked():
            raise RuntimeError('cannot wait on un-acquired lock')

        fut = self._get_loop().create_future()
        self.release()
        try:
            try:
                self._waiters.append(fut)
                try:
                    await fut
                    return True
                finally:
                    self._waiters.remove(fut)

            finally:
                # Re-acquire the lock, even when cancelled.
                err = None
                while True:
                    try:
                        await self.acquire()
                        break
                    except exceptions.CancelledError as e:
                        err = e

                if err is not None:
                    try:
                        raise err  # re-raise the original cancellation
                    finally:
                        err = None  # break reference cycles
        except BaseException:
            # A notification consumed by a waiter that does not return True
            # (cancelled or failed): pass it on to another waiter.
            self._notify(1)
            raise

    async def wait_for(self, predicate):
        """Wait until *predicate()* becomes true; return its last value."""
        result = predicate()
        while not result:
            await self.wait()
            result = predicate()
        return result

    def notify(self, n=1):
        """Wake up at most *n* waiters (the lock must be held)."""
        if not self.locked():
            raise RuntimeError('cannot notify on un-acquired lock')
        self._notify(n)

    def _notify(self, n):
        idx = 0
        for fut in self._waiters:
            if idx >= n:
                break

            if not fut.done():
                idx += 1
                fut.set_result(False)

    def notify_all(self):
        """Wake up all waiters (the lock must be held)."""
        self.notify(len(self._waiters))


class Semaphore(_ContextManagerMixin, mixins._LoopBoundMixin):
    """A Semaphore: a counter decremented by acquire() and incremented by
    release(); acquire() blocks while the counter is zero."""

    def __init__(self, value=1):
        if value < 0:
            raise ValueError("Semaphore initial value must be >= 0")
        self._waiters = None
        self._value = value

    def __repr__(self):
        res = super().__repr__()
        extra = 'locked' if self.locked() else f'unlocked, value:{self._value}'
        if self._waiters:
            extra = f'{extra}, waiters:{len(self._waiters)}'
        return f'<{res[1:-1]} [{extra}]>'

    def locked(self):
        """Returns True if semaphore cannot be acquired immediately."""
        # Due to state, or FIFO rules (must allow others to run first).
        return self._value == 0 or (
            any(not w.cancelled() for w in (self._waiters or ())))

    async def acquire(self):
        """Acquire the semaphore (waiting if needed); return True."""
        if not self.locked():
            # fast path: no waiting needed
            self._value -= 1
            return True

        if self._waiters is None:
            self._waiters = collections.deque()
        fut = self._get_loop().create_future()
        self._waiters.append(fut)

        try:
            try:
                await fut
            finally:
                self._waiters.remove(fut)
        except exceptions.CancelledError:
            # Woken up (the value was decremented for us) but cancelled at
            # the same time: give the slot back.
            if fut.done() and not fut.cancelled():
                self._value += 1
            raise

        finally:
            # Several releases may have happened before we could run.
            while self._value > 0:
                if not self._wake_up_next():
                    break  # there was no-one to wake up
        return True

    def release(self):
        """Increment the counter and wake up one waiter."""
        self._value += 1
        self._wake_up_next()

    def _wake_up_next(self):
        """Wake up the first non-done waiter; return True if one was."""
        if not self._waiters:
            return False
        for fut in self._waiters:
            if not fut.done():
                self._value -= 1
                fut.set_result(True)
                # the woken-up waiter does not decrement the value itself
                return True
        return False


class BoundedSemaphore(Semaphore):
    """A Semaphore whose value may not exceed its initial value."""

    def __init__(self, value=1):
        self._bound_value = value
        super().__init__(value)

    def release(self):
        if self._value >= self._bound_value:
            raise ValueError('BoundedSemaphore released too many times')
        super().release()


class _BarrierState(enum.Enum):
    FILLING = 'filling'
    DRAINING = 'draining'
    RESETTING = 'resetting'
    BROKEN = 'broken'


class Barrier(mixins._LoopBoundMixin):
    """Asynchronous equivalent of threading.Barrier: *parties* tasks wait
    until all of them have called wait()."""

    def __init__(self, parties):
        if parties < 1:
            raise ValueError('parties must be >= 1')

        self._cond = Condition()  # notified on any state change

        self._parties = parties
        self._state = _BarrierState.FILLING
        self._count = 0  # tasks currently inside the barrier

    def __repr__(self):
        res = super().__repr__()
        extra = f'{self._state.value}'
        if not self.broken:
            extra += f', waiters:{self.n_waiting}/{self.parties}'
        return f'<{res[1:-1]} [{extra}]>'

    async def __aenter__(self):
        # wait() returns the index; that's what "as" binds to
        return await self.wait()

    async def __aexit__(self, *args):
        pass

    async def wait(self):
        """Wait for the barrier; return an index in range(parties) unique
        to each waiting task."""
        async with self._cond:
            await self._block()  # wait while draining or resetting
            try:
                index = self._count
                self._count += 1
                if index + 1 == self._parties:
                    # last task: release everybody
                    await self._release()
                else:
                    await self._wait()
                return index
            finally:
                self._count -= 1
                # the last one out wakes up tasks waiting to enter
                self._exit()

    async def _block(self):
        # Wait while the previous cycle drains or the barrier resets.
        await self._cond.wait_for(
            lambda: self._state not in (
                _BarrierState.DRAINING, _BarrierState.RESETTING
            )
        )

        if self._state is _BarrierState.BROKEN:
            raise exceptions.BrokenBarrierError("Barrier aborted")

    async def _release(self):
        self._state = _BarrierState.DRAINING
        self._cond.notify_all()

    async def _wait(self):
        await self._cond.wait_for(lambda: self._state is not _BarrierState.FILLING)

        if self._state in (_BarrierState.BROKEN, _BarrierState.RESETTING):
            raise exceptions.BrokenBarrierError("Abort or reset of barrier")

    def _exit(self):
        if self._count == 0:
            if self._state in (_BarrierState.RESETTING, _BarrierState.DRAINING):
                self._state = _BarrierState.FILLING
            self._cond.notify_all()

    async def reset(self):
        """Reset the barrier; current waiters get BrokenBarrierError."""
        async with self._cond:
            if self._count > 0:
                if self._state is not _BarrierState.RESETTING:
                    self._state = _BarrierState.RESETTING
            else:
                self._state = _BarrierState.FILLING
            self._cond.notify_all()

    async def abort(self):
        """Put the barrier into the broken state."""
        async with self._cond:
            self._state = _BarrierState.BROKEN
            self._cond.notify_all()

    @property
    def parties(self):
        """Number of tasks required to trip the barrier."""
        return self._parties

    @property
    def n_waiting(self):
        """Number of tasks blocked in the filling phase."""
        if self._state is _BarrierState.FILLING:
            return self._count
        return 0

    @property
    def broken(self):
        """True if the barrier is broken."""
        return self._state is _BarrierState.BROKEN
