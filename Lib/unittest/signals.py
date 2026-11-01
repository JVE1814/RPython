"""Control-C handling: the first SIGINT asks running tests to stop
gracefully, a second one interrupts as usual."""

import signal
import weakref

from functools import wraps

__unittest = True


def _ignore_signal(signum, frame):
    pass


class _InterruptHandler(object):
    """SIGINT handler stopping all registered results."""

    def __init__(self, default_handler):
        self.called = False
        self.original_handler = default_handler
        if isinstance(default_handler, int):
            if default_handler == signal.SIG_DFL:
                # behave like the standard KeyboardInterrupt handler
                default_handler = signal.default_int_handler
            elif default_handler == signal.SIG_IGN:
                default_handler = _ignore_signal
            else:
                raise TypeError('expected SIGINT signal handler to be '
                                'signal.SIG_IGN, signal.SIG_DFL, or a '
                                'callable object')
        self.default_handler = default_handler

    def __call__(self, signum, frame):
        if signal.getsignal(signal.SIGINT) is not self:
            # someone replaced us: just pass the signal on
            self.default_handler(signum, frame)
        if self.called:
            # second Control-C: give up on stopping gracefully
            self.default_handler(signum, frame)
        self.called = True
        for result in _results.keys():
            result.stop()


_results = weakref.WeakKeyDictionary()


def registerResult(result):
    """Have *result* stopped (result.stop()) on Control-C."""
    _results[result] = 1


def removeResult(result):
    """Undo registerResult(); returns whether it was registered."""
    return bool(_results.pop(result, None))


_interrupt_handler = None


def installHandler():
    """Install the Control-C handler (once)."""
    global _interrupt_handler
    if _interrupt_handler is None:
        previous = signal.getsignal(signal.SIGINT)
        _interrupt_handler = _InterruptHandler(previous)
        signal.signal(signal.SIGINT, _interrupt_handler)


def removeHandler(method=None):
    """Restore the original SIGINT handler.

    Used as a decorator, the handler is removed only while the decorated
    function runs."""
    if method is not None:
        @wraps(method)
        def without_handler(*args, **kwargs):
            current = signal.getsignal(signal.SIGINT)
            removeHandler()
            try:
                return method(*args, **kwargs)
            finally:
                signal.signal(signal.SIGINT, current)
        return without_handler

    if _interrupt_handler is not None:
        signal.signal(signal.SIGINT, _interrupt_handler.original_handler)
