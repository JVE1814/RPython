"""Exception classes of the asyncio package."""

__all__ = ('BrokenBarrierError',
           'CancelledError', 'InvalidStateError', 'TimeoutError',
           'IncompleteReadError', 'LimitOverrunError',
           'SendfileNotAvailableError')

# asyncio uses the builtin TimeoutError.
TimeoutError = TimeoutError


class CancelledError(BaseException):
    """Raised inside a task (or by a future) that was cancelled."""


class InvalidStateError(Exception):
    """A future or task is not in the state the operation requires."""


class SendfileNotAvailableError(RuntimeError):
    """The native sendfile path cannot be used for this socket/file pair."""


class IncompleteReadError(EOFError):
    """The stream ended before the requested amount of data was read.

    Attributes: partial (bytes received) and expected (requested size or
    None).
    """

    def __init__(self, partial, expected):
        wanted = 'undefined' if expected is None else repr(expected)
        super().__init__(f'{len(partial)} bytes read on a total of '
                         f'{wanted} expected bytes')
        self.partial = partial
        self.expected = expected

    def __reduce__(self):
        return (type(self), (self.partial, self.expected))


class LimitOverrunError(Exception):
    """A separator was not found within the stream's buffer limit.

    Attribute consumed: number of bytes that would have to be consumed.
    """

    def __init__(self, message, consumed):
        super().__init__(message)
        self.consumed = consumed

    def __reduce__(self):
        return (type(self), (self.args[0], self.consumed))


class BrokenBarrierError(RuntimeError):
    """A Barrier was broken or reset while tasks were waiting on it."""
