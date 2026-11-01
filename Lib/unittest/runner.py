"""Text-based test runner and its result class."""

import sys
import time
import warnings

from _colorize import get_theme

from . import result
from .case import _SubTest
from .signals import registerResult

__unittest = True


class _WritelnDecorator(object):
    """Wraps a stream, adding writeln()."""

    def __init__(self, stream):
        self.stream = stream

    def __getattr__(self, attr):
        if attr in ('stream', '__getstate__'):
            # not delegated: keeps pickling/copying from recursing
            raise AttributeError(attr)
        return getattr(self.stream, attr)

    def writeln(self, arg=None):
        if arg:
            self.write(arg)
        self.write('\n')


class TextTestResult(result.TestResult):
    """A test result that reports progress to a stream: one character per
    test (verbosity 1) or one line per test (verbosity 2 and up).

    Used by TextTestRunner.
    """

    separator1 = '=' * 70
    separator2 = '-' * 70

    def __init__(self, stream, descriptions, verbosity, *, durations=None):
        """*stream* receives the progress output; *descriptions* adds the
        first docstring line to test names."""
        super(TextTestResult, self).__init__(stream, descriptions, verbosity)
        self.stream = stream
        self.showAll = verbosity > 1
        self.dots = verbosity == 1
        self.descriptions = descriptions
        self._theme = get_theme(tty_file=stream).unittest
        self._newline = True
        self.durations = durations

    def getDescription(self, test):
        first_line = test.shortDescription()
        if self.descriptions and first_line:
            return '\n'.join((str(test), first_line))
        return str(test)

    def startTest(self, test):
        super(TextTestResult, self).startTest(test)
        if self.showAll:
            self.stream.write(self.getDescription(test))
            self.stream.write(' ... ')
            self.stream.flush()
            self._newline = False

    def _write_status(self, test, status):
        # Subtest outcomes get their own indented line; a test whose line
        # has already been completed (e.g. by a subtest) is described again.
        subtest = isinstance(test, _SubTest)
        if subtest or self._newline:
            if not self._newline:
                self.stream.writeln()
            if subtest:
                self.stream.write('  ')
            self.stream.write(self.getDescription(test))
            self.stream.write(' ... ')
        self.stream.writeln(status)
        self.stream.flush()
        self._newline = True

    def _report(self, test, word, letter, color):
        theme = self._theme
        if self.showAll:
            self._write_status(test, f'{color}{word}{theme.reset}')
        elif self.dots:
            self.stream.write(f'{color}{letter}{theme.reset}')
            self.stream.flush()

    def addSubTest(self, test, subtest, err):
        if err is not None:
            if issubclass(err[0], subtest.failureException):
                self._report(subtest, 'FAIL', 'F', self._theme.fail)
            else:
                self._report(subtest, 'ERROR', 'E', self._theme.fail)
        super(TextTestResult, self).addSubTest(test, subtest, err)

    def addSuccess(self, test):
        super(TextTestResult, self).addSuccess(test)
        self._report(test, 'ok', '.', self._theme.passed)

    def addError(self, test, err):
        super(TextTestResult, self).addError(test, err)
        self._report(test, 'ERROR', 'E', self._theme.fail)

    def addFailure(self, test, err):
        super(TextTestResult, self).addFailure(test, err)
        self._report(test, 'FAIL', 'F', self._theme.fail)

    def addSkip(self, test, reason):
        super(TextTestResult, self).addSkip(test, reason)
        theme = self._theme
        if self.showAll:
            self._write_status(test, f'{theme.warn}skipped{theme.reset} '
                                     f'{reason!r}')
        elif self.dots:
            self.stream.write(f'{theme.warn}s{theme.reset}')
            self.stream.flush()

    def addExpectedFailure(self, test, err):
        super(TextTestResult, self).addExpectedFailure(test, err)
        self._report(test, 'expected failure', 'x', self._theme.warn)

    def addUnexpectedSuccess(self, test):
        super(TextTestResult, self).addUnexpectedSuccess(test)
        self._report(test, 'unexpected success', 'u', self._theme.fail)

    def printErrors(self):
        theme = self._theme
        if self.dots or self.showAll:
            self.stream.writeln()
            self.stream.flush()
        self.printErrorList(f'{theme.fail}ERROR{theme.reset}', self.errors)
        self.printErrorList(f'{theme.fail}FAIL{theme.reset}', self.failures)
        unexpected = getattr(self, 'unexpectedSuccesses', ())
        if unexpected:
            self.stream.writeln(self.separator1)
            for test in unexpected:
                self.stream.writeln(
                    f'{theme.fail}UNEXPECTED SUCCESS{theme.fail_info}: '
                    f'{self.getDescription(test)}{theme.reset}')
            self.stream.flush()

    def printErrorList(self, flavour, errors):
        theme = self._theme
        for test, err in errors:
            self.stream.writeln(self.separator1)
            self.stream.writeln(f'{flavour}{theme.fail_info}: '
                                f'{self.getDescription(test)}{theme.reset}')
            self.stream.writeln(self.separator2)
            self.stream.writeln('%s' % err)
            self.stream.flush()


