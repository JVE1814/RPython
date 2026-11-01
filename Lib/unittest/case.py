"""TestCase, FunctionTestCase and the skipping decorators."""

import sys
import functools
import difflib
import pprint
import re
import warnings
import collections
import contextlib
import traceback
import time
import types

from . import result
from .util import (strclass, safe_repr, _count_diff_all_purpose,
                   _count_diff_hashable, _common_shorten_repr)

# Frames of modules defining __unittest are hidden from failure tracebacks.
__unittest = True

_subtest_msg_sentinel = object()

DIFF_OMITTED = ('\nDiff is %s characters long. '
                'Set self.maxDiff to None to see it.')


class SkipTest(Exception):
    """Raise this (or call TestCase.skipTest) to skip the current test."""


class _ShouldStop(Exception):
    """Internal: abort the current test (failfast inside a subtest)."""


class _UnexpectedSuccess(Exception):
    """Internal: stands in for the missing failure of an expected-failure
    test when reporting to a result without addUnexpectedSuccess."""


def _report_skip(result, test_case, reason):
    add_skip = getattr(result, 'addSkip', None)
    if add_skip is None:
        warnings.warn('TestResult has no addSkip method, skips not reported',
                      RuntimeWarning, 2)
        result.addSuccess(test_case)
    else:
        add_skip(test_case, reason)


def _report_exception(result, test, exc_info):
    if result is None or exc_info is None:
        return
    if issubclass(exc_info[0], test.failureException):
        result.addFailure(test, exc_info)
    else:
        result.addError(test, exc_info)


class _Outcome:
    """Bookkeeping for one TestCase.run(): collects whether every part
    (setUp, test, tearDown, cleanups, subtests) succeeded."""

    def __init__(self, result=None):
        self.expecting_failure = False
        self.result = result
        self.result_supports_subtests = hasattr(result, 'addSubTest')
        self.success = True
        self.expectedFailure = None

    @contextlib.contextmanager
    def testPartExecutor(self, test_case, subTest=False):
        """Run one part of a test, reporting its exception (if any)."""
        outer_success = self.success
        self.success = True
        try:
            yield
        except KeyboardInterrupt:
            raise
        except SkipTest as exc:
            self.success = False
            _report_skip(self.result, test_case, str(exc))
        except _ShouldStop:
            pass
        except:
            info = sys.exc_info()
            if self.expecting_failure:
                self.expectedFailure = info
            else:
                self.success = False
                if subTest:
                    self.result.addSubTest(test_case.test_case, test_case,
                                           info)
                else:
                    _report_exception(self.result, test_case, info)
            # do not keep the frames (and the exception) alive
            info = None
        else:
            if subTest and self.success:
                self.result.addSubTest(test_case.test_case, test_case, None)
        finally:
            self.success = self.success and outer_success


def _enter_context(cm, addcleanup):
    # Like ExitStack.enter_context: look the methods up on the type.
    cls = type(cm)
    try:
        enter = cls.__enter__
        exit = cls.__exit__
    except AttributeError:
        raise TypeError(f"'{cls.__module__}.{cls.__qualname__}' object does "
                        f"not support the context manager protocol"
                        ) from None
    value = enter(cm)
    addcleanup(exit, cm, None, None, None)
    return value


# -- module level fixtures ----------------------------------------------------

_module_cleanups = []


def addModuleCleanup(function, /, *args, **kwargs):
    """Register a function to run after the module's tests (and its
    tearDownModule) have finished."""
    _module_cleanups.append((function, args, kwargs))


def enterModuleContext(cm):
    """Enter a context manager now and exit it as a module cleanup."""
    return _enter_context(cm, addModuleCleanup)


def doModuleCleanups():
    """Run the registered module cleanups (last in, first out).

    Errors are collected and raised together as an ExceptionGroup."""
    failures = []
    while _module_cleanups:
        function, args, kwargs = _module_cleanups.pop()
        try:
            function(*args, **kwargs)
        except Exception as exc:
            failures.append(exc)
    if failures:
        raise ExceptionGroup('module cleanup failed', failures)


# -- skipping decorators -------------------------------------------------------

def _mark_skipped(test_item, reason):
    if not isinstance(test_item, type):
        original = test_item

        @functools.wraps(original)
        def skip_wrapper(*args, **kwargs):
            raise SkipTest(reason)
        test_item = skip_wrapper
    test_item.__unittest_skip__ = True
    test_item.__unittest_skip_why__ = reason
    return test_item


def skip(reason):
    """Unconditionally skip the decorated test (or class)."""
    if isinstance(reason, types.FunctionType):
        # used bare, as @skip
        return _mark_skipped(reason, '')
    return lambda test_item: _mark_skipped(test_item, reason)


def _keep(test_item):
    return test_item


def skipIf(condition, reason):
    """Skip the decorated test if *condition* is true."""
    if condition:
        return skip(reason)
    return _keep


def skipUnless(condition, reason):
    """Skip the decorated test unless *condition* is true."""
    if not condition:
        return skip(reason)
    return _keep


def expectedFailure(test_item):
    """Mark a test as expected to fail: a failure counts as success and
    passing is reported as an unexpected success."""
    test_item.__unittest_expecting_failure__ = True
    return test_item


def _is_subtype(expected, basetype):
    if isinstance(expected, tuple):
        return all(_is_subtype(item, basetype) for item in expected)
    return isinstance(expected, type) and issubclass(expected, basetype)


# -- context managers behind assertRaises / assertWarns ---------------------------

