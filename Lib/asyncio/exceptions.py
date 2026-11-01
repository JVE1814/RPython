"""asyncio exceptions."""


__all__ = ('BrokenBarrierError',
           'CancelledError', 'InvalidStateError', 'TimeoutError',
           'IncompleteReadError', 'LimitOverrunError',
           'SendfileNotAvailableError')


class CancelledError(BaseException):
    """The Future or Task was cancelled."""


TimeoutError = TimeoutError  # the builtin, re-exported


class InvalidStateError(Exception):
    """The operation is not allowed in this state."""


class SendfileNotAvailableError(RuntimeError):
    """sendfile syscall is not available for the given socket or file type.

    Raised when loop.sendfile() cannot use the native implementation.
    """


class IncompleteReadError(EOFError):
    """EOF was reached before the expected number of bytes was read.

    - partial: the bytes read before EOF
    - expected: total number of expected bytes (or None if undefined)
    """
    def __init__(self, partial, expected):
        r_expected = 'undefined' if expected is None else repr(expected)
        super().__init__(f'{len(partial)} bytes read on a total of '
                         f'{r_expected} expected bytes')
        self.partial = partial
        self.expected = expected

    def __reduce__(self):
        return type(self), (self.partial, self.expected)


class LimitOverrunError(Exception):
    """The buffer limit was reached while looking for a separator.

    - consumed: total number of bytes to be consumed
    """
    def __init__(self, message, consumed):
        super().__init__(message)
        self.consumed = consumed

    def __reduce__(self):
        return type(self), (self.args[0], self.consumed)


class BrokenBarrierError(RuntimeError):
    """The Barrier is broken or was reset while a task was waiting."""
