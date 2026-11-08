"""The core of concurrent.futures: Future, the Executor interface, and the
functions wait() and as_completed()."""

import collections
import logging
import threading
import time
import types

FIRST_COMPLETED = 'FIRST_COMPLETED'
FIRST_EXCEPTION = 'FIRST_EXCEPTION'
ALL_COMPLETED = 'ALL_COMPLETED'
_AS_COMPLETED = '_AS_COMPLETED'

# The states of a Future.  A cancelled future moves on to
# CANCELLED_AND_NOTIFIED once its executor has seen the cancellation (and
# told the waiters).
PENDING = 'PENDING'
RUNNING = 'RUNNING'
CANCELLED = 'CANCELLED'
CANCELLED_AND_NOTIFIED = 'CANCELLED_AND_NOTIFIED'
FINISHED = 'FINISHED'

_FUTURE_STATES = [PENDING, RUNNING, CANCELLED, CANCELLED_AND_NOTIFIED, FINISHED]

_STATE_TO_DESCRIPTION_MAP = {
    PENDING: "pending",
    RUNNING: "running",
    CANCELLED: "cancelled",
    CANCELLED_AND_NOTIFIED: "cancelled",
    FINISHED: "finished",
}

_CANCELLED_STATES = (CANCELLED, CANCELLED_AND_NOTIFIED)
_DONE_STATES = (CANCELLED, CANCELLED_AND_NOTIFIED, FINISHED)
# Done as far as waiters are concerned (a plain CANCELLED is reported to
# them only when the executor gets to it).
_SETTLED_STATES = (CANCELLED_AND_NOTIFIED, FINISHED)

LOGGER = logging.getLogger("concurrent.futures")


class Error(Exception):
    """Base class of the concurrent.futures exceptions."""


class CancelledError(Error):
    """The future was cancelled."""


TimeoutError = TimeoutError


class InvalidStateError(Error):
    """The future is not in a state that allows the operation."""


class BrokenExecutor(RuntimeError):
    """The executor can no longer run calls."""


# --- waiters ----------------------------------------------------------------
#
# wait() and as_completed() attach one waiter to every future they watch.
# A future reports to its waiters, under its own condition, when it
# finishes or when its cancellation takes effect; the waiter collects those
# futures and sets its event when the caller should look at them.

class _Waiter(object):
    """Collects the futures that settle.  *wake_when* says when to set the
    event: every time (as_completed), on the first one, on the first
    failure, or once *outstanding* futures have settled."""

    def __init__(self, wake_when, outstanding=0):
        self.event = threading.Event()
        self.lock = threading.Lock()
        self.finished_futures = []
        self.wake_when = wake_when
        self.outstanding = outstanding

    def _settled(self, future, failed):
        with self.lock:
            self.finished_futures.append(future)
            if self.wake_when in (_AS_COMPLETED, FIRST_COMPLETED):
                self.event.set()
            elif failed and self.wake_when == FIRST_EXCEPTION:
                self.event.set()
            else:
                self.outstanding -= 1
                if not self.outstanding:
                    self.event.set()

    def add_result(self, future):
        self._settled(future, failed=False)

    def add_exception(self, future):
        self._settled(future, failed=True)

    def add_cancelled(self, future):
        self._settled(future, failed=False)

    def take(self):
        """Return the futures collected so far and start over."""
        with self.lock:
            collected, self.finished_futures = self.finished_futures, []
            self.event.clear()
        return collected


_WAKE_CONDITIONS = (_AS_COMPLETED, FIRST_COMPLETED, FIRST_EXCEPTION,
                    ALL_COMPLETED)


def _attach_waiter(futures, wake_when):
    # Caller holds every future's condition.
    if wake_when not in _WAKE_CONDITIONS:
        raise ValueError("Invalid return condition: %r" % wake_when)
    outstanding = sum(f._state not in _SETTLED_STATES for f in futures)
    waiter = _Waiter(wake_when, outstanding)
    for f in futures:
        f._waiters.append(waiter)
    return waiter


def _detach_waiter(futures, waiter):
    for f in futures:
        with f._condition:
            f._waiters.remove(waiter)


class _AcquireFutures(object):
    """Hold the conditions of several futures at once.  They are always
    taken in the same (id) order, so two callers cannot deadlock."""

    def __init__(self, futures):
        self.futures = sorted(futures, key=id)

    def __enter__(self):
        for f in self.futures:
            f._condition.acquire()

    def __exit__(self, *exc_info):
        for f in self.futures:
            f._condition.release()


def _split_settled(futures):
    settled = {f for f in futures if f._state in _SETTLED_STATES}
    return settled, futures - settled


