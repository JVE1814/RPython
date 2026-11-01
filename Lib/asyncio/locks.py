"""Synchronization primitives for coroutines (not thread-safe)."""

__all__ = ('Lock', 'Event', 'Condition', 'Semaphore',
           'BoundedSemaphore', 'Barrier')

import collections
import enum

from . import exceptions
from . import mixins


def _describe(obj, state, waiters):
    base = object.__repr__(obj)[1:-1]
    if waiters:
        state = f'{state}, waiters:{waiters}'
    return f'<{base} [{state}]>'


def _wake(fut, value=True):
    """Complete a waiter future if it is still pending; True if woken."""
    if fut.done():
        return False
    fut.set_result(value)
    return True


class _ContextManagerMixin:
    async def __aenter__(self):
        await self.acquire()
        return None

    async def __aexit__(self, exc_type, exc, tb):
        self.release()


class Lock(_ContextManagerMixin, mixins._LoopBoundMixin):
    """Mutual exclusion lock with FIFO hand-over to waiters."""

    def __init__(self):
        self._waiters = None   # deque of futures, created on demand
        self._locked = False

    def __repr__(self):
        return _describe(self, 'locked' if self._locked else 'unlocked',
                         len(self._waiters) if self._waiters else 0)

    def locked(self):
        """True if the lock is held."""
        return self._locked

    def _has_live_waiters(self):
        return bool(self._waiters) and \
            any(not w.cancelled() for w in self._waiters)

    async def acquire(self):
        """Wait until the lock is free, then take it; returns True."""
        if not self._locked and not self._has_live_waiters():
            self._locked = True
            return True
        if self._waiters is None:
            self._waiters = collections.deque()
        ticket = self._get_loop().create_future()
        self._waiters.append(ticket)
        try:
            try:
                await ticket
            finally:
                self._waiters.remove(ticket)
        except exceptions.CancelledError:
            # A wake-up that arrives together with the cancellation must
            # not be lost: pass it to the next waiter.
            if not self._locked:
                self._wake_up_first()
            raise
        self._locked = True
        return True

    def release(self):
        """Release the lock (RuntimeError if it is not held)."""
        if not self._locked:
            raise RuntimeError('Lock is not acquired.')
        self._locked = False
        self._wake_up_first()

    def _wake_up_first(self):
        if self._waiters:
            _wake(self._waiters[0])


class Event(mixins._LoopBoundMixin):
    """A flag tasks can wait for (like threading.Event)."""

    def __init__(self):
        self._waiters = collections.deque()
        self._value = False

    def __repr__(self):
        return _describe(self, 'set' if self._value else 'unset',
                         len(self._waiters))

    def is_set(self):
        return self._value

    def set(self):
        """Set the flag and wake every waiter."""
        if self._value:
            return
        self._value = True
        for fut in self._waiters:
            _wake(fut)

    def clear(self):
        self._value = False

    async def wait(self):
        """Wait until the flag is set; returns True."""
        if self._value:
            return True
        fut = self._get_loop().create_future()
        self._waiters.append(fut)
        try:
            await fut
        finally:
            self._waiters.remove(fut)
        return True


class Condition(_ContextManagerMixin, mixins._LoopBoundMixin):
    """Condition variable on top of an asyncio Lock."""

    def __init__(self, lock=None):
        self._lock = Lock() if lock is None else lock
        self.locked = self._lock.locked
        self.acquire = self._lock.acquire
        self.release = self._lock.release
        self._waiters = collections.deque()

    def __repr__(self):
        return _describe(self, 'locked' if self.locked() else 'unlocked',
                         len(self._waiters))

    async def _reacquire(self):
        # The lock must be held again on return, even through repeated
        # cancellations; the first cancellation is re-raised afterwards.
        cancelled = None
        while True:
            try:
                await self.acquire()
                break
            except exceptions.CancelledError as exc:
                if cancelled is None:
                    cancelled = exc
        if cancelled is not None:
            try:
                raise cancelled
            finally:
                cancelled = None

    async def wait(self):
        """Release the lock, wait for notify(), take the lock back."""
        if not self.locked():
            raise RuntimeError('cannot wait on un-acquired lock')
        signal = self._get_loop().create_future()
        self.release()
        try:
            try:
                self._waiters.append(signal)
                try:
                    await signal
                finally:
                    self._waiters.remove(signal)
            finally:
                await self._reacquire()
        except BaseException:
            # we may have consumed a notification: hand it on
            self._notify(1)
            raise
        return True

    async def wait_for(self, predicate):
        """Wait until predicate() is true; return its value."""
        outcome = predicate()
        while not outcome:
            await self.wait()
            outcome = predicate()
        return outcome

    def notify(self, n=1):
        """Wake up to *n* waiters (the lock must be held)."""
        if not self.locked():
            raise RuntimeError('cannot notify on un-acquired lock')
        self._notify(n)

    def _notify(self, n):
        woken = 0
        for fut in self._waiters:
            if woken >= n:
                break
            if _wake(fut, False):
                woken += 1

    def notify_all(self):
        """Wake every waiter (the lock must be held)."""
        self.notify(len(self._waiters))