class _BaseTestCaseContext:

    def __init__(self, test_case):
        self.test_case = test_case

    def _raiseFailure(self, standardMsg):
        msg = self.test_case._formatMessage(self.msg, standardMsg)
        raise self.test_case.failureException(msg)


class _AssertRaisesBaseContext(_BaseTestCaseContext):

    def __init__(self, expected, test_case, expected_regex=None):
        _BaseTestCaseContext.__init__(self, test_case)
        self.expected = expected
        self.test_case = test_case
        if expected_regex is not None:
            expected_regex = re.compile(expected_regex)
        self.expected_regex = expected_regex
        self.obj_name = None
        self.msg = None

    def handle(self, name, args, kwargs):
        """Either return self (context manager use) or call args[0] with
        the remaining arguments inside the context."""
        try:
            if not _is_subtype(self.expected, self._base_type):
                raise TypeError('%s() arg 1 must be %s' %
                                (name, self._base_type_str))
            if not args:
                self.msg = kwargs.pop('msg', None)
                if kwargs:
                    raise TypeError('%r is an invalid keyword argument for '
                                    'this function' % (next(iter(kwargs)),))
                return self
            func, *args = args
            try:
                self.obj_name = func.__name__
            except AttributeError:
                self.obj_name = str(func)
            with self:
                func(*args, **kwargs)
        finally:
            # the context refers to the frame; do not keep it alive
            self = None

    def _expected_name(self):
        try:
            return self.expected.__name__
        except AttributeError:
            return str(self.expected)


class _AssertRaisesContext(_AssertRaisesBaseContext):
    """Context manager used by assertRaises and assertRaisesRegex."""

    _base_type = BaseException
    _base_type_str = 'an exception type or tuple of exception types'

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_value, tb):
        if exc_type is None:
            what = self._expected_name()
            if self.obj_name:
                self._raiseFailure('{} not raised by {}'.format(
                    what, self.obj_name))
            self._raiseFailure('{} not raised'.format(what))
        traceback.clear_frames(tb)
        if not issubclass(exc_type, self.expected):
            # something else went wrong: let it propagate
            return False
        self.exception = exc_value.with_traceback(None)
        regex = self.expected_regex
        if regex is not None and not regex.search(str(exc_value)):
            self._raiseFailure('"{}" does not match "{}"'.format(
                regex.pattern, str(exc_value)))
        return True

    __class_getitem__ = classmethod(types.GenericAlias)


class _AssertWarnsContext(_AssertRaisesBaseContext):
    """Context manager used by assertWarns and assertWarnsRegex."""

    _base_type = Warning
    _base_type_str = 'a warning type or tuple of warning types'

    def __enter__(self):
        # Warnings already shown once would be suppressed by the
        # per-module registries; start from a clean slate.
        for module in list(sys.modules.values()):
            if getattr(module, '__warningregistry__', None):
                module.__warningregistry__ = {}
        self.warnings_manager = warnings.catch_warnings(record=True)
        self.warnings = self.warnings_manager.__enter__()
        warnings.simplefilter('always', self.expected)
        return self

    def __exit__(self, exc_type, exc_value, tb):
        self.warnings_manager.__exit__(exc_type, exc_value, tb)
        # Warnings of other categories went through the normal filters
        # already; show them now as they would have been shown without us.
        for record in self.warnings:
            if not isinstance(record.message, self.expected):
                warnings._showwarnmsg(record)
        if exc_type is not None:
            return
        first_match = None
        regex = self.expected_regex
        for record in self.warnings:
            warning = record.message
            if not isinstance(warning, self.expected):
                continue
            if first_match is None:
                first_match = warning
            if regex is not None and not regex.search(str(warning)):
                continue
            self.warning = warning
            self.filename = record.filename
            self.lineno = record.lineno
            return
        if first_match is not None:
            self._raiseFailure('"{}" does not match "{}"'.format(
                regex.pattern, str(first_match)))
        what = self._expected_name()
        if self.obj_name:
            self._raiseFailure('{} not triggered by {}'.format(
                what, self.obj_name))
        self._raiseFailure('{} not triggered'.format(what))


class _OrderedChainMap(collections.ChainMap):
    """ChainMap whose iteration follows the order of the maps (innermost
    first) and of each map's insertion order."""

    def __iter__(self):
        seen = set()
        for mapping in self.maps:
            for key in mapping:
                if key not in seen:
                    seen.add(key)
                    yield key


# -- TestCase -----------------------------------------------------------------------

