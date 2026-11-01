"""Deadlines for blocks of asynchronous code."""

import enum

from . import events
from . import exceptions
from . import tasks

__all__ = (
    "Timeout",
    "timeout",
    "timeout_at",
)


class _State(enum.Enum):
    CREATED = "created"
    ENTERED = "active"
    EXPIRING = "expiring"
    EXPIRED = "expired"
    EXITED = "finished"


class Timeout:
    """Async context manager cancelling its block at a deadline.

    The deadline is in loop.time() units (None: no deadline).  When it
    fires, the enclosing task is cancelled and the CancelledError leaving
    the block is turned into TimeoutError.  Create with timeout() or
    timeout_at().
    """

    def __init__(self, when):
        self._state = _State.CREATED
        self._when = when
        self._timeout_handler = None
        self._task = None
        self._cancelling = 0

    def __repr__(self):
        text = f"<Timeout [{self._state.value}]"
        if self._state is _State.ENTERED:
            deadline = None if self._when is None else round(self._when, 3)
            text += f" when={deadline}"
        return text + ">"

    def when(self):
        return self._when

    def expired(self):
        """True once the deadline has passed (cancellation issued)."""
        return self._state in (_State.EXPIRING, _State.EXPIRED)

    def _arm(self):
        if self._timeout_handler is not None:
            self._timeout_handler.cancel()
            self._timeout_handler = None
        if self._when is None:
            return
        loop = events.get_running_loop()
        if self._when <= loop.time():
            self._timeout_handler = loop.call_soon(self._on_timeout)
        else:
            self._timeout_handler = loop.call_at(self._when, self._on_timeout)

    def reschedule(self, when):
        """Move the deadline (None removes it)."""
        if self._state is _State.CREATED:
            raise RuntimeError("Timeout has not been entered")
        if self._state is not _State.ENTERED:
            raise RuntimeError(
                f"Cannot change state of {self._state.value} Timeout")
        self._when = when
        self._arm()

    def _on_timeout(self):
        assert self._state is _State.ENTERED
        self._task.cancel()
        self._state = _State.EXPIRING
        self._timeout_handler = None

    async def __aenter__(self):
        if self._state is not _State.CREATED:
            raise RuntimeError("Timeout has already been entered")
        task = tasks.current_task()
        if task is None:
            raise RuntimeError("Timeout should be used inside a task")
        self._task = task
        self._cancelling = task.cancelling()
        self._state = _State.ENTERED
        self._arm()
        return self

    async def __aexit__(self, exc_type, exc_val, exc_tb):
        assert self._state in (_State.ENTERED, _State.EXPIRING)
        if self._timeout_handler is not None:
            self._timeout_handler.cancel()
            self._timeout_handler = None

        if self._state is _State.ENTERED:
            self._state = _State.EXITED
            return None

        self._state = _State.EXPIRED
        # Only our own cancellation becomes a timeout; outer cancel()
        # requests made meanwhile keep the count above the entry level.
        if self._task.uncancel() > self._cancelling or exc_type is None:
            return None
        if issubclass(exc_type, exceptions.CancelledError):
            raise TimeoutError from exc_val
        if exc_val is not None:
            self._mark_timeout(exc_val)
            if isinstance(exc_val, ExceptionGroup):
                for inner in exc_val.exceptions:
                    self._mark_timeout(inner)
        return None

    @staticmethod
    def _mark_timeout(exc):
        # Splice a TimeoutError between the exception and the CancelledError
        # found in its __context__ chain.
        node = exc
        while node.__context__ is not None:
            if isinstance(node.__context__, exceptions.CancelledError):
                marker = TimeoutError()
                marker.__context__ = marker.__cause__ = node.__context__
                node.__context__ = marker
                return
            node = node.__context__


def timeout(delay):
    """Timeout context manager expiring *delay* seconds from now."""
    loop = events.get_running_loop()
    return Timeout(None if delay is None else loop.time() + delay)


def timeout_at(when):
    """Timeout context manager expiring at loop time *when*."""
    return Timeout(when)
