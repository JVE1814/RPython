"""Core of concurrent.futures: the Future object, the Executor interface and the
helpers wait() and as_completed()."""

import collections
import logging
import threading
import time
import types

FIRST_COMPLETED = 'FIRST_COMPLETED'
FIRST_EXCEPTION = 'FIRST_EXCEPTION'
ALL_COMPLETED = 'ALL_COMPLETED'
_AS_COMPLETED = '_AS_COMPLETED'

# Life cycle of a Future.
PENDING = 'PENDING'
RUNNING = 'RUNNING'
CANCELLED = 'CANCELLED'
# Cancelled and every waiter has been told about it.
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

LOGGER = logging.getLogger("concurrent.futures")


class Error(Exception):
    """Root of the concurrent.futures exception hierarchy."""


class CancelledError(Error):
    """The Future was cancelled."""


TimeoutError = TimeoutError


class InvalidStateError(Error):
    """The operation does not fit the current state of the Future."""


class BrokenExecutor(RuntimeError):
    """The executor cannot run any more work."""


# ---------------------------------------------------------------- waiters
#
# A waiter is attached to every future a wait()/as_completed() call watches;
# futures report their completion to it and it decides when to wake the
# waiting thread.

class _Waiter(object):
    def __init__(self):
        self.event = threading.Event()
        self.finished_futures = []

    def add_result(self, future):
        self.finished_futures.append(future)

    def add_exception(self, future):
        self.finished_futures.append(future)

    def add_cancelled(self, future):
        self.finished_futures.append(future)


class _AsCompletedWaiter(_Waiter):
    """Wakes the consumer each time any future completes."""

    def __init__(self):
        super().__init__()
        self.lock = threading.Lock()

    def _record(self, future):
        with self.lock:
            self.finished_futures.append(future)
            self.event.set()

    add_result = add_exception = add_cancelled = _record


class _FirstCompletedWaiter(_Waiter):
    """Wakes the waiter as soon as one future completes."""

    def _record(self, future):
        self.finished_futures.append(future)
        self.event.set()

    add_result = add_exception = add_cancelled = _record


class _AllCompletedWaiter(_Waiter):
    """Wakes the waiter once every future is done (or, optionally, on the
    first exception)."""

    def __init__(self, num_pending_calls, stop_on_exception):
        super().__init__()
        self.num_pending_calls = num_pending_calls
        self.stop_on_exception = stop_on_exception
        self.lock = threading.Lock()

    def _count_down(self):
        with self.lock:
            self.num_pending_calls -= 1
            if not self.num_pending_calls:
                self.event.set()

    def add_result(self, future):
        self.finished_futures.append(future)
        self._count_down()

    def add_exception(self, future):
        self.finished_futures.append(future)
        if self.stop_on_exception:
            self.event.set()
        else:
            self._count_down()

    def add_cancelled(self, future):
        self.finished_futures.append(future)
        self._count_down()


class _AcquireFutures(object):
    """Context manager holding the conditions of several futures at once
    (always taken in id() order so that concurrent users cannot deadlock)."""

    def __init__(self, futures):
        self.futures = sorted(futures, key=id)

    def __enter__(self):
        for f in self.futures:
            f._condition.acquire()

    def __exit__(self, *exc):
        for f in self.futures:
            f._condition.release()


def _create_and_install_waiters(fs, return_when):
    if return_when == _AS_COMPLETED:
        waiter = _AsCompletedWaiter()
    elif return_when == FIRST_COMPLETED:
        waiter = _FirstCompletedWaiter()
    else:
        pending = sum(f._state not in (CANCELLED_AND_NOTIFIED, FINISHED)
                      for f in fs)
        if return_when == FIRST_EXCEPTION:
            waiter = _AllCompletedWaiter(pending, stop_on_exception=True)
        elif return_when == ALL_COMPLETED:
            waiter = _AllCompletedWaiter(pending, stop_on_exception=False)
        else:
            raise ValueError("Invalid return condition: %r" % return_when)
    for f in fs:
        f._waiters.append(waiter)
    return waiter


def _yield_finished_futures(fs, waiter, ref_collect):
    """Yield the futures of the list *fs* (consuming it from the end) and
    forget every reference to them held by this machinery first, so that a
    caller dropping a yielded future really frees it."""
    while fs:
        f = fs[-1]
        for collection in ref_collect:
            collection.remove(f)
        with f._condition:
            f._waiters.remove(waiter)
        del f
        yield fs.pop()


