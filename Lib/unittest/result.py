"""TestResult: collects the outcome of running tests."""

import io
import sys
import traceback

from . import util
from functools import wraps

__unittest = True

STDOUT_LINE = '\nStdout:\n%s'
STDERR_LINE = '\nStderr:\n%s'


def failfast(method):
    """Decorator: stop the run after this kind of event if failfast is set."""
    @wraps(method)
    def inner(self, *args, **kw):
        if getattr(self, 'failfast', False):
            self.stop()
        return method(self, *args, **kw)
    return inner


def _with_newline(text):
    return text if text.endswith('\n') else text + '\n'


class TestResult(object):
    """Holder for test result information.

    The lists ``failures`` and ``errors`` contain (test, formatted
    traceback) pairs, ``skipped`` (test, reason) pairs,
    ``expectedFailures`` (test, formatted traceback) pairs and
    ``unexpectedSuccesses`` tests; ``testsRun`` counts started tests.

    Note: the methods are written so that they also work when copied into
    a class that does not inherit from TestResult.
    """

    _previousTestClass = None
    _testRunEntered = False
    _moduleSetUpFailed = False

    def __init__(self, stream=None, descriptions=None, verbosity=None):
        self.failfast = False
        self.failures = []
        self.errors = []
        self.testsRun = 0
        self.skipped = []
        self.expectedFailures = []
        self.unexpectedSuccesses = []
        self.collectedDurations = []
        self.shouldStop = False
        self.buffer = False
        self.tb_locals = False
        self._stdout_buffer = None
        self._stderr_buffer = None
        self._original_stdout = sys.stdout
        self._original_stderr = sys.stderr
        self._mirrorOutput = False

    def printErrors(self):
        "Called by TestRunner after test run"

    def startTest(self, test):
        "Called when the given test is about to be run"
        self.testsRun += 1
        self._mirrorOutput = False
        self._setupStdout()

    def _setupStdout(self):
        if not self.buffer:
            return
        if self._stderr_buffer is None:
            self._stderr_buffer = io.StringIO()
            self._stdout_buffer = io.StringIO()
        sys.stdout = self._stdout_buffer
        sys.stderr = self._stderr_buffer

    def startTestRun(self):
        """Called once before any tests are executed.

        See startTest for a method called before each test.
        """

    def stopTest(self, test):
        """Called when the given test has been run"""
        self._restoreStdout()
        self._mirrorOutput = False

    def _restoreStdout(self):
        if not self.buffer:
            return
        if self._mirrorOutput:
            # the test failed: show what it printed
            captured_out = sys.stdout.getvalue()
            captured_err = sys.stderr.getvalue()
            if captured_out:
                self._original_stdout.write(
                    STDOUT_LINE % _with_newline(captured_out))
            if captured_err:
                self._original_stderr.write(
                    STDERR_LINE % _with_newline(captured_err))
        sys.stdout = self._original_stdout
        sys.stderr = self._original_stderr
        for buf in (self._stdout_buffer, self._stderr_buffer):
            buf.seek(0)
            buf.truncate()

    def stopTestRun(self):
        """Called once after all tests are executed.

        See stopTest for a method called after each test.
        """

    @failfast
    def addError(self, test, err):
        """Called when an error has occurred. 'err' is a tuple of values as
        returned by sys.exc_info().
        """
        self.errors.append((test, self._exc_info_to_string(err, test)))
        self._mirrorOutput = True

    @failfast
    def addFailure(self, test, err):
        """Called when an error has occurred. 'err' is a tuple of values as
        returned by sys.exc_info()."""
        self.failures.append((test, self._exc_info_to_string(err, test)))
        self._mirrorOutput = True

    def addSubTest(self, test, subtest, err):
        """Called at the end of a subtest.
        'err' is None if the subtest ended successfully, otherwise it's a
        tuple of values as returned by sys.exc_info().
        """
        if err is None:
            return
        if getattr(self, 'failfast', False):
            self.stop()
        if issubclass(err[0], test.failureException):
            target = self.failures
        else:
            target = self.errors
        target.append((subtest, self._exc_info_to_string(err, test)))
        self._mirrorOutput = True

    def addSuccess(self, test):
        "Called when a test has completed successfully"

    def addSkip(self, test, reason):
        """Called when a test is skipped."""
        self.skipped.append((test, reason))

    def addExpectedFailure(self, test, err):
        """Called when an expected failure/error occurred."""
        self.expectedFailures.append(
            (test, self._exc_info_to_string(err, test)))

    @failfast
    def addUnexpectedSuccess(self, test):
        """Called when a test was expected to fail, but succeed."""
        self.unexpectedSuccesses.append(test)

    def addDuration(self, test, elapsed):
        """Called when a test finished to run, regardless of its outcome.
        *test* is the test case corresponding to the test method.
        *elapsed* is the time represented in seconds, and it includes the
        execution of cleanup functions.
        """
        # older result subclasses may lack the list
        if hasattr(self, 'collectedDurations'):
            self.collectedDurations.append((str(test), elapsed))

    def wasSuccessful(self):
        """Tells whether or not this result was a success."""
        if self.failures or self.errors:
            return False
        return not getattr(self, 'unexpectedSuccesses', ())

    def stop(self):
        """Indicates that the tests should be aborted."""
        self.shouldStop = True

    def _exc_info_to_string(self, err, test):
        """Converts a sys.exc_info()-style tuple of values into a string."""
        exctype, value, tb = err
        tb = self._clean_tracebacks(exctype, value, tb, test)
        formatter = traceback.TracebackException(
            exctype, value, tb, capture_locals=self.tb_locals, compact=True)
        from _colorize import can_colorize
        colorize = hasattr(self, 'stream') and can_colorize(file=self.stream)
        pieces = list(formatter.format(colorize=colorize))

        if self.buffer:
            captured_out = sys.stdout.getvalue()
            captured_err = sys.stderr.getvalue()
            if captured_out:
                pieces.append(STDOUT_LINE % _with_newline(captured_out))
            if captured_err:
                pieces.append(STDERR_LINE % _with_newline(captured_err))
        return ''.join(pieces)

    def _clean_tracebacks(self, exctype, value, tb, test):
        """Drop unittest's own frames from the traceback of the exception
        and of its chained causes/contexts; returns the new top tb."""
        top = None
        first = True
        pending = [(exctype, value, tb)]
        seen = {id(value)}
        while pending:
            exctype, value, tb = pending.pop()
            # frames of the test runner at the top
            while tb and self._is_relevant_tb_level(tb):
                tb = tb.tb_next
            # frames of the failing assert*() method at the bottom
            if exctype is test.failureException:
                self._remove_unittest_tb_frames(tb)
            if first:
                top = tb
                first = False
            else:
                value.__traceback__ = tb
            if value is not None:
                for chained in (value.__cause__, value.__context__):
                    if chained is not None and id(chained) not in seen:
                        pending.append((type(chained), chained,
                                        chained.__traceback__))
                        seen.add(id(chained))
        return top

    def _is_relevant_tb_level(self, tb):
        return '__unittest' in tb.tb_frame.f_globals

    def _remove_unittest_tb_frames(self, tb):
        """Cut the traceback before the first unittest frame."""
        previous = None
        while tb and not self._is_relevant_tb_level(tb):
            previous = tb
            tb = tb.tb_next
        if previous is not None:
            previous.tb_next = None

    def __repr__(self):
        return ('<%s run=%i errors=%i failures=%i>' %
                (util.strclass(self.__class__), self.testsRun,
                 len(self.errors), len(self.failures)))