class TestCase(object):
    """A single test: one test method of a TestCase subclass.

    Subclasses define test methods (by default those whose names start
    with 'test'), optionally setUp/tearDown, setUpClass/tearDownClass, and
    use the assert* methods to check conditions.  Class attributes:

    * failureException: exception class signalling a test failure
      (AssertionError).
    * longMessage: when true, a custom msg is appended to the standard
      failure message instead of replacing it.
    * maxDiff: maximum length of diffs shown in failure messages
      (None for no limit).
    """

    failureException = AssertionError

    longMessage = True

    maxDiff = 80 * 8

    # Strings longer than this are not compared with difflib.
    _diffThreshold = 2 ** 16

    def __init_subclass__(cls, *args, **kwargs):
        # every class gets its own fixture state
        cls._classSetupFailed = False
        cls._class_cleanups = []
        super().__init_subclass__(*args, **kwargs)

    def __init__(self, methodName='runTest'):
        """Create an instance that will run the method *methodName*.

        Raises ValueError if there is no such method (except for the default
        'runTest', which allows instantiation for interactive use).
        """
        self._testMethodName = methodName
        self._outcome = None
        self._testMethodDoc = 'No test'
        try:
            method = getattr(self, methodName)
        except AttributeError:
            if methodName != 'runTest':
                raise ValueError('no such test method in %s: %s' %
                                 (self.__class__, methodName))
        else:
            self._testMethodDoc = method.__doc__
        self._cleanups = []
        self._subtest = None
        # Names (not bound methods, to avoid reference cycles) of the
        # type-specific assertEqual implementations.
        self._type_equality_funcs = {}
        self.addTypeEqualityFunc(dict, 'assertDictEqual')
        self.addTypeEqualityFunc(list, 'assertListEqual')
        self.addTypeEqualityFunc(tuple, 'assertTupleEqual')
        self.addTypeEqualityFunc(set, 'assertSetEqual')
        self.addTypeEqualityFunc(frozenset, 'assertSetEqual')
        self.addTypeEqualityFunc(str, 'assertMultiLineEqual')

    def addTypeEqualityFunc(self, typeobj, function):
        """Use *function* (a callable or the name of a method) for
        assertEqual() when both arguments are exactly of type *typeobj*.
        It must raise self.failureException(msg) on inequality."""
        self._type_equality_funcs[typeobj] = function

    # -- cleanups and contexts ------------------------------------------------

    def addCleanup(self, function, /, *args, **kwargs):
        """Register a function to call after tearDown (also when setUp
        fails).  Cleanups run last in, first out."""
        self._cleanups.append((function, args, kwargs))

    def enterContext(self, cm):
        """Enter a context manager and exit it as a cleanup."""
        return _enter_context(cm, self.addCleanup)

    @classmethod
    def addClassCleanup(cls, function, /, *args, **kwargs):
        """Register a function to call after tearDownClass (also when
        setUpClass fails)."""
        cls._class_cleanups.append((function, args, kwargs))

    @classmethod
    def enterClassContext(cls, cm):
        """Enter a context manager and exit it as a class cleanup."""
        return _enter_context(cm, cls.addClassCleanup)

    # -- fixtures -----------------------------------------------------------------

    def setUp(self):
        "Hook method for setting up the test fixture before exercising it."

    def tearDown(self):
        "Hook method for deconstructing the test fixture after testing it."

    @classmethod
    def setUpClass(cls):
        "Hook method for setting up class fixture before running tests in the class."

    @classmethod
    def tearDownClass(cls):
        "Hook method for deconstructing the class fixture after running all tests in the class."

    # -- identity -------------------------------------------------------------------

    def countTestCases(self):
        return 1

    def defaultTestResult(self):
        return result.TestResult()

    def shortDescription(self):
        """First line of the test method's docstring, or None."""
        doc = self._testMethodDoc
        if not doc:
            return None
        return doc.strip().split('\n')[0].strip()

    def id(self):
        return '%s.%s' % (strclass(self.__class__), self._testMethodName)

    def __eq__(self, other):
        if type(self) is not type(other):
            return NotImplemented
        return self._testMethodName == other._testMethodName

    def __hash__(self):
        return hash((type(self), self._testMethodName))

    def __str__(self):
        return '%s (%s.%s)' % (self._testMethodName, strclass(self.__class__),
                               self._testMethodName)

    def __repr__(self):
        return '<%s testMethod=%s>' % (strclass(self.__class__),
                                       self._testMethodName)

    # -- running ---------------------------------------------------------------------

    @contextlib.contextmanager
    def subTest(self, msg=_subtest_msg_sentinel, **params):
        """Context manager marking a block as a subtest identified by
        *msg* and *params*: a failure inside is reported for the subtest
        and the test continues after the block."""
        outcome = self._outcome
        if outcome is None or not outcome.result_supports_subtests:
            yield
            return
        parent = self._subtest
        if parent is None:
            chained = _OrderedChainMap(params)
        else:
            chained = parent.params.new_child(params)
        self._subtest = _SubTest(self, msg, chained)
        try:
            with outcome.testPartExecutor(self._subtest, subTest=True):
                yield
            if not outcome.success:
                result = outcome.result
                if result is not None and result.failfast:
                    raise _ShouldStop
            elif outcome.expectedFailure:
                # the expected failure happened: no need to go on
                raise _ShouldStop
        finally:
            self._subtest = parent

    def _addExpectedFailure(self, result, exc_info):
        try:
            add = result.addExpectedFailure
        except AttributeError:
            warnings.warn('TestResult has no addExpectedFailure method, '
                          'reporting as passes', RuntimeWarning)
            result.addSuccess(self)
        else:
            add(self, exc_info)

    def _addUnexpectedSuccess(self, result):
        try:
            add = result.addUnexpectedSuccess
        except AttributeError:
            warnings.warn('TestResult has no addUnexpectedSuccess method, '
                          'reporting as failure', RuntimeWarning)
            # a failure needs exception info: manufacture some
            try:
                raise _UnexpectedSuccess from None
            except _UnexpectedSuccess:
                result.addFailure(self, sys.exc_info())
        else:
            add(self)

    def _addDuration(self, result, elapsed):
        try:
            add = result.addDuration
        except AttributeError:
            warnings.warn('TestResult has no addDuration method',
                          RuntimeWarning)
        else:
            add(self, elapsed)

    def _callSetUp(self):
        self.setUp()

    def _callTestMethod(self, method):
        returned = method()
        if returned is not None:
            text = ('It is deprecated to return a value that is not None '
                    f'from a test case ({method} returned '
                    f'{type(returned).__name__!r})')
            if inspect_iscoroutine(returned):
                text += ('. Maybe you forgot to use IsolatedAsyncioTestCase '
                         'as the base class?')
            warnings.warn(text, DeprecationWarning, stacklevel=3)

    def _callTearDown(self):
        self.tearDown()

    def _callCleanup(self, function, /, *args, **kwargs):
        function(*args, **kwargs)

    def _skip_reason(self, method):
        if (getattr(self.__class__, '__unittest_skip__', False) or
                getattr(method, '__unittest_skip__', False)):
            return (getattr(self.__class__, '__unittest_skip_why__', '') or
                    getattr(method, '__unittest_skip_why__', ''))
        return None

    def run(self, result=None):
        """Run the test, reporting to *result* (a fresh default result,
        with startTestRun/stopTestRun, if none is given)."""
        own_result = result is None
        if own_result:
            result = self.defaultTestResult()
            start_run = getattr(result, 'startTestRun', None)
            if start_run is not None:
                start_run()
        result.startTest(self)
        try:
            method = getattr(self, self._testMethodName)
            reason = self._skip_reason(method)
            if reason is not None:
                _report_skip(result, self, reason)
                return result

            expecting_failure = (
                getattr(self, '__unittest_expecting_failure__', False) or
                getattr(method, '__unittest_expecting_failure__', False))
            outcome = _Outcome(result)
            started = time.perf_counter()
            try:
                self._outcome = outcome
                with outcome.testPartExecutor(self):
                    self._callSetUp()
                if outcome.success:
                    outcome.expecting_failure = expecting_failure
                    with outcome.testPartExecutor(self):
                        self._callTestMethod(method)
                    outcome.expecting_failure = False
                    with outcome.testPartExecutor(self):
                        self._callTearDown()
                self.doCleanups()
                self._addDuration(result, time.perf_counter() - started)

                if outcome.success:
                    if not expecting_failure:
                        result.addSuccess(self)
                    elif outcome.expectedFailure:
                        self._addExpectedFailure(result,
                                                 outcome.expectedFailure)
                    else:
                        self._addUnexpectedSuccess(result)
                return result
            finally:
                # drop references to frames/exceptions
                outcome.expectedFailure = None
                outcome = None
                self._outcome = None
        finally:
            result.stopTest(self)
            if own_result:
                stop_run = getattr(result, 'stopTestRun', None)
                if stop_run is not None:
                    stop_run()

    def doCleanups(self):
        """Run the registered cleanups; called by run() after tearDown."""
        outcome = self._outcome or _Outcome()
        while self._cleanups:
            function, args, kwargs = self._cleanups.pop()
            with outcome.testPartExecutor(self):
                self._callCleanup(function, *args, **kwargs)
        return outcome.success

    @classmethod
    def doClassCleanups(cls):
        """Run the class cleanups; errors are stored in
        cls.tearDown_exceptions."""
        cls.tearDown_exceptions = []
        while cls._class_cleanups:
            function, args, kwargs = cls._class_cleanups.pop()
            try:
                function(*args, **kwargs)
            except Exception:
                cls.tearDown_exceptions.append(sys.exc_info())

    def __call__(self, *args, **kwds):
        return self.run(*args, **kwds)

    def debug(self):
        """Run the test without a result object: exceptions propagate."""
        method = getattr(self, self._testMethodName)
        reason = self._skip_reason(method)
        if reason is not None:
            raise SkipTest(reason)
        self._callSetUp()
        self._callTestMethod(method)
        self._callTearDown()
        while self._cleanups:
            function, args, kwargs = self._cleanups.pop()
            self._callCleanup(function, *args, **kwargs)

    def skipTest(self, reason):
        """Skip this test."""
        raise SkipTest(reason)

    # -- assertions ------------------------------------------------------------------

    def fail(self, msg=None):
        """Fail immediately with the given message."""
        raise self.failureException(msg)

    def _formatMessage(self, msg, standardMsg):
        """Combine the standard failure message with a user message.

        With longMessage the user message is appended (' : '), otherwise it
        replaces the standard one."""
        if not self.longMessage:
            return msg or standardMsg
        if msg is None:
            return standardMsg
        try:
            return '%s : %s' % (standardMsg, msg)
        except UnicodeDecodeError:
            return '%s : %s' % (safe_repr(standardMsg), safe_repr(msg))

    def _fail_with(self, msg, standardMsg):
        raise self.failureException(self._formatMessage(msg, standardMsg))

    def assertFalse(self, expr, msg=None):
        """Check that the expression is false."""
        if expr:
            self._fail_with(msg, '%s is not false' % safe_repr(expr))

    def assertTrue(self, expr, msg=None):
        """Check that the expression is true."""
        if not expr:
            self._fail_with(msg, '%s is not true' % safe_repr(expr))

    def assertRaises(self, expected_exception, *args, **kwargs):
        """Fail unless an exception of class *expected_exception* is raised
        by the callable args[0] (called with the remaining arguments), or,
        when used as a context manager, inside the with block.

        The context manager keeps the caught exception in its 'exception'
        attribute."""
        context = _AssertRaisesContext(expected_exception, self)
        try:
            return context.handle('assertRaises', args, kwargs)
        finally:
            context = None

    def assertWarns(self, expected_warning, *args, **kwargs):
        """Fail unless a warning of class *expected_warning* is triggered;
        like assertRaises.  The context manager stores 'warning',
        'filename' and 'lineno' of the matching warning."""
        context = _AssertWarnsContext(expected_warning, self)
        return context.handle('assertWarns', args, kwargs)

    def assertLogs(self, logger=None, level=None, formatter=None):
        """Context manager failing unless at least one message of *level*
        (default INFO) or higher is logged on *logger* (default root) or
        its children.  The yielded object has 'records' and 'output'."""
        from ._log import _AssertLogsContext
        return _AssertLogsContext(self, logger, level, no_logs=False,
                                  formatter=formatter)

    def assertNoLogs(self, logger=None, level=None):
        """Context manager failing if any message of *level* (default
        INFO) or higher is logged on *logger* or its children."""
        from ._log import _AssertLogsContext
        return _AssertLogsContext(self, logger, level, no_logs=True)

    def _getAssertEqualityFunc(self, first, second):
        """The type-specific equality assertion for these arguments, if
        both have the same registered type; else _baseAssertEqual."""
        if type(first) == type(second):
            asserter = self._type_equality_funcs.get(type(first))
            if asserter is not None:
                if isinstance(asserter, str):
                    asserter = getattr(self, asserter)
                return asserter
        return self._baseAssertEqual

    def _baseAssertEqual(self, first, second, msg=None):
        """The default assertEqual implementation (no type specifics)."""
        if not first == second:
            self._fail_with(msg, '%s != %s' % _common_shorten_repr(first,
                                                                  second))

    def assertEqual(self, first, second, msg=None):
        """Fail unless first == second."""
        self._getAssertEqualityFunc(first, second)(first, second, msg=msg)

    def assertNotEqual(self, first, second, msg=None):
        """Fail if first == second."""
        if not first != second:
            self._fail_with(msg, '%s == %s' % (safe_repr(first),
                                               safe_repr(second)))

    def assertAlmostEqual(self, first, second, places=None, msg=None,
                          delta=None):
        """Fail unless first and second are equal after rounding their
        difference to *places* decimal places (default 7), or differ by at
        most *delta*."""
        if first == second:
            return
        if delta is not None and places is not None:
            raise TypeError('specify delta or places not both')
        diff = abs(first - second)
        if delta is not None:
            if diff <= delta:
                return
            standard = '%s != %s within %s delta (%s difference)' % (
                safe_repr(first), safe_repr(second), safe_repr(delta),
                safe_repr(diff))
        else:
            if places is None:
                places = 7
            if round(diff, places) == 0:
                return
            standard = '%s != %s within %r places (%s difference)' % (
                safe_repr(first), safe_repr(second), places, safe_repr(diff))
        self._fail_with(msg, standard)

    def assertNotAlmostEqual(self, first, second, places=None, msg=None,
                             delta=None):
        """Fail if first and second are equal after rounding their
        difference to *places* (default 7), or differ by at most *delta*."""
        if delta is not None and places is not None:
            raise TypeError('specify delta or places not both')
        diff = abs(first - second)
        if delta is not None:
            if not (first == second) and diff > delta:
                return
            standard = '%s == %s within %s delta (%s difference)' % (
                safe_repr(first), safe_repr(second), safe_repr(delta),
                safe_repr(diff))
        else:
            if places is None:
                places = 7
            if not (first == second) and round(diff, places) != 0:
                return
            standard = '%s == %s within %r places' % (
                safe_repr(first), safe_repr(second), places)
        self._fail_with(msg, standard)

    def _sequence_difference(self, seq1, seq2, seq_type_name, seq_type):
        """Describe how two sequences differ; None if they are equal."""
        try:
            len1 = len(seq1)
        except (TypeError, NotImplementedError):
            return 'First %s has no length.    Non-sequence?' % seq_type_name
        try:
            len2 = len(seq2)
        except (TypeError, NotImplementedError):
            return 'Second %s has no length.    Non-sequence?' % seq_type_name

        if seq1 == seq2:
            return None
        text = '%ss differ: %s != %s\n' % (
            (seq_type_name.capitalize(),) + _common_shorten_repr(seq1, seq2))

        found_difference = False
        for index in range(min(len1, len2)):
            try:
                item1 = seq1[index]
            except (TypeError, IndexError, NotImplementedError):
                text += ('\nUnable to index element %d of first %s\n' %
                         (index, seq_type_name))
                found_difference = True
                break
            try:
                item2 = seq2[index]
            except (TypeError, IndexError, NotImplementedError):
                text += ('\nUnable to index element %d of second %s\n' %
                         (index, seq_type_name))
                found_difference = True
                break
            if item1 != item2:
                text += ('\nFirst differing element %d:\n%s\n%s\n' %
                         ((index,) + _common_shorten_repr(item1, item2)))
                found_difference = True
                break
        if (not found_difference and len1 == len2 and seq_type is None
                and type(seq1) != type(seq2)):
            # same items, different sequence types: that is fine
            return None

        if len1 > len2:
            text += ('\nFirst %s contains %d additional elements.\n' %
                     (seq_type_name, len1 - len2))
            try:
                text += ('First extra element %d:\n%s\n' %
                         (len2, safe_repr(seq1[len2])))
            except (TypeError, IndexError, NotImplementedError):
                text += ('Unable to index element %d of first %s\n' %
                         (len2, seq_type_name))
        elif len1 < len2:
            text += ('\nSecond %s contains %d additional elements.\n' %
                     (seq_type_name, len2 - len1))
            try:
                text += ('First extra element %d:\n%s\n' %
                         (len1, safe_repr(seq2[len1])))
            except (TypeError, IndexError, NotImplementedError):
                text += ('Unable to index element %d of second %s\n' %
                         (len1, seq_type_name))
        return text

    def assertSequenceEqual(self, seq1, seq2, msg=None, seq_type=None):
        """Fail unless the two sequences have equal items.

        With *seq_type* both arguments must be instances of it.  The failure
        message describes the first difference and includes a diff."""
        if seq_type is not None:
            type_name = seq_type.__name__
            if not isinstance(seq1, seq_type):
                raise self.failureException('First sequence is not a %s: %s'
                                            % (type_name, safe_repr(seq1)))
            if not isinstance(seq2, seq_type):
                raise self.failureException('Second sequence is not a %s: %s'
                                            % (type_name, safe_repr(seq2)))
        else:
            type_name = 'sequence'

        difference = self._sequence_difference(seq1, seq2, type_name,
                                               seq_type)
        if difference is None:
            return
        diff = '\n' + '\n'.join(difflib.ndiff(
            pprint.pformat(seq1).splitlines(),
            pprint.pformat(seq2).splitlines()))
        standard = self._truncateMessage(difference, diff)
        self.fail(self._formatMessage(msg, standard))

    def _truncateMessage(self, message, diff):
        limit = self.maxDiff
        if limit is None or len(diff) <= limit:
            return message + diff
        return message + (DIFF_OMITTED % len(diff))

    def assertListEqual(self, list1, list2, msg=None):
        """assertSequenceEqual() for lists."""
        self.assertSequenceEqual(list1, list2, msg, seq_type=list)

    def assertTupleEqual(self, tuple1, tuple2, msg=None):
        """assertSequenceEqual() for tuples."""
        self.assertSequenceEqual(tuple1, tuple2, msg, seq_type=tuple)

    def assertSetEqual(self, set1, set2, msg=None):
        """Fail unless the two sets (anything with a difference() method)
        have the same members; the message lists the differences."""
        try:
            only_first = set1.difference(set2)
        except TypeError as exc:
            self.fail('invalid type when attempting set difference: %s' % exc)
        except AttributeError as exc:
            self.fail('first argument does not support set difference: %s'
                      % exc)
        try:
            only_second = set2.difference(set1)
        except TypeError as exc:
            self.fail('invalid type when attempting set difference: %s' % exc)
        except AttributeError as exc:
            self.fail('second argument does not support set difference: %s'
                      % exc)
        if not (only_first or only_second):
            return
        lines = []
        if only_first:
            lines.append('Items in the first set but not the second:')
            lines.extend(repr(item) for item in only_first)
        if only_second:
            lines.append('Items in the second set but not the first:')
            lines.extend(repr(item) for item in only_second)
        self.fail(self._formatMessage(msg, '\n'.join(lines)))

    def assertIn(self, member, container, msg=None):
        """Just like self.assertTrue(a in b), but with a nicer message."""
        if member not in container:
            self._fail_with(msg, '%s not found in %s' % (
                safe_repr(member), safe_repr(container)))

    def assertNotIn(self, member, container, msg=None):
        """Just like self.assertTrue(a not in b), but with a nicer
        message."""
        if member in container:
            self._fail_with(msg, '%s unexpectedly found in %s' % (
                safe_repr(member), safe_repr(container)))

    def assertIs(self, expr1, expr2, msg=None):
        """Just like self.assertTrue(a is b), but with a nicer message."""
        if expr1 is not expr2:
            self._fail_with(msg, '%s is not %s' % (safe_repr(expr1),
                                                   safe_repr(expr2)))

    def assertIsNot(self, expr1, expr2, msg=None):
        """Just like self.assertTrue(a is not b), but with a nicer
        message."""
        if expr1 is expr2:
            self._fail_with(msg, 'unexpectedly identical: %s' %
                            safe_repr(expr1))

    def assertDictEqual(self, d1, d2, msg=None):
        """Fail unless the two dicts are equal (with a diff on failure)."""
        self.assertIsInstance(d1, dict, 'First argument is not a dictionary')
        self.assertIsInstance(d2, dict, 'Second argument is not a dictionary')
        if d1 != d2:
            standard = '%s != %s' % _common_shorten_repr(d1, d2)
            diff = '\n' + '\n'.join(difflib.ndiff(
                pprint.pformat(d1).splitlines(),
                pprint.pformat(d2).splitlines()))
            standard = self._truncateMessage(standard, diff)
            self.fail(self._formatMessage(msg, standard))

    def assertCountEqual(self, first, second, msg=None):
        """Fail unless both iterables contain the same elements the same
        number of times, regardless of order."""
        first_items, second_items = list(first), list(second)
        try:
            counts1 = collections.Counter(first_items)
            counts2 = collections.Counter(second_items)
        except TypeError:
            # unhashable elements
            differences = _count_diff_all_purpose(first_items, second_items)
        else:
            if counts1 == counts2:
                return
            differences = _count_diff_hashable(first_items, second_items)
        if differences:
            lines = ['First has %d, Second has %d:  %r' % tuple(d)
                     for d in differences]
            standard = self._truncateMessage(
                'Element counts were not equal:\n', '\n'.join(lines))
            self.fail(self._formatMessage(msg, standard))

    def assertMultiLineEqual(self, first, second, msg=None):
        """Fail unless the two strings are equal; shows a line diff."""
        self.assertIsInstance(first, str, 'First argument is not a string')
        self.assertIsInstance(second, str, 'Second argument is not a string')
        if first == second:
            return
        if (len(first) > self._diffThreshold or
                len(second) > self._diffThreshold):
            # too big for difflib
            self._baseAssertEqual(first, second, msg)
        # Make both end in a newline (unless empty) so that a difference in
        # the final newline shows up in the diff.
        text1, text2 = first, second
        if first and second:
            if not first.endswith('\n') or not second.endswith('\n'):
                text1 += '\n'
                text2 += '\n'
        elif second and not second.endswith('\n'):
            text2 += '\n'
        elif first and not first.endswith('\n'):
            text1 += '\n'
        diff = '\n' + ''.join(difflib.ndiff(text1.splitlines(keepends=True),
                                            text2.splitlines(keepends=True)))
        standard = '%s != %s' % _common_shorten_repr(first, second)
        standard = self._truncateMessage(standard, diff)
        self.fail(self._formatMessage(msg, standard))

    def assertLess(self, a, b, msg=None):
        """Just like self.assertTrue(a < b), but with a nicer message."""
        if not a < b:
            self._fail_with(msg, '%s not less than %s' % (safe_repr(a),
                                                          safe_repr(b)))

    def assertLessEqual(self, a, b, msg=None):
        """Just like self.assertTrue(a <= b), but with a nicer message."""
        if not a <= b:
            self._fail_with(msg, '%s not less than or equal to %s' % (
                safe_repr(a), safe_repr(b)))

    def assertGreater(self, a, b, msg=None):
        """Just like self.assertTrue(a > b), but with a nicer message."""
        if not a > b:
            self._fail_with(msg, '%s not greater than %s' % (safe_repr(a),
                                                             safe_repr(b)))

    def assertGreaterEqual(self, a, b, msg=None):
        """Just like self.assertTrue(a >= b), but with a nicer message."""
        if not a >= b:
            self._fail_with(msg, '%s not greater than or equal to %s' % (
                safe_repr(a), safe_repr(b)))

    def assertIsNone(self, obj, msg=None):
        """Same as self.assertTrue(obj is None), with a nicer message."""
        if obj is not None:
            self._fail_with(msg, '%s is not None' % (safe_repr(obj),))

    def assertIsNotNone(self, obj, msg=None):
        """Included for symmetry with assertIsNone."""
        if obj is None:
            self._fail_with(msg, 'unexpectedly None')

    def assertIsInstance(self, obj, cls, msg=None):
        """Same as self.assertTrue(isinstance(obj, cls)), with a nicer
        message."""
        if isinstance(obj, cls):
            return
        if isinstance(cls, tuple):
            standard = f'{safe_repr(obj)} is not an instance of any of {cls!r}'
        else:
            standard = f'{safe_repr(obj)} is not an instance of {cls!r}'
        self._fail_with(msg, standard)

    def assertNotIsInstance(self, obj, cls, msg=None):
        """Included for symmetry with assertIsInstance."""
        if not isinstance(obj, cls):
            return
        if isinstance(cls, tuple):
            cls = next(c for c in cls if isinstance(obj, c))
        self._fail_with(msg, f'{safe_repr(obj)} is an instance of {cls!r}')

    def assertIsSubclass(self, cls, superclass, msg=None):
        """Fail unless *cls* is a subclass of *superclass* (or of one of
        a tuple of classes)."""
        try:
            if issubclass(cls, superclass):
                return
        except TypeError:
            if not isinstance(cls, type):
                self._fail_with(msg, f'{cls!r} is not a class')
            raise
        if isinstance(superclass, tuple):
            standard = f'{cls!r} is not a subclass of any of {superclass!r}'
        else:
            standard = f'{cls!r} is not a subclass of {superclass!r}'
        self._fail_with(msg, standard)

    def assertNotIsSubclass(self, cls, superclass, msg=None):
        """Fail if *cls* is a subclass of *superclass* (or of one of a
        tuple of classes)."""
        try:
            if not issubclass(cls, superclass):
                return
        except TypeError:
            if not isinstance(cls, type):
                self._fail_with(msg, f'{cls!r} is not a class')
            raise
        if isinstance(superclass, tuple):
            superclass = next(c for c in superclass if issubclass(cls, c))
        self._fail_with(msg, f'{cls!r} is a subclass of {superclass!r}')

    @staticmethod
    def _describe_owner(obj):
        if isinstance(obj, types.ModuleType):
            return f'module {obj.__name__!r}'
        if isinstance(obj, type):
            return f'type object {obj.__name__!r}'
        return f'{type(obj).__name__!r} object'

    def assertHasAttr(self, obj, name, msg=None):
        """Fail unless *obj* has an attribute *name*."""
        if not hasattr(obj, name):
            self._fail_with(msg, f'{self._describe_owner(obj)} has no '
                                 f'attribute {name!r}')

    def assertNotHasAttr(self, obj, name, msg=None):
        """Fail if *obj* has an attribute *name*."""
        if hasattr(obj, name):
            self._fail_with(msg, f'{self._describe_owner(obj)} has '
                                 f'unexpected attribute {name!r}')

    def assertRaisesRegex(self, expected_exception, expected_regex,
                          *args, **kwargs):
        """Like assertRaises, and the string form of the exception must
        match the regular expression *expected_regex* (re.search)."""
        context = _AssertRaisesContext(expected_exception, self,
                                       expected_regex)
        return context.handle('assertRaisesRegex', args, kwargs)

    def assertWarnsRegex(self, expected_warning, expected_regex,
                         *args, **kwargs):
        """Like assertWarns, and the message of a triggered warning must
        match the regular expression *expected_regex* (re.search)."""
        context = _AssertWarnsContext(expected_warning, self, expected_regex)
        return context.handle('assertWarnsRegex', args, kwargs)

    def assertRegex(self, text, expected_regex, msg=None):
        """Fail unless *text* matches the regular expression."""
        if isinstance(expected_regex, (str, bytes)):
            assert expected_regex, 'expected_regex must not be empty.'
            expected_regex = re.compile(expected_regex)
        if not expected_regex.search(text):
            self._fail_with(msg, "Regex didn't match: %r not found in %r" % (
                expected_regex.pattern, text))

    def assertNotRegex(self, text, unexpected_regex, msg=None):
        """Fail if *text* matches the regular expression."""
        if isinstance(unexpected_regex, (str, bytes)):
            unexpected_regex = re.compile(unexpected_regex)
        found = unexpected_regex.search(text)
        if found:
            self._fail_with(msg, 'Regex matched: %r matches %r in %r' % (
                text[found.start():found.end()], unexpected_regex.pattern,
                text))

    # -- startswith / endswith -------------------------------------------------------------

    def _affix_check(self, s, affix, method_name):
        """Return (matched, matching_affix) for s.startswith/endswith,
        turning type mismatches into failures."""
        try:
            matched = getattr(s, method_name)(affix)
        except (AttributeError, TypeError):
            sample = affix[0] if isinstance(affix, tuple) and affix else affix
            if isinstance(sample, str):
                wanted, kinds = 'str', (str,)
            elif isinstance(sample, (bytes, bytearray)):
                wanted, kinds = 'bytes', (bytes, bytearray)
            else:
                raise
            if not isinstance(s, kinds):
                self.fail(f'Expected {wanted}, not {type(s).__name__}')
            raise
        if matched and isinstance(affix, tuple):
            for candidate in affix:
                if getattr(s, method_name)(candidate):
                    return True, candidate
        return matched, affix

    @staticmethod
    def _affix_text(affix):
        if isinstance(affix, tuple):
            return f'any of {affix!r}'
        return repr(affix)

    def assertStartsWith(self, s, prefix, msg=None):
        """Fail unless *s* starts with *prefix* (or one of a tuple)."""
        matched, _ = self._affix_check(s, prefix, 'startswith')
        if not matched:
            self._fail_with(msg, f"{safe_repr(s, short=True)} doesn't start "
                                 f"with {self._affix_text(prefix)}")

    def assertNotStartsWith(self, s, prefix, msg=None):
        """Fail if *s* starts with *prefix* (or one of a tuple)."""
        matched, which = self._affix_check(s, prefix, 'startswith')
        if matched:
            self._fail_with(msg, f'{safe_repr(s, short=True)} starts with '
                                 f'{which!r}')

    def assertEndsWith(self, s, suffix, msg=None):
        """Fail unless *s* ends with *suffix* (or one of a tuple)."""
        matched, _ = self._affix_check(s, suffix, 'endswith')
        if not matched:
            self._fail_with(msg, f"{safe_repr(s, short=True)} doesn't end "
                                 f"with {self._affix_text(suffix)}")

    def assertNotEndsWith(self, s, suffix, msg=None):
        """Fail if *s* ends with *suffix* (or one of a tuple)."""
        matched, which = self._affix_check(s, suffix, 'endswith')
        if matched:
            self._fail_with(msg, f'{safe_repr(s, short=True)} ends with '
                                 f'{which!r}')


