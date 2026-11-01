"""IsolatedAsyncioTestCase: TestCase running coroutine tests in a fresh
event loop per test."""

import asyncio
import contextvars
import inspect
import warnings

from .case import TestCase

__unittest = True


class IsolatedAsyncioTestCase(TestCase):
    """A TestCase whose test methods, asyncSetUp/asyncTearDown and cleanups
    may be coroutine functions.

    Each test gets its own event loop (an asyncio.Runner created with
    ``loop_factory``); synchronous and asynchronous parts of a test share
    one contextvars context.
    """

    loop_factory = None

    def __init__(self, methodName='runTest'):
        super().__init__(methodName)
        self._asyncioRunner = None
        self._asyncioTestContext = contextvars.copy_context()

    async def asyncSetUp(self):
        pass

    async def asyncTearDown(self):
        pass

    def addAsyncCleanup(self, func, /, *args, **kwargs):
        """Register a cleanup; coroutine functions are awaited.

        (No check is made whether *func* is asynchronous: any callable
        works, coroutine results are run on the test's loop.)"""
        self.addCleanup(*(func, *args), **kwargs)

    async def enterAsyncContext(self, cm):
        """Enter an asynchronous context manager and exit it as a
        cleanup."""
        cls = type(cm)
        try:
            enter = cls.__aenter__
            exit = cls.__aexit__
        except AttributeError:
            raise TypeError(f"'{cls.__module__}.{cls.__qualname__}' object "
                            f"does not support the asynchronous context "
                            f"manager protocol") from None
        value = await enter(cm)
        self.addAsyncCleanup(exit, cm, None, None, None)
        return value

    def _callSetUp(self):
        # create the loop now so that setUp() can already use it
        self._asyncioRunner.get_loop()
        self._asyncioTestContext.run(self.setUp)
        self._callAsync(self.asyncSetUp)

    def _callTestMethod(self, method):
        returned = self._callMaybeAsync(method)
        if returned is not None:
            warnings.warn('It is deprecated to return a value that is not '
                          f'None from a test case ({method} returned '
                          f'{type(returned).__name__!r})',
                          DeprecationWarning, stacklevel=4)

    def _callTearDown(self):
        self._callAsync(self.asyncTearDown)
        self._asyncioTestContext.run(self.tearDown)

    def _callCleanup(self, function, *args, **kwargs):
        self._callMaybeAsync(function, *args, **kwargs)

    def _callAsync(self, func, /, *args, **kwargs):
        assert self._asyncioRunner is not None, \
            'asyncio runner is not initialized'
        assert inspect.iscoroutinefunction(func), \
            f'{func!r} is not an async function'
        return self._asyncioRunner.run(func(*args, **kwargs),
                                       context=self._asyncioTestContext)

    def _callMaybeAsync(self, func, /, *args, **kwargs):
        assert self._asyncioRunner is not None, \
            'asyncio runner is not initialized'
        if inspect.iscoroutinefunction(func):
            return self._asyncioRunner.run(func(*args, **kwargs),
                                           context=self._asyncioTestContext)
        return self._asyncioTestContext.run(func, *args, **kwargs)

    def _setupAsyncioRunner(self):
        assert self._asyncioRunner is None, \
            'asyncio runner is already initialized'
        self._asyncioRunner = asyncio.Runner(debug=True,
                                             loop_factory=self.loop_factory)

    def _tearDownAsyncioRunner(self):
        self._asyncioRunner.close()

    def run(self, result=None):
        self._setupAsyncioRunner()
        try:
            return super().run(result)
        finally:
            self._tearDownAsyncioRunner()

    def debug(self):
        self._setupAsyncioRunner()
        super().debug()
        self._tearDownAsyncioRunner()

    def __del__(self):
        if self._asyncioRunner is not None:
            self._tearDownAsyncioRunner()
