"""Command line front end: unittest.main() / python -m unittest."""

import sys
import argparse
import os

from . import loader, runner
from .signals import installHandler

__unittest = True

# Exit status when no tests ran at all.
_NO_TESTS_EXITCODE = 5

MAIN_EXAMPLES = """\
Examples:
  %(prog)s test_module               - run tests from test_module
  %(prog)s module.TestClass          - run tests from module.TestClass
  %(prog)s module.Class.test_method  - run specified test method
  %(prog)s path/to/test_file.py      - run tests from test_file.py
"""

MODULE_EXAMPLES = """\
Examples:
  %(prog)s                           - run default set of tests
  %(prog)s MyTestSuite               - run suite 'MyTestSuite'
  %(prog)s MyTestCase.testSomething  - run MyTestCase.testSomething
  %(prog)s MyTestCase                - run all 'test*' test methods
                                       in MyTestCase
"""


def _convert_name(name):
    """Turn the path of an existing .py file (relative to the current
    directory) into a dotted module name; other names are kept."""
    if not (os.path.isfile(name) and name.lower().endswith('.py')):
        return name
    if os.path.isabs(name):
        relative = os.path.relpath(name, os.getcwd())
        if os.path.isabs(relative) or relative.startswith(os.pardir):
            # outside the current directory: cannot become a module name
            return name
        name = relative
    # both separators are accepted, whatever the platform
    return os.path.normpath(name)[:-3].replace('\\', '.').replace('/', '.')


def _convert_names(names):
    return [_convert_name(name) for name in names]


def _convert_select_pattern(pattern):
    # a plain substring matches anywhere
    if '*' not in pattern:
        pattern = '*%s*' % pattern
    return pattern