def as_completed(fs, timeout=None):
    """Iterate over the futures of *fs* in completion order.

    Raises TimeoutError if the futures are not all finished *timeout* seconds
    after the call; duplicate futures are reported once."""
    if timeout is not None:
        deadline = timeout + time.monotonic()

    fs = set(fs)
    total = len(fs)
    with _AcquireFutures(fs):
        done = set(f for f in fs
                   if f._state in (CANCELLED_AND_NOTIFIED, FINISHED))
        pending = fs - done
        waiter = _create_and_install_waiters(fs, _AS_COMPLETED)
    done = list(done)
    try:
        yield from _yield_finished_futures(done, waiter, ref_collect=(fs,))

        while pending:
            if timeout is None:
                wait_timeout = None
            else:
                wait_timeout = deadline - time.monotonic()
                if wait_timeout < 0:
                    raise TimeoutError('%d (of %d) futures unfinished' % (
                        len(pending), total))

            waiter.event.wait(wait_timeout)

            with waiter.lock:
                newly = waiter.finished_futures
                waiter.finished_futures = []
                waiter.event.clear()

            # oldest first: we yield from the end of the list
            newly.reverse()
            yield from _yield_finished_futures(newly, waiter,
                                               ref_collect=(fs, pending))
    finally:
        for f in fs:
            with f._condition:
                f._waiters.remove(waiter)


DoneAndNotDoneFutures = collections.namedtuple(
    'DoneAndNotDoneFutures', 'done not_done')


def wait(fs, timeout=None, return_when=ALL_COMPLETED):
    """Wait for the futures of *fs* according to *return_when*.

    Returns a named 2-tuple of sets (done, not_done)."""
    fs = set(fs)
    with _AcquireFutures(fs):
        done = {f for f in fs if f._state in (CANCELLED_AND_NOTIFIED, FINISHED)}
        not_done = fs - done
        if return_when == FIRST_COMPLETED and done:
            return DoneAndNotDoneFutures(done, not_done)
        if return_when == FIRST_EXCEPTION and done:
            if any(not f.cancelled() and f.exception() is not None
                   for f in done):
                return DoneAndNotDoneFutures(done, not_done)
        if len(done) == len(fs):
            return DoneAndNotDoneFutures(done, not_done)
        waiter = _create_and_install_waiters(fs, return_when)

    waiter.event.wait(timeout)
    for f in fs:
        with f._condition:
            f._waiters.remove(waiter)

    done.update(waiter.finished_futures)
    return DoneAndNotDoneFutures(done, fs - done)


def _result_or_cancel(fut, timeout=None):
    try:
        try:
            return fut.result(timeout)
        finally:
            fut.cancel()
    finally:
        # do not keep the future (and its result) alive through this frame
        del fut


