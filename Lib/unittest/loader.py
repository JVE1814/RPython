"""TestLoader: builds test suites from classes, modules, names and
directories."""

import os
import re
import sys
import traceback
import types
import functools

from fnmatch import fnmatch, fnmatchcase

from . import case, suite, util

__unittest = True

# File names discovery treats as importable test modules.
VALID_MODULE_NAME = re.compile(r'[_a-z]\w*\.py$', re.IGNORECASE)


class _FailedTest(case.TestCase):
    """A test standing in for something that could not be loaded; running
    it re-raises the loading error."""

    _testMethodName = None

    def __init__(self, method_name, exception):
        self._exception = exception
        super(_FailedTest, self).__init__(method_name)

    def __getattr__(self, name):
        if name != self._testMethodName:
            return super(_FailedTest, self).__getattr__(name)

        def testFailure():
            raise self._exception
        return testFailure


def _failed_test(method_name, exception, suiteClass, message):
    return suiteClass((_FailedTest(method_name, exception),)), message


def _make_failed_import_test(name, suiteClass):
    message = 'Failed to import test module: %s\n%s' % (
        name, traceback.format_exc())
    return _failed_test(name, ImportError(message), suiteClass, message)


def _make_failed_load_tests(name, exception, suiteClass):
    message = 'Failed to call load_tests:\n%s' % (traceback.format_exc(),)
    return _failed_test(name, exception, suiteClass, message)


def _make_failed_test(methodname, exception, suiteClass, message):
    return _failed_test(methodname, exception, suiteClass, message)


def _make_skipped_test(methodname, exception, suiteClass):
    @case.skip(str(exception))
    def testSkipped(self):
        pass
    skipped_class = type('ModuleSkipped', (case.TestCase,),
                         {methodname: testSkipped})
    return suiteClass((skipped_class(methodname),))


def _is_abstract(cls):
    return bool(getattr(cls, '__abstractmethods__', None))


def _strip_extension(path):
    return os.path.splitext(path)[0]