class TextTestRunner(object):
    """Runs tests and reports the results to a stream (default stderr):
    progress while running, then the errors and a summary."""

    resultclass = TextTestResult

    def __init__(self, stream=None, descriptions=True, verbosity=1,
                 failfast=False, buffer=False, resultclass=None,
                 warnings=None, *, tb_locals=False, durations=None):
        """The options are copied onto the result object at run time
        (failfast, buffer, tb_locals) or used for reporting."""
        if stream is None:
            stream = sys.stderr
        self.stream = _WritelnDecorator(stream)
        self.descriptions = descriptions
        self.verbosity = verbosity
        self.failfast = failfast
        self.buffer = buffer
        self.tb_locals = tb_locals
        self.durations = durations
        self.warnings = warnings
        if resultclass is not None:
            self.resultclass = resultclass

    def _makeResult(self):
        try:
            return self.resultclass(self.stream, self.descriptions,
                                    self.verbosity, durations=self.durations)
        except TypeError:
            # a result class that does not know about durations
            return self.resultclass(self.stream, self.descriptions,
                                    self.verbosity)

    def _printDurations(self, result):
        if not result.collectedDurations:
            return
        slowest = sorted(result.collectedDurations, key=lambda item: item[1],
                         reverse=True)
        if self.durations > 0:
            slowest = slowest[:self.durations]
        self.stream.writeln('Slowest test durations')
        if hasattr(result, 'separator2'):
            self.stream.writeln(result.separator2)
        hidden = False
        for test, elapsed in slowest:
            if self.verbosity < 2 and elapsed < 0.001:
                hidden = True
                continue
            self.stream.writeln('%-10s %s' % ('%.3fs' % elapsed, test))
        if hidden:
            self.stream.writeln('\n(durations < 0.001s were hidden; use -v '
                                'to show these durations)')
        else:
            self.stream.writeln('')

    def run(self, test):
        """Run *test* (a case or suite) and report on the stream."""
        result = self._makeResult()
        registerResult(result)
        result.failfast = self.failfast
        result.buffer = self.buffer
        result.tb_locals = self.tb_locals
        with warnings.catch_warnings():
            if self.warnings:
                # an explicit warnings action applies to everything
                warnings.simplefilter(self.warnings)
            started = time.perf_counter()
            start_run = getattr(result, 'startTestRun', None)
            if start_run is not None:
                start_run()
            try:
                test(result)
            finally:
                stop_run = getattr(result, 'stopTestRun', None)
                if stop_run is not None:
                    stop_run()
            elapsed = time.perf_counter() - started

        result.printErrors()
        if self.durations is not None:
            self._printDurations(result)
        if hasattr(result, 'separator2'):
            self.stream.writeln(result.separator2)
        run = result.testsRun
        self.stream.writeln('Ran %d test%s in %.3fs' %
                            (run, 's' if run != 1 else '', elapsed))
        self.stream.writeln()
        self._write_summary(result, run)
        self.stream.flush()
        return result

    def _write_summary(self, result, run):
        try:
            expected_fails = len(result.expectedFailures)
            unexpected = len(result.unexpectedSuccesses)
            skipped = len(result.skipped)
        except AttributeError:
            expected_fails = unexpected = skipped = 0
        theme = get_theme(tty_file=self.stream).unittest
        details = []
        if not result.wasSuccessful():
            self.stream.write(f'{theme.fail_info}FAILED{theme.reset}')
            failed, errored = len(result.failures), len(result.errors)
            if failed:
                details.append(f'{theme.fail_info}failures={failed}'
                               f'{theme.reset}')
            if errored:
                details.append(f'{theme.fail_info}errors={errored}'
                               f'{theme.reset}')
        elif run == 0 and not skipped:
            self.stream.write(f'{theme.warn}NO TESTS RAN{theme.reset}')
        else:
            self.stream.write(f'{theme.passed}OK{theme.reset}')
        if skipped:
            details.append(f'{theme.warn}skipped={skipped}{theme.reset}')
        if expected_fails:
            details.append(f'{theme.warn}expected failures={expected_fails}'
                           f'{theme.reset}')
        if unexpected:
            details.append(f'{theme.fail}unexpected successes={unexpected}'
                           f'{theme.reset}')
        if details:
            self.stream.writeln(' (%s)' % (', '.join(details),))
        else:
            self.stream.write('\n')