def as_completed(fs, timeout=None):
    """Yield the futures of *fs* as they complete (finished or cancelled),
    those already complete first.  Each distinct future is yielded once.

    Raises TimeoutError if not all of them have completed *timeout*
    seconds after the call.
    """
    deadline = None if timeout is None else time.monotonic() + timeout
    watched = set(fs)
    total = len(watched)
    with _AcquireFutures(watched):
        ready, pending = _split_settled(watched)
        waiter = _attach_waiter(watched, _AS_COMPLETED)
    ready = list(ready)
    try:
        yield from _hand_out(ready, waiter, (watched,))
        while pending:
            remaining = None
            if deadline is not None:
                remaining = deadline - time.monotonic()
                if remaining < 0:
                    raise TimeoutError('%d (of %d) futures unfinished' % (
                        len(pending), total))
            waiter.event.wait(remaining)
            arrived = waiter.take()
            # _hand_out() yields from the end: oldest first.
            arrived.reverse()
            yield from _hand_out(arrived, waiter, (watched, pending))
    finally:
        _detach_waiter(watched, waiter)


def _hand_out(futures, waiter, holders):
    # Yield the futures of the list *futures*, last first.  Before a future
    # is yielded every other reference this machinery holds to it is
    # dropped, so that a caller who discards it really frees it.
    while futures:
        future = futures[-1]
        for holder in holders:
            holder.remove(future)
        with future._condition:
            future._waiters.remove(waiter)
        del future
        yield futures.pop()


DoneAndNotDoneFutures = collections.namedtuple(
    'DoneAndNotDoneFutures', 'done not_done')


def wait(fs, timeout=None, return_when=ALL_COMPLETED):
    """Wait until the futures of *fs* satisfy *return_when*
    (FIRST_COMPLETED, FIRST_EXCEPTION or ALL_COMPLETED) or *timeout*
    seconds have passed.

    Returns a named 2-tuple of sets: (done, not_done).
    """
    watched = set(fs)
    with _AcquireFutures(watched):
        done, not_done = _split_settled(watched)
        if _already_satisfied(done, not_done, return_when):
            return DoneAndNotDoneFutures(done, not_done)
        waiter = _attach_waiter(watched, return_when)
    waiter.event.wait(timeout)
    _detach_waiter(watched, waiter)
    done.update(waiter.finished_futures)
    return DoneAndNotDoneFutures(done, watched - done)


def _already_satisfied(done, not_done, return_when):
    if not not_done:
        return True
    if not done:
        return False
    if return_when == FIRST_COMPLETED:
        return True
    if return_when == FIRST_EXCEPTION:
        return any(not f.cancelled() and f.exception() is not None
                   for f in done)
    return False


def _result_or_cancel(fut, timeout=None):
    try:
        try:
            return fut.result(timeout)
        finally:
            fut.cancel()
    finally:
        # The frame must not keep the future (and so its result) alive.
        del fut


# --- Future -----------------------------------------------------------------