class TestProgram(object):
    """Load tests (from a module, names or by discovery), run them with a
    test runner and (optionally) exit with a status reflecting the result.
    """

    # class level defaults, also used by tests that skip __init__
    module = None
    verbosity = 1
    failfast = catchbreak = buffer = progName = warnings = None
    testNamePatterns = None
    _discovery_parser = None

    def __init__(self, module='__main__', defaultTest=None, argv=None,
                 testRunner=None, testLoader=loader.defaultTestLoader,
                 exit=True, verbosity=1, failfast=None, catchbreak=None,
                 buffer=None, warnings=None, *, tb_locals=False,
                 durations=None):
        if isinstance(module, str):
            self.module = __import__(module)
            for part in module.split('.')[1:]:
                self.module = getattr(self.module, part)
        else:
            self.module = module
        if argv is None:
            argv = sys.argv

        self.exit = exit
        self.failfast = failfast
        self.catchbreak = catchbreak
        self.verbosity = verbosity
        self.buffer = buffer
        self.tb_locals = tb_locals
        self.durations = durations
        if warnings is None and not sys.warnoptions:
            # show warnings that are hidden by default (e.g. deprecations),
            # unless -W or the warnings argument says otherwise
            self.warnings = 'default'
        else:
            self.warnings = warnings
        self.defaultTest = defaultTest
        self.testRunner = testRunner
        self.testLoader = testLoader
        self.progName = os.path.basename(argv[0])
        self.parseArgs(argv)
        self.runTests()

    def _print_help(self, *args, **kwargs):
        print(self._main_parser.format_help())
        if self.module is None:
            print(MAIN_EXAMPLES % {'prog': self.progName})
            self._discovery_parser.print_help()
        else:
            print(MODULE_EXAMPLES % {'prog': self.progName})

    def parseArgs(self, argv):
        self._initArgParsers()
        if self.module is None:
            if len(argv) > 1 and argv[1].lower() == 'discover':
                self._do_discovery(argv[2:])
                return
            self._main_parser.parse_args(argv[1:], self)
            if not self.tests:
                # 'python -m unittest [options]' means discovery
                self._do_discovery([])
                return
        else:
            self._main_parser.parse_args(argv[1:], self)

        if self.tests:
            self.testNames = _convert_names(self.tests)
        elif self.defaultTest is None:
            # load everything from self.module
            self.testNames = None
        elif isinstance(self.defaultTest, str):
            self.testNames = (self.defaultTest,)
        else:
            self.testNames = list(self.defaultTest)
        self.createTests()

    def createTests(self, from_discovery=False, Loader=None):
        if self.testNamePatterns:
            self.testLoader.testNamePatterns = self.testNamePatterns
        if from_discovery:
            test_loader = self.testLoader if Loader is None else Loader()
            self.test = test_loader.discover(self.start, self.pattern,
                                             self.top)
        elif self.testNames is None:
            self.test = self.testLoader.loadTestsFromModule(self.module)
        else:
            self.test = self.testLoader.loadTestsFromNames(self.testNames,
                                                           self.module)

    def _initArgParsers(self):
        common = self._getParentArgParser()
        self._main_parser = self._getMainArgParser(common)
        self._discovery_parser = self._getDiscoveryArgParser(common)

    def _getParentArgParser(self):
        parser = argparse.ArgumentParser(add_help=False)
        parser.add_argument('-v', '--verbose', dest='verbosity',
                            action='store_const', const=2,
                            help='Verbose output')
        parser.add_argument('-q', '--quiet', dest='verbosity',
                            action='store_const', const=0,
                            help='Quiet output')
        parser.add_argument('--locals', dest='tb_locals',
                            action='store_true',
                            help='Show local variables in tracebacks')
        parser.add_argument('--durations', dest='durations', type=int,
                            default=None, metavar='N',
                            help='Show the N slowest test cases (N=0 for all)')
        # Options already decided by the caller of main() are not offered.
        if self.failfast is None:
            parser.add_argument('-f', '--failfast', dest='failfast',
                                action='store_true',
                                help='Stop on first fail or error')
            self.failfast = False
        if self.catchbreak is None:
            parser.add_argument('-c', '--catch', dest='catchbreak',
                                action='store_true',
                                help='Catch Ctrl-C and display results so far')
            self.catchbreak = False
        if self.buffer is None:
            parser.add_argument('-b', '--buffer', dest='buffer',
                                action='store_true',
                                help='Buffer stdout and stderr during tests')
            self.buffer = False
        if self.testNamePatterns is None:
            parser.add_argument('-k', dest='testNamePatterns',
                                action='append', type=_convert_select_pattern,
                                help='Only run tests which match the given '
                                     'substring')
            self.testNamePatterns = []
        return parser

    def _getMainArgParser(self, parent):
        parser = argparse.ArgumentParser(parents=[parent], color=True)
        parser.prog = self.progName
        parser.print_help = self._print_help
        parser.add_argument('tests', nargs='*',
                            help='a list of any number of test modules, '
                                 'classes and test methods.')
        return parser

    def _getDiscoveryArgParser(self, parent):
        parser = argparse.ArgumentParser(parents=[parent], color=True)
        parser.prog = '%s discover' % self.progName
        parser.epilog = ('For test discovery all test modules must be '
                         'importable from the top level directory of the '
                         'project.')
        parser.add_argument('-s', '--start-directory', dest='start',
                            help="Directory to start discovery ('.' default)")
        parser.add_argument('-p', '--pattern', dest='pattern',
                            help="Pattern to match tests ('test*.py' default)")
        parser.add_argument('-t', '--top-level-directory', dest='top',
                            help='Top level directory of project (defaults '
                                 'to start directory)')
        # the same three values may also be given positionally
        for name in ('start', 'pattern', 'top'):
            parser.add_argument(name, nargs='?', default=argparse.SUPPRESS,
                                help=argparse.SUPPRESS)
        return parser

    def _do_discovery(self, argv, Loader=None):
        self.start = '.'
        self.pattern = 'test*.py'
        self.top = None
        if argv is not None:
            if self._discovery_parser is None:
                self._initArgParsers()
            self._discovery_parser.parse_args(argv, self)
        self.createTests(from_discovery=True, Loader=Loader)

    def _make_runner(self):
        if self.testRunner is None:
            self.testRunner = runner.TextTestRunner
        if not isinstance(self.testRunner, type):
            # already a runner instance
            return self.testRunner
        factory = self.testRunner
        try:
            return factory(verbosity=self.verbosity, failfast=self.failfast,
                           buffer=self.buffer, warnings=self.warnings,
                           tb_locals=self.tb_locals,
                           durations=self.durations)
        except TypeError:
            pass
        try:
            # runners predating tb_locals/durations
            return factory(verbosity=self.verbosity, failfast=self.failfast,
                           buffer=self.buffer, warnings=self.warnings)
        except TypeError:
            # runners predating all of these options
            return factory()

    def runTests(self):
        if self.catchbreak:
            installHandler()
        self.result = self._make_runner().run(self.test)
        if not self.exit:
            return
        if not self.result.wasSuccessful():
            sys.exit(1)
        if self.result.testsRun == 0 and not self.result.skipped:
            sys.exit(_NO_TESTS_EXITCODE)
        sys.exit(0)


main = TestProgram