class Semaphore(_ContextManagerMixin, mixins._LoopBoundMixin):
    """Counter limiting concurrent access; acquire() waits at zero."""

    def __init__(self, value=1):
        if value < 0:
            raise ValueError("Semaphore initial value must be >= 0")
        self._waiters = None
        self._value = value

    def __repr__(self):
        state = 'locked' if self.locked() else f'unlocked, value:{self._value}'
        return _describe(self, state,
                         len(self._waiters) if self._waiters else 0)

    def locked(self):
        """True if acquire() would have to wait."""
        if self._value == 0:
            return True
        return bool(self._waiters) and \
            any(not w.cancelled() for w in self._waiters)

    async def acquire(self):
        """Decrement the counter, waiting while it is zero; returns True."""
        if not self.locked():
            self._value -= 1
            return True
        if self._waiters is None:
            self._waiters = collections.deque()
        ticket = self._get_loop().create_future()
        self._waiters.append(ticket)
        try:
            try:
                await ticket
            finally:
                self._waiters.remove(ticket)
        except exceptions.CancelledError:
            # the unit reserved for us by _wake_up_next() goes back
            if ticket.done() and not ticket.cancelled():
                self._value += 1
            raise
        finally:
            # more units may have been released meanwhile
            while self._value > 0 and self._wake_up_next():
                pass
        return True

    def release(self):
        """Increment the counter and wake one waiter."""
        self._value += 1
        self._wake_up_next()

    def _wake_up_next(self):
        # The unit is reserved for the woken waiter here.
        for fut in self._waiters or ():
            if _wake(fut):
                self._value -= 1
                return True
        return False


class BoundedSemaphore(Semaphore):
    """Semaphore refusing to grow above its initial value."""

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
    """Rendezvous point for a fixed number of tasks."""

    def __init__(self, parties):
        if parties < 1:
            raise ValueError('parties must be >= 1')
        self._parties = parties
        self._cond = Condition()
        self._state = _BarrierState.FILLING
        self._count = 0

    def __repr__(self):
        state = self._state.value
        if not self.broken:
            state += f', waiters:{self.n_waiting}/{self.parties}'
        return _describe(self, state, 0)

    async def __aenter__(self):
        return await self.wait()

    async def __aexit__(self, *exc_info):
        pass

    async def wait(self):
        """Wait until *parties* tasks arrived; return this task's index."""
        async with self._cond:
            # a previous round must finish leaving first
            await self._cond.wait_for(lambda: self._state not in (
                _BarrierState.DRAINING, _BarrierState.RESETTING))
            if self._state is _BarrierState.BROKEN:
                raise exceptions.BrokenBarrierError("Barrier aborted")
            index = self._count
            self._count += 1
            try:
                if self._count == self._parties:
                    self._state = _BarrierState.DRAINING
                    self._cond.notify_all()
                else:
                    await self._cond.wait_for(
                        lambda: self._state is not _BarrierState.FILLING)
                    if self._state in (_BarrierState.BROKEN,
                                       _BarrierState.RESETTING):
                        raise exceptions.BrokenBarrierError(
                            "Abort or reset of barrier")
                return index
            finally:
                self._count -= 1
                if self._count == 0:
                    if self._state in (_BarrierState.DRAINING,
                                       _BarrierState.RESETTING):
                        self._state = _BarrierState.FILLING
                    self._cond.notify_all()

    async def reset(self):
        """Return to the initial state; current waiters get
        BrokenBarrierError."""
        async with self._cond:
            if self._count > 0:
                self._state = _BarrierState.RESETTING
            else:
                self._state = _BarrierState.FILLING
            self._cond.notify_all()

    async def abort(self):
        """Break the barrier: all current and future wait() calls fail."""
        async with self._cond:
            self._state = _BarrierState.BROKEN
            self._cond.notify_all()

    @property
    def parties(self):
        return self._parties

    @property
    def n_waiting(self):
        return self._count if self._state is _BarrierState.FILLING else 0

    @property
    def broken(self):
        return self._state is _BarrierState.BROKEN