def inspect_iscoroutine(obj):
    return isinstance(obj, types.CoroutineType)


class FunctionTestCase(TestCase):
    """Adapts a plain test function (plus optional set-up and tear-down
    functions) to the TestCase interface."""

    def __init__(self, testFunc, setUp=None, tearDown=None,
                 description=None):
        super(FunctionTestCase, self).__init__()
        self._setUpFunc = setUp
        self._tearDownFunc = tearDown
        self._testFunc = testFunc
        self._description = description

    def setUp(self):
        if self._setUpFunc is not None:
            self._setUpFunc()

    def tearDown(self):
        if self._tearDownFunc is not None:
            self._tearDownFunc()

    def runTest(self):
        self._testFunc()

    def id(self):
        return self._testFunc.__name__

    def _key(self):
        return (self._setUpFunc, self._tearDownFunc, self._testFunc,
                self._description)

    def __eq__(self, other):
        if not isinstance(other, self.__class__):
            return NotImplemented
        return self._key() == other._key()

    def __hash__(self):
        return hash((type(self),) + self._key())

    def __str__(self):
        return '%s (%s)' % (strclass(self.__class__), self._testFunc.__name__)

    def __repr__(self):
        return '<%s tec=%s>' % (strclass(self.__class__), self._testFunc)

    def shortDescription(self):
        if self._description is not None:
            return self._description
        doc = self._testFunc.__doc__
        return doc and doc.split('\n')[0].strip() or None


class _SubTest(TestCase):
    """The pseudo test case reported for a subTest() block."""

    def __init__(self, test_case, message, params):
        super().__init__()
        self._message = message
        self.test_case = test_case
        self.params = params
        self.failureException = test_case.failureException

    def runTest(self):
        raise NotImplementedError('subtests cannot be run directly')

    def _subDescription(self):
        parts = []
        if self._message is not _subtest_msg_sentinel:
            parts.append('[{}]'.format(self._message))
        if self.params:
            shown = ', '.join('{}={!r}'.format(key, value)
                              for key, value in self.params.items())
            parts.append('({})'.format(shown))
        return ' '.join(parts) or '(<subtest>)'

    def id(self):
        return '{} {}'.format(self.test_case.id(), self._subDescription())

    def shortDescription(self):
        """The description of the enclosing test."""
        return self.test_case.shortDescription()

    def __str__(self):
        return '{} {}'.format(self.test_case, self._subDescription())
