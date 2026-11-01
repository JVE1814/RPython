"""TestSuite: an ordered collection of tests, with class/module fixtures."""

import sys

from . import case
from . import util

__unittest = True


def _call_if_exists(parent, attr):
    hook = getattr(parent, attr, None)
    if hook is not None:
        hook()


class BaseTestSuite(object):
    """A simple test suite: runs its tests in order, no fixtures."""

    # Drop references to tests once they ran (lets them be freed early).
    _cleanup = True

    def __init__(self, tests=()):
        self._tests = []
        self._removed_tests = 0
        self.addTests(tests)

    def __repr__(self):
        return '<%s tests=%s>' % (util.strclass(self.__class__), list(self))

    def __eq__(self, other):
        if not isinstance(other, self.__class__):
            return NotImplemented
        return list(self) == list(other)

    def __iter__(self):
        return iter(self._tests)

    def countTestCases(self):
        total = self._removed_tests
        for test in self:
            if test:
                total += test.countTestCases()
        return total

    def addTest(self, test):
        if not callable(test):
            raise TypeError('{} is not callable'.format(repr(test)))
        if isinstance(test, type) and issubclass(test, (case.TestCase,
                                                        TestSuite)):
            raise TypeError('TestCases and TestSuites must be instantiated '
                            'before passing them to addTest()')
        self._tests.append(test)

    def addTests(self, tests):
        if isinstance(tests, str):
            raise TypeError('tests must be an iterable of tests, not a string')
        for test in tests:
            self.addTest(test)

    def run(self, result):
        for index, test in enumerate(self):
            if result.shouldStop:
                break
            test(result)
            if self._cleanup:
                self._removeTestAtIndex(index)
        return result

    def _removeTestAtIndex(self, index):
        """Forget the test at *index*, remembering how many cases it had."""
        try:
            test = self._tests[index]
        except TypeError:
            # _tests is not indexable (a subclass may use anything)
            return
        if hasattr(test, 'countTestCases'):
            self._removed_tests += test.countTestCases()
        self._tests[index] = None

    def __call__(self, *args, **kwds):
        return self.run(*args, **kwds)

    def debug(self):
        """Run the tests without collecting errors in a TestResult"""
        for test in self:
            test.debug()


