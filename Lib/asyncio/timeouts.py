import enum

from types import TracebackType

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
    """Asynchronous context manager cancelling the enclosed code at a
    deadline (loop.time() units, None = no deadline).

    Use timeout() / timeout_at() rather than instantiating directly.
    """

    def __init__(self, when):
        self._state = _State.CREATED

        self._timeout_handler = None
        self._task = None
        self._when = when

    def when(self):
        """Return the current deadline."""
        return self._when

    def reschedule(self, when):
        """Change the deadline (None disables it)."""
        if self._state is not _State.ENTERED:
            if self._state is _State.CREATED:
                raise RuntimeError("Timeout has not been entered")
            raise RuntimeError(
                f"Cannot change state of {self._state.value} Timeout",
            )

        self._when = when

        if self._timeout_handler is not None:
            self._timeout_handler.cancel()

        if when is None:
            self._timeout_handler = None
        else:
            loop = events.get_running_loop()
            if when <= loop.time():
                self._timeout_handler = loop.call_soon(self._on_timeout)
            else:
                self._timeout_handler = loop.call_at(when, self._on_timeout)

    def expired(self):
        """Return True if the deadline has been reached."""
        return self._state in (_State.EXPIRING, _State.EXPIRED)

    def __repr__(self):
        info = ['']
        if self._state is _State.ENTERED:
            when = round(self._when, 3) if self._when is not None else None
            info.append(f"when={when}")
        info_str = ' '.join(info)
        return f"<Timeout [{self._state.value}]{info_str}>"

    async def __aenter__(self):
        if self._state is not _State.CREATED:
            raise RuntimeError("Timeout has already been entered")
        task = tasks.current_task()
        if task is None:
            raise RuntimeError("Timeout should be used inside a task")
        self._state = _State.ENTERED
        self._task = task
        self._cancelling = self._task.cancelling()
        self.reschedule(self._when)
        return self

    async def __aexit__(self, exc_type, exc_val, exc_tb):
        assert self._state in (_State.ENTERED, _State.EXPIRING)

        if self._timeout_handler is not None:
            self._timeout_handler.cancel()
            self._timeout_handler = None

        if self._state is _State.EXPIRING:
            self._state = _State.EXPIRED

            if self._task.uncancel() <= self._cancelling and exc_type is not None:
                # our own cancellation, not an outer one: convert it
                if issubclass(exc_type, exceptions.CancelledError):
                    raise TimeoutError from exc_val
                elif exc_val is not None:
                    self._insert_timeout_error(exc_val)
                    if isinstance(exc_val, ExceptionGroup):
                        for exc in exc_val.exceptions:
                            self._insert_timeout_error(exc)
        elif self._state is _State.ENTERED:
            self._state = _State.EXITED

        return None

    def _on_timeout(self):
        assert self._state is _State.ENTERED
        self._task.cancel()
        self._state = _State.EXPIRING
        # drop the handle early
        self._timeout_handler = None

    @staticmethod
    def _insert_timeout_error(exc_val):
        # put a TimeoutError between the CancelledError and what it caused
        while exc_val.__context__ is not None:
            if isinstance(exc_val.__context__, exceptions.CancelledError):
                te = TimeoutError()
                te.__context__ = te.__cause__ = exc_val.__context__
                exc_val.__context__ = te
                break
            exc_val = exc_val.__context__


def timeout(delay):
    """Timeout context manager expiring *delay* seconds from now
    (None: no timeout).  Raises TimeoutError on expiry."""
    loop = events.get_running_loop()
    return Timeout(loop.time() + delay if delay is not None else None)


def timeout_at(when):
    """Like timeout(), with an absolute deadline in loop.time() units."""
    return Timeout(when)