class Future(object):
    """The result of an asynchronous computation."""

    def __init__(self):
        self._condition = threading.Condition()
        self._state = PENDING
        self._result = None
        self._exception = None
        self._waiters = []
        self._done_callbacks = []

    def _invoke_callbacks(self):
        for callback in self._done_callbacks:
            try:
                callback(self)
            except Exception:
                LOGGER.exception('exception calling callback for %r', self)

    def __repr__(self):
        with self._condition:
            state = self._state
            desc = _STATE_TO_DESCRIPTION_MAP[state]
            head = '<%s at %#x state=%s' % (self.__class__.__name__, id(self), desc)
            if state == FINISHED:
                if self._exception is not None:
                    return '%s raised %s>' % (head, self._exception.__class__.__name__)
                return '%s returned %s>' % (head, self._result.__class__.__name__)
            return head + '>'

    def cancel(self):
        """Try to cancel the call.  Returns False if it is already running or
        finished, True otherwise."""
        with self._condition:
            if self._state in (RUNNING, FINISHED):
                return False
            if self._state in (CANCELLED, CANCELLED_AND_NOTIFIED):
                return True
            self._state = CANCELLED
            self._condition.notify_all()
        self._invoke_callbacks()
        return True

    def cancelled(self):
        """True if the future was cancelled."""
        with self._condition:
            return self._state in (CANCELLED, CANCELLED_AND_NOTIFIED)

    def running(self):
        """True if the call is running right now."""
        with self._condition:
            return self._state == RUNNING

    def done(self):
        """True if the future was cancelled or has finished."""
        with self._condition:
            return self._state in (CANCELLED, CANCELLED_AND_NOTIFIED, FINISHED)

    def __get_result(self):
        if self._exception is not None:
            try:
                raise self._exception
            finally:
                # break the cycle exception -> traceback -> frame -> self
                self = None
        return self._result

    def add_done_callback(self, fn):
        """Call *fn(future)* once the future is done (immediately if it
        already is).  Callbacks run in the thread that completes the future."""
        with self._condition:
            if self._state not in (CANCELLED, CANCELLED_AND_NOTIFIED, FINISHED):
                self._done_callbacks.append(fn)
                return
        try:
            fn(self)
        except Exception:
            LOGGER.exception('exception calling callback for %r', self)

    def result(self, timeout=None):
        """Return the call's result, waiting up to *timeout* seconds.

        Raises CancelledError if the future was cancelled, TimeoutError on
        timeout, or the exception raised by the call."""
        try:
            with self._condition:
                if self._state in (CANCELLED, CANCELLED_AND_NOTIFIED):
                    raise CancelledError()
                if self._state == FINISHED:
                    return self.__get_result()
                self._condition.wait(timeout)
                if self._state in (CANCELLED, CANCELLED_AND_NOTIFIED):
                    raise CancelledError()
                if self._state == FINISHED:
                    return self.__get_result()
                raise TimeoutError()
        finally:
            self = None

    def exception(self, timeout=None):
        """Return the exception raised by the call (None if it succeeded),
        waiting up to *timeout* seconds."""
        with self._condition:
            if self._state in (CANCELLED, CANCELLED_AND_NOTIFIED):
                raise CancelledError()
            if self._state == FINISHED:
                return self._exception
            self._condition.wait(timeout)
            if self._state in (CANCELLED, CANCELLED_AND_NOTIFIED):
                raise CancelledError()
            if self._state == FINISHED:
                return self._exception
            raise TimeoutError()

    # The following methods are for Executor implementations and tests only.

    def set_running_or_notify_cancel(self):
        """Mark the future running, or tell the waiters it was cancelled.

        Returns False if the future was cancelled (the call must not run),
        True if it is now running."""
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

    def set_result(self, result):
        """Store the call's result and wake the waiters."""
        with self._condition:
            if self._state in (CANCELLED, CANCELLED_AND_NOTIFIED, FINISHED):
                raise InvalidStateError('{}: {!r}'.format(self._state, self))
            self._result = result
            self._state = FINISHED
            for waiter in self._waiters:
                waiter.add_result(self)
            self._condition.notify_all()
        self._invoke_callbacks()

    def set_exception(self, exception):
        """Store the exception raised by the call and wake the waiters."""
        with self._condition:
            if self._state in (CANCELLED, CANCELLED_AND_NOTIFIED, FINISHED):
                raise InvalidStateError('{}: {!r}'.format(self._state, self))
            self._exception = exception
            self._state = FINISHED
            for waiter in self._waiters:
                waiter.add_exception(self)
            self._condition.notify_all()
        self._invoke_callbacks()

    def _get_snapshot(self):
        """Return (done, cancelled, result, exception) read atomically."""
        with self._condition:
            state = self._state
            if state == FINISHED:
                return True, False, self._result, self._exception
            if state in (CANCELLED, CANCELLED_AND_NOTIFIED):
                return True, True, None, None
            return False, False, None, None

    __class_getitem__ = classmethod(types.GenericAlias)


class Executor(object):
    """Abstract base of the executors."""

    def submit(self, fn, /, *args, **kwargs):
        """Schedule fn(*args, **kwargs) and return a Future for it."""
        raise NotImplementedError()

    def map(self, fn, *iterables, timeout=None, chunksize=1, buffersize=None):
        """Like map(fn, *iterables), but the calls run asynchronously.

        All calls are submitted up front unless *buffersize* limits the
        number of submitted-but-unconsumed results.  The returned iterator
        raises TimeoutError if a result is not ready *timeout* seconds after
        map() was called, and re-raises the exception of a failed call."""
        if buffersize is not None and not isinstance(buffersize, int):
            raise TypeError("buffersize must be an integer or None")
        if buffersize is not None and buffersize < 1:
            raise ValueError("buffersize must be None or > 0")

        if timeout is not None:
            deadline = timeout + time.monotonic()

        zipped = zip(*iterables)
        if buffersize:
            fs = collections.deque(
                self.submit(fn, *args)
                for args in _islice(zipped, buffersize))
        else:
            fs = [self.submit(fn, *args) for args in zipped]

        # A weak reference lets the executor die while the iterator lives.
        import weakref
        executor_ref = weakref.ref(self)

        def result_iterator():
            try:
                # consume from the end of a reversed list for O(1) pops
                if not buffersize:
                    fs.reverse()
                while fs:
                    # Never keep the future (or its result) in a local while
                    # suspended: callers rely on results being freed.
                    if buffersize:
                        executor = executor_ref()
                        if executor is not None:
                            args = next(zipped, None)
                            if args is not None:
                                fs.append(executor.submit(fn, *args))
                        executor = None
                        take = fs.popleft
                    else:
                        take = fs.pop
                    if timeout is None:
                        yield _result_or_cancel(take())
                    else:
                        yield _result_or_cancel(take(), deadline - time.monotonic())
            finally:
                for future in fs:
                    future.cancel()
        return result_iterator()

    def shutdown(self, wait=True, *, cancel_futures=False):
        """Release the executor's resources; no submit() is allowed after
        this.  With *wait*, return only once all pending work has run."""
        pass

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        self.shutdown(wait=True)
        return False


def _islice(iterable, n):
    it = iter(iterable)
    for _ in range(n):
        try:
            yield next(it)
        except StopIteration:
            return