class TestSuite(BaseTestSuite):
    """A test suite that also runs class and module level fixtures
    (setUpClass/tearDownClass, setUpModule/tearDownModule and their
    cleanups) around the test cases it contains, at any nesting depth.
    """

    def run(self, result, debug=False):
        outermost = False
        if getattr(result, '_testRunEntered', False) is False:
            result._testRunEntered = outermost = True

        for index, test in enumerate(self):
            if result.shouldStop:
                break
            if _isnotsuite(test):
                self._leave_previous_class(test, result)
                self._enter_module(test, result)
                self._enter_class(test, result)
                result._previousTestClass = test.__class__
                if (getattr(test.__class__, '_classSetupFailed', False) or
                        getattr(result, '_moduleSetUpFailed', False)):
                    continue
            if debug:
                test.debug()
            else:
                test(result)
            if self._cleanup:
                self._removeTestAtIndex(index)

        if outermost:
            self._leave_previous_class(None, result)
            self._leave_module(result)
            result._testRunEntered = False
        return result

    def debug(self):
        """Run the tests without collecting errors in a TestResult"""
        self.run(_DebugResult(), True)

    # -- reporting fixture errors ------------------------------------------------

    def _report_fixture_error(self, result, exc, method_name, owner,
                              info=None):
        holder = _ErrorHolder(f'{method_name} ({owner})')
        add_skip = getattr(result, 'addSkip', None)
        if add_skip is not None and isinstance(exc, case.SkipTest):
            add_skip(holder, str(exc))
        elif info:
            result.addError(holder, info)
        else:
            result.addError(holder, sys.exc_info())

    def _run_module_cleanups(self, result, method_name, module_name):
        try:
            case.doModuleCleanups()
        except ExceptionGroup as group:
            if isinstance(result, _DebugResult):
                raise
            for exc in group.exceptions:
                info = (type(exc), exc, exc.__traceback__)
                self._report_fixture_error(result, exc, method_name,
                                           module_name, info=info)
        except Exception as exc:
            if isinstance(result, _DebugResult):
                raise
            self._report_fixture_error(result, exc, method_name, module_name)

    # -- class fixtures -------------------------------------------------------------

    def _enter_class(self, test, result):
        previous = getattr(result, '_previousTestClass', None)
        current = test.__class__
        if current == previous:
            return
        if result._moduleSetUpFailed:
            return
        if getattr(current, '__unittest_skip__', False):
            return

        try:
            current._classSetupFailed = False
        except TypeError:
            # the "test" may be a plain function: builtin class
            pass

        set_up = getattr(current, 'setUpClass', None)
        cleanups = getattr(current, 'doClassCleanups', None)
        if set_up is None:
            return
        _call_if_exists(result, '_setupStdout')
        try:
            failed = False
            class_name = util.strclass(current)
            try:
                set_up()
            except Exception as exc:
                if isinstance(result, _DebugResult):
                    raise
                failed = True
                try:
                    current._classSetupFailed = True
                except TypeError:
                    pass
                self._report_fixture_error(result, exc, 'setUpClass',
                                           class_name)
            if failed and cleanups is not None:
                cleanups()
                for info in current.tearDown_exceptions:
                    self._report_fixture_error(result, info[1], 'setUpClass',
                                               class_name, info=info)
        finally:
            _call_if_exists(result, '_restoreStdout')

    def _leave_previous_class(self, test, result):
        previous = getattr(result, '_previousTestClass', None)
        current = test.__class__
        if current == previous or previous is None:
            return
        if getattr(previous, '_classSetupFailed', False):
            return
        if getattr(result, '_moduleSetUpFailed', False):
            return
        if getattr(previous, '__unittest_skip__', False):
            return

        tear_down = getattr(previous, 'tearDownClass', None)
        cleanups = getattr(previous, 'doClassCleanups', None)
        if tear_down is None and cleanups is None:
            return
        class_name = util.strclass(previous)
        _call_if_exists(result, '_setupStdout')
        try:
            if tear_down is not None:
                try:
                    tear_down()
                except Exception as exc:
                    if isinstance(result, _DebugResult):
                        raise
                    self._report_fixture_error(result, exc, 'tearDownClass',
                                               class_name)
            if cleanups is not None:
                cleanups()
                for info in previous.tearDown_exceptions:
                    if isinstance(result, _DebugResult):
                        raise info[1]
                    self._report_fixture_error(result, info[1],
                                               'tearDownClass', class_name,
                                               info=info)
        finally:
            _call_if_exists(result, '_restoreStdout')

    # -- module fixtures ---------------------------------------------------------------

    @staticmethod
    def _previous_module(result):
        previous = getattr(result, '_previousTestClass', None)
        if previous is None:
            return None
        return previous.__module__

    def _enter_module(self, test, result):
        current = test.__class__.__module__
        if current == self._previous_module(result):
            return
        self._leave_module(result)
        result._moduleSetUpFailed = False
        module = sys.modules.get(current)
        if module is None:
            return
        set_up = getattr(module, 'setUpModule', None)
        if set_up is None:
            return
        _call_if_exists(result, '_setupStdout')
        try:
            try:
                set_up()
            except Exception as exc:
                if isinstance(result, _DebugResult):
                    raise
                result._moduleSetUpFailed = True
                self._report_fixture_error(result, exc, 'setUpModule',
                                           current)
            if result._moduleSetUpFailed:
                self._run_module_cleanups(result, 'setUpModule', current)
        finally:
            _call_if_exists(result, '_restoreStdout')

    def _leave_module(self, result):
        previous = self._previous_module(result)
        if previous is None:
            return
        if result._moduleSetUpFailed:
            return
        module = sys.modules.get(previous)
        if module is None:
            return
        _call_if_exists(result, '_setupStdout')
        try:
            tear_down = getattr(module, 'tearDownModule', None)
            if tear_down is not None:
                try:
                    tear_down()
                except Exception as exc:
                    if isinstance(result, _DebugResult):
                        raise
                    self._report_fixture_error(result, exc,
                                               'tearDownModule', previous)
            self._run_module_cleanups(result, 'tearDownModule', previous)
        finally:
            _call_if_exists(result, '_restoreStdout')


class _ErrorHolder(object):
    """Stands in for a test when a fixture (setUpClass, setUpModule, ...)
    fails, so that the error can be reported like a test error."""

    # Only used for reporting; an error is never a "failure".
    failureException = None

    def __init__(self, description):
        self.description = description

    def id(self):
        return self.description

    def shortDescription(self):
        return None

    def __repr__(self):
        return '<ErrorHolder description=%r>' % (self.description,)

    def __str__(self):
        return self.id()

    def run(self, result):
        # nothing to run: the error has been reported already
        pass

    def __call__(self, result):
        return self.run(result)

    def countTestCases(self):
        return 0


def _isnotsuite(test):
    """A test case is anything that is not iterable."""
    try:
        iter(test)
    except TypeError:
        return True
    return False


class _DebugResult(object):
    """Minimal result used by TestSuite.debug(): exceptions propagate."""

    _previousTestClass = None
    _moduleSetUpFailed = False
    shouldStop = False
