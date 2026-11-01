"""Unit testing framework.

Write tests as methods (named test*) of TestCase subclasses, checking
conditions with the assert* methods, then run them with unittest.main(),
'python -m unittest' or a TestRunner:

    import unittest

    class ArithmeticTest(unittest.TestCase):
        def test_addition(self):
            self.assertEqual(1 + 1, 2)

    if __name__ == '__main__':
        unittest.main()

TestLoader collects tests into TestSuites, TestResult records outcomes
and TextTestRunner reports them.  unittest.mock provides mock objects.
"""

__all__ = ['TestResult', 'TestCase', 'IsolatedAsyncioTestCase', 'TestSuite',
           'TextTestRunner', 'TestLoader', 'FunctionTestCase', 'main',
           'defaultTestLoader', 'SkipTest', 'skip', 'skipIf', 'skipUnless',
           'expectedFailure', 'TextTestResult', 'installHandler',
           'registerResult', 'removeResult', 'removeHandler',
           'addModuleCleanup', 'doModuleCleanups', 'enterModuleContext']

__unittest = True

from .result import TestResult
from .case import (addModuleCleanup, TestCase, FunctionTestCase, SkipTest,
                   skip, skipIf, skipUnless, expectedFailure,
                   doModuleCleanups, enterModuleContext)
from .suite import BaseTestSuite, TestSuite
from .loader import TestLoader, defaultTestLoader
from .main import TestProgram, main
from .runner import TextTestRunner, TextTestResult
from .signals import (installHandler, registerResult, removeResult,
                      removeHandler)


def __dir__():
    return list(globals()) + ['IsolatedAsyncioTestCase']


def __getattr__(name):
    # asyncio is heavy: import the async test case only when asked for
    if name == 'IsolatedAsyncioTestCase':
        global IsolatedAsyncioTestCase
        from .async_case import IsolatedAsyncioTestCase
        return IsolatedAsyncioTestCase
    raise AttributeError(f'module {__name__!r} has no attribute {name!r}')