class TestLoader(object):
    """Creates test suites.

    Attributes that customise loading: testMethodPrefix, the comparison
    function sortTestMethodsUsing (None: no sorting), testNamePatterns
    (fnmatch patterns a test's full name must match) and suiteClass.
    Problems found while loading are recorded as strings in ``errors``.
    """

    testMethodPrefix = 'test'
    sortTestMethodsUsing = staticmethod(util.three_way_cmp)
    testNamePatterns = None
    suiteClass = suite.TestSuite
    _top_level_dir = None

    def __init__(self):
        super(TestLoader, self).__init__()
        self.errors = []
        # packages whose load_tests is currently running (avoids loading a
        # package recursively from its own load_tests)
        self._loading_packages = set()

    # -- the basic loaders ----------------------------------------------------

    def loadTestsFromTestCase(self, testCaseClass):
        """Return a suite of all test cases contained in testCaseClass."""
        if issubclass(testCaseClass, suite.TestSuite):
            raise TypeError('Test cases should not be derived from '
                            'TestSuite. Maybe you meant to derive from '
                            'TestCase?')
        if (testCaseClass in (case.TestCase, case.FunctionTestCase)
                or _is_abstract(testCaseClass)):
            # base classes (and abstract ones) contribute no tests
            names = []
        else:
            names = self.getTestCaseNames(testCaseClass)
            if not names and hasattr(testCaseClass, 'runTest'):
                names = ['runTest']
        return self.suiteClass(map(testCaseClass, names))

    def loadTestsFromModule(self, module, *, pattern=None):
        """Return a suite of all test cases contained in the given module,
        or what its load_tests(loader, tests, pattern) returns."""
        found = []
        for name in dir(module):
            obj = getattr(module, name)
            if (isinstance(obj, type) and issubclass(obj, case.TestCase) and
                    obj not in (case.TestCase, case.FunctionTestCase) and
                    not _is_abstract(obj)):
                found.append(self.loadTestsFromTestCase(obj))
        tests = self.suiteClass(found)
        load_tests = getattr(module, 'load_tests', None)
        if load_tests is None:
            return tests
        try:
            return load_tests(self, tests, pattern)
        except Exception as exc:
            error_case, message = _make_failed_load_tests(
                module.__name__, exc, self.suiteClass)
            self.errors.append(message)
            return error_case

    def _import_longest_prefix(self, parts):
        """Import the longest importable dotted prefix of *parts*.

        Returns (module, remaining parts, deferred error) where the deferred
        error describes the last failed import (or is None); if nothing can
        be imported the module is None."""
        candidate = parts[:]
        deferred = None
        while candidate:
            module_name = '.'.join(candidate)
            try:
                module = __import__(module_name)
            except ImportError:
                last = candidate.pop()
                deferred = _make_failed_import_test(last, self.suiteClass)
                continue
            return module, parts[1:], deferred
        return None, None, deferred

    def loadTestsFromName(self, name, module=None):
        """Return a suite of all test cases given a string specifier.

        The name may resolve to a module, a test case class, a test method
        within a test case class, a TestSuite or a callable returning a
        TestCase or TestSuite.  With *module*, the name is looked up
        relative to it.
        """
        parts = name.split('.')
        deferred = None
        if module is None:
            module, parts, deferred = self._import_longest_prefix(parts)
            if module is None:
                error_case, message = deferred
                self.errors.append(message)
                return error_case

        obj = module
        parent = None
        for part in parts:
            try:
                parent, obj = obj, getattr(obj, part)
            except AttributeError as exc:
                if (getattr(obj, '__path__', None) is not None
                        and deferred is not None):
                    # In a package we cannot tell a missing attribute from
                    # a submodule that failed to import: report the import.
                    error_case, message = deferred
                else:
                    error_case, message = _make_failed_test(
                        part, exc, self.suiteClass,
                        'Failed to access attribute:\n%s' % (
                            traceback.format_exc(),))
                self.errors.append(message)
                return error_case

        if isinstance(obj, types.ModuleType):
            return self.loadTestsFromModule(obj)
        if isinstance(obj, type) and issubclass(obj, case.TestCase):
            return self.loadTestsFromTestCase(obj)
        if (isinstance(obj, types.FunctionType) and isinstance(parent, type)
                and issubclass(parent, case.TestCase)):
            method_name = parts[-1]
            if _is_abstract(parent):
                raise TypeError('Cannot instantiate abstract test case %s'
                                % parent.__name__)
            instance = parent(method_name)
            # a staticmethod stays a plain function: treated as callable
            if not isinstance(getattr(instance, method_name),
                              types.FunctionType):
                return self.suiteClass([instance])
        elif isinstance(obj, suite.TestSuite):
            return obj

        if not callable(obj):
            raise TypeError("don't know how to make test from: %s" % obj)
        made = obj()
        if isinstance(made, suite.TestSuite):
            return made
        if isinstance(made, case.TestCase):
            return self.suiteClass([made])
        raise TypeError('calling %s returned %s, not a test' % (obj, made))

    def loadTestsFromNames(self, names, module=None):
        """Return a suite of all test cases found using the given sequence
        of string specifiers. See 'loadTestsFromName()'."""
        return self.suiteClass([self.loadTestsFromName(name, module)
                                for name in names])

    def getTestCaseNames(self, testCaseClass):
        """Return a sorted sequence of method names found within
        testCaseClass."""
        prefix = self.testMethodPrefix
        patterns = self.testNamePatterns

        def wanted(attrname):
            if not attrname.startswith(prefix):
                return False
            if not callable(getattr(testCaseClass, attrname)):
                return False
            if patterns is None:
                return True
            full_name = '%s.%s.%s' % (testCaseClass.__module__,
                                      testCaseClass.__qualname__, attrname)
            return any(fnmatchcase(full_name, pattern)
                       for pattern in patterns)

        names = [name for name in dir(testCaseClass) if wanted(name)]
        if self.sortTestMethodsUsing:
            names.sort(key=functools.cmp_to_key(self.sortTestMethodsUsing))
        return names

    # -- discovery ---------------------------------------------------------------------

    def discover(self, start_dir, pattern='test*.py', top_level_dir=None):
        """Find all test modules below *start_dir* (a directory or a dotted
        module name), recursing into packages, and return a suite of their
        tests.

        Only files matching *pattern* that are importable from
        *top_level_dir* (default: *start_dir*, which is added to sys.path)
        are loaded.  A package's load_tests(loader, tests, pattern), if
        present, decides what the package contributes; otherwise discovery
        continues into it.
        """
        saved_top = self._top_level_dir
        implicit_top = False
        if top_level_dir is None:
            if self._top_level_dir is not None:
                # nested call (from a package's load_tests)
                top_level_dir = self._top_level_dir
            else:
                implicit_top = True
                top_level_dir = start_dir

        top_level_dir = os.path.abspath(top_level_dir)
        if top_level_dir not in sys.path:
            # imports of the discovered modules go through sys.path
            sys.path.insert(0, top_level_dir)
        self._top_level_dir = top_level_dir

        not_importable = False
        namespace_dirs = None
        if os.path.isdir(os.path.abspath(start_dir)):
            start_dir = os.path.abspath(start_dir)
            if start_dir != top_level_dir:
                not_importable = not os.path.isfile(
                    os.path.join(start_dir, '__init__.py'))
        else:
            # a dotted module name
            try:
                __import__(start_dir)
            except ImportError:
                not_importable = True
            else:
                module = sys.modules[start_dir]
                top_part = start_dir.split('.')[0]
                location = self._module_location(module)
                if location is None:
                    namespace_dirs = list(module.__path__)
                else:
                    start_dir = location
                if implicit_top:
                    if namespace_dirs is None:
                        self._top_level_dir = \
                            self._get_directory_containing_module(top_part)
                    sys.path.remove(top_level_dir)

        if not_importable:
            raise ImportError('Start directory is not importable: %r'
                              % start_dir)

        if namespace_dirs is None:
            tests = list(self._find_tests(start_dir, pattern))
        else:
            tests = []
            package_path = start_dir.replace('.', os.path.sep)
            for directory in namespace_dirs:
                if implicit_top:
                    root = directory
                    if directory.endswith(package_path):
                        root = directory[:len(directory) - len(package_path)]
                    self._top_level_dir = os.path.abspath(root or os.sep)
                tests.extend(self._find_tests(directory, pattern,
                                              namespace=True))
        self._top_level_dir = saved_top
        return self.suiteClass(tests)

    @staticmethod
    def _module_location(module):
        """Directory of a module given by dotted name; None for a namespace
        package.  Raises TypeError for modules without a location."""
        try:
            filename = module.__file__
        except AttributeError:
            filename = None
            if module.__name__ in sys.builtin_module_names:
                raise TypeError('Can not use builtin modules '
                                'as dotted module names') from None
            raise TypeError("don't know how to discover from {!r}"
                            .format(module)) from None
        if filename is None:
            if getattr(module, '__path__', None) is None:
                raise TypeError("don't know how to discover from {!r}"
                                .format(module))
            return None
        return os.path.abspath(os.path.dirname(filename))

    def _get_directory_containing_module(self, module_name):
        module = sys.modules[module_name]
        full_path = os.path.abspath(module.__file__)
        if os.path.basename(full_path).lower().startswith('__init__.py'):
            # a package: its parent directory is the import root
            return os.path.dirname(os.path.dirname(full_path))
        return os.path.dirname(full_path)

    def _get_name_from_path(self, path):
        if path == self._top_level_dir:
            return '.'
        relative = os.path.relpath(_strip_extension(path),
                                   self._top_level_dir)
        assert not os.path.isabs(relative), 'Path must be within the project'
        assert not relative.startswith('..'), 'Path must be within the project'
        return relative.replace(os.path.sep, '.')

    def _get_module_from_name(self, name):
        __import__(name)
        return sys.modules[name]

    def _match_path(self, path, full_path, pattern):
        # overridable hook
        return fnmatch(path, pattern)

    def _find_tests(self, start_dir, pattern, namespace=False):
        """Used by discovery. Yields test suites it loads."""
        name = self._get_name_from_path(start_dir)
        # '.' is the top level directory, never a package itself; a package
        # whose load_tests is running is not loaded again
        if (name != '.' and name not in self._loading_packages
                and not namespace):
            tests, recurse = self._find_test_path(start_dir, pattern)
            if tests is not None:
                yield tests
            if not recurse:
                return

        for entry in sorted(os.listdir(start_dir)):
            full_path = os.path.join(start_dir, entry)
            tests, recurse = self._find_test_path(full_path, pattern)
            if tests is not None:
                yield tests
            if recurse:
                package = self._get_name_from_path(full_path)
                self._loading_packages.add(package)
                try:
                    yield from self._find_tests(full_path, pattern)
                finally:
                    self._loading_packages.discard(package)

    def _import_for_discovery(self, name):
        """Import *name*; returns (module, None) or (None, test reporting
        the problem)."""
        try:
            return self._get_module_from_name(name), None
        except case.SkipTest as exc:
            return None, _make_skipped_test(name, exc, self.suiteClass)
        except:
            error_case, message = _make_failed_import_test(name,
                                                           self.suiteClass)
            self.errors.append(message)
            return None, error_case

    def _find_test_path(self, full_path, pattern):
        """Load the tests of one file, or of a package's __init__.

        Returns (tests or None, whether to recurse into the directory)."""
        basename = os.path.basename(full_path)
        if os.path.isfile(full_path):
            if not VALID_MODULE_NAME.match(basename):
                return None, False
            if not self._match_path(basename, full_path, pattern):
                return None, False
            name = self._get_name_from_path(full_path)
            module, problem = self._import_for_discovery(name)
            if module is None:
                return problem, False
            self._check_module_origin(module, full_path)
            return self.loadTestsFromModule(module, pattern=pattern), False

        if os.path.isdir(full_path):
            if not os.path.isfile(os.path.join(full_path, '__init__.py')):
                return None, False
            name = self._get_name_from_path(full_path)
            package, problem = self._import_for_discovery(name)
            if package is None:
                return problem, False
            has_load_tests = getattr(package, 'load_tests', None) is not None
            self._loading_packages.add(name)
            try:
                tests = self.loadTestsFromModule(package, pattern=pattern)
            finally:
                self._loading_packages.discard(name)
            # a package with load_tests is responsible for its contents
            return tests, not has_load_tests

        # neither file nor directory (socket, broken link ...)
        return None, False

    @staticmethod
    def _check_module_origin(module, full_path):
        """Refuse a module that was imported from somewhere else than the
        file discovery found (e.g. an installed module of the same name)."""
        module_file = os.path.abspath(getattr(module, '__file__', full_path))
        actual = _strip_extension(os.path.realpath(module_file))
        expected = _strip_extension(os.path.realpath(full_path))
        if actual.lower() == expected.lower():
            return
        raise ImportError(
            '%r module incorrectly imported from %r. Expected %r. '
            'Is this module globally installed?' % (
                _strip_extension(os.path.basename(full_path)),
                os.path.dirname(actual), os.path.dirname(full_path)))


defaultTestLoader = TestLoader()