class Future(object):
    """The eventual outcome of a call."""

    def __init__(self):
        self._condition = threading.Condition()
        self._state = PENDING
        self._result = None
        self._exception = None
        self._waiters = []
        self._done_callbacks = []

    def _run_callback(self, fn):
        try:
            fn(self)
        except Exception:
            LOGGER.exception('exception calling callback for %r', self)

    def _invoke_callbacks(self):
        for fn in self._done_callbacks:
            self._run_callback(fn)

    def __repr__(self):
        with self._condition:
            text = '<%s at %#x state=%s' % (
                type(self).__name__, id(self),
                _STATE_TO_DESCRIPTION_MAP[self._state])
            if self._state == FINISHED:
                if self._exception is not None:
                    text += ' raised %s' % type(self._exception).__name__
                else:
                    text += ' returned %s' % type(self._result).__name__
            return text + '>'

    def cancel(self):
        """Cancel the call unless it is running or finished.  Returns
        whether the future is (now) cancelled."""
        with self._condition:
            if self._state in (RUNNING, FINISHED):
                return False
            if self._state in _CANCELLED_STATES:
                return True
            self._state = CANCELLED
            self._condition.notify_all()
        self._invoke_callbacks()
        return True

    def cancelled(self):
        """Whether the future was cancelled."""
        with self._condition:
            return self._state in _CANCELLED_STATES

    def running(self):
        """Whether the call is running right now."""
        with self._condition:
            return self._state == RUNNING

    def done(self):
        """Whether the future was cancelled or the call has finished."""
        with self._condition:
            return self._state in _DONE_STATES

    def _outcome(self):
        # Caller holds the condition and the state is FINISHED.
        if self._exception is None:
            return self._result
        try:
            raise self._exception
        finally:
            # The traceback will reference this frame: do not let the frame
            # reference the future.
            self = None

    def _wait_until_done(self, timeout):
        # Caller holds the condition.  Raises CancelledError/TimeoutError.
        if self._state in _CANCELLED_STATES:
            raise CancelledError()
        if self._state != FINISHED:
            self._condition.wait(timeout)
            if self._state in _CANCELLED_STATES:
                raise CancelledError()
            if self._state != FINISHED:
                raise TimeoutError()

    def add_done_callback(self, fn):
        """Arrange for fn(future) to be called when the future is done,
        right away if it already is.  Callbacks run in the thread that
        completes the future; exceptions they raise are logged."""
        with self._condition:
            if self._state not in _DONE_STATES:
                self._done_callbacks.append(fn)
                return
        self._run_callback(fn)

    def result(self, timeout=None):
        """Return the call's result, waiting at most *timeout* seconds.
        Raises CancelledError, TimeoutError, or what the call raised."""
        try:
            with self._condition:
                self._wait_until_done(timeout)
                return self._outcome()
        finally:
            self = None

    def exception(self, timeout=None):
        """Return what the call raised (None if it returned), waiting at
        most *timeout* seconds.  Raises CancelledError or TimeoutError."""
        with self._condition:
            self._wait_until_done(timeout)
            return self._exception

    # -- for Executor implementations (and tests) ---------------------------

    def set_running_or_notify_cancel(self):
        """Called by an executor before it runs the call.  Returns True
        after marking the future running, or False if it was cancelled
        (the waiters are told, and the call must not run)."""
        with self._condition:
            if self._state == CANCELLED:
                self._state = CANCELLED_AND_NOTIFIED
                for waiter in self._waiters:
                    waiter.add_cancelled(self)
                return False
            if self._state == PENDING:
                self._state = RUNNING
                return True
            LOGGER.critical('Future %s in unexpected state: %s',
                            id(self), self._state)
            raise RuntimeError('Future in unexpected state')

    def _finish(self, result, exception):
        with self._condition:
            if self._state in _DONE_STATES:
                raise InvalidStateError('{}: {!r}'.format(self._state, self))
            self._result = result
            self._exception = exception
            self._state = FINISHED
            for waiter in self._waiters:
                if exception is None:
                    waiter.add_result(self)
                else:
                    waiter.add_exception(self)
            self._condition.notify_all()
        self._invoke_callbacks()

    def set_result(self, result):
        """Record the call's result and wake everyone waiting."""
        self._finish(result, None)

    def set_exception(self, exception):
        """Record the exception the call raised and wake everyone waiting."""
        self._finish(None, exception)

    def _get_snapshot(self):
        """(done, cancelled, result, exception), read in one go."""
        with self._condition:
            if self._state == FINISHED:
                return True, False, self._result, self._exception
            if self._state in _CANCELLED_STATES:
                return True, True, None, None
            return False, False, None, None

    __class_getitem__ = classmethod(types.GenericAlias)


# --- Executor ---------------------------------------------------------------

def _first(iterator, n):
    # Up to n items of *iterator*.
    for _ in range(n):
        try:
            yield next(iterator)
        except StopIteration:
            return


class Executor(object):
    """Base class of the executors."""

    def submit(self, fn, /, *args, **kwargs):
        """Schedule fn(*args, **kwargs); return a Future for its outcome."""
        raise NotImplementedError()

    def map(self, fn, *iterables, timeout=None, chunksize=1, buffersize=None):
        """Like the builtin map(), but the calls are executed asynchronously.

        All calls are submitted at once, unless *buffersize* bounds how many
        submitted results may wait for the consumer.  The iterator raises
        TimeoutError if a result is not available *timeout* seconds after
        map() was called, and re-raises the exception of a failed call (the
        calls not yet consumed are then cancelled).
        """
        if buffersize is not None and not isinstance(buffersize, int):
            raise TypeError("buffersize must be an integer or None")
        if buffersize is not None and buffersize < 1:
            raise ValueError("buffersize must be None or > 0")
        deadline = None if timeout is None else time.monotonic() + timeout
        arguments = zip(*iterables)
        if buffersize:
            futures = collections.deque(
                self.submit(fn, *args) for args in _first(arguments, buffersize))
            next_future = futures.popleft
        else:
            futures = [self.submit(fn, *args) for args in arguments]
            futures.reverse()
            next_future = futures.pop
        # The iterator must not keep the executor alive.
        import weakref
        executor_ref = weakref.ref(self)

        def results():
            try:
                while futures:
                    if buffersize:
                        executor = executor_ref()
                        if executor is not None:
                            args = next(arguments, None)
                            if args is not None:
                                futures.append(executor.submit(fn, *args))
                        executor = None
                    # No local may hold the future while suspended: callers
                    # count on results being freed once consumed.
                    if deadline is None:
                        yield _result_or_cancel(next_future())
                    else:
                        yield _result_or_cancel(next_future(),
                                                deadline - time.monotonic())
            finally:
                for future in futures:
                    future.cancel()
        return results()

    def shutdown(self, wait=True, *, cancel_futures=False):
        """Free the executor's resources; no submit() may follow.  With
        *wait*, return only after the pending calls have run (or, with
        *cancel_futures*, been cancelled if they had not started)."""
        pass

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        self.shutdown(wait=True)
        return False
