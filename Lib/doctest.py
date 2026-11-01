"""Run the interactive examples found in docstrings and text files.

An *example* is a block of lines starting with the '>>> ' prompt (and
'... ' continuation lines), followed by the output it is expected to
produce, up to the next blank line or prompt.  doctest executes each example
and compares the actual output with the expected one.

Main entry points:

    testmod(m)          test the docstrings of module m (default: __main__)
    testfile(path)      test the examples in a text file
    DocTestSuite(m)     unittest suite for the docstrings of a module
    DocFileSuite(path)  unittest suite for text files

Building blocks: Example, DocTest, DocTestParser, DocTestFinder,
DocTestRunner, OutputChecker, DebugRunner.  Option flags (ELLIPSIS,
NORMALIZE_WHITESPACE, ...) change how output is compared and reported;
they can be given per example with a "# doctest: +FLAG" comment.
"""

__docformat__ = 'reStructuredText en'

__all__ = [
    # option flags
    'register_optionflag',
    'DONT_ACCEPT_TRUE_FOR_1',
    'DONT_ACCEPT_BLANKLINE',
    'NORMALIZE_WHITESPACE',
    'ELLIPSIS',
    'SKIP',
    'IGNORE_EXCEPTION_DETAIL',
    'COMPARISON_FLAGS',
    'REPORT_UDIFF',
    'REPORT_CDIFF',
    'REPORT_NDIFF',
    'REPORT_ONLY_FIRST_FAILURE',
    'REPORTING_FLAGS',
    'FAIL_FAST',
    # public classes
    'Example',
    'DocTest',
    'DocTestParser',
    'DocTestFinder',
    'DocTestRunner',
    'OutputChecker',
    'DocTestFailure',
    'UnexpectedException',
    'DebugRunner',
    # functions
    'testmod',
    'testfile',
    'run_docstring_examples',
    'DocTestSuite',
    'DocFileSuite',
    'set_unittest_reportflags',
    'script_from_examples',
    'testsource',
    'debug_src',
    'debug',
]

import __future__
import difflib
import functools
import inspect
import linecache
import os
import re
import sys
import traceback
import types
import unittest
from io import StringIO, IncrementalNewlineDecoder
from collections import namedtuple
import _colorize

# Frames of this module are hidden in unittest tracebacks.
__unittest = True


class TestResults(namedtuple('TestResults', 'failed attempted')):
    """(failed, attempted) counts, plus the number of skipped examples in
    the `skipped` attribute (not part of the tuple)."""

    def __new__(cls, failed, attempted, *, skipped=0):
        results = super().__new__(cls, failed, attempted)
        results.skipped = skipped
        return results


# ---------------------------------------------------------------- flags

OPTIONFLAGS_BY_NAME = {}


def register_optionflag(name):
    """Register a named option flag (returns the existing value if the name
    is already registered)."""
    return OPTIONFLAGS_BY_NAME.setdefault(name, 1 << len(OPTIONFLAGS_BY_NAME))


DONT_ACCEPT_TRUE_FOR_1 = register_optionflag('DONT_ACCEPT_TRUE_FOR_1')
DONT_ACCEPT_BLANKLINE = register_optionflag('DONT_ACCEPT_BLANKLINE')
NORMALIZE_WHITESPACE = register_optionflag('NORMALIZE_WHITESPACE')
ELLIPSIS = register_optionflag('ELLIPSIS')
SKIP = register_optionflag('SKIP')
IGNORE_EXCEPTION_DETAIL = register_optionflag('IGNORE_EXCEPTION_DETAIL')

COMPARISON_FLAGS = (DONT_ACCEPT_TRUE_FOR_1 | DONT_ACCEPT_BLANKLINE |
                    NORMALIZE_WHITESPACE | ELLIPSIS | SKIP |
                    IGNORE_EXCEPTION_DETAIL)

REPORT_UDIFF = register_optionflag('REPORT_UDIFF')
REPORT_CDIFF = register_optionflag('REPORT_CDIFF')
REPORT_NDIFF = register_optionflag('REPORT_NDIFF')
REPORT_ONLY_FIRST_FAILURE = register_optionflag('REPORT_ONLY_FIRST_FAILURE')
FAIL_FAST = register_optionflag('FAIL_FAST')

REPORTING_FLAGS = (REPORT_UDIFF | REPORT_CDIFF | REPORT_NDIFF |
                   REPORT_ONLY_FIRST_FAILURE | FAIL_FAST)

BLANKLINE_MARKER = '<BLANKLINE>'
ELLIPSIS_MARKER = '...'


# ---------------------------------------------------------------- helpers

def _extract_future_flags(globs):
    """Compiler flags of the __future__ features imported into globs."""
    flags = 0
    for name in __future__.all_feature_names:
        feature = globs.get(name, None)
        if feature is getattr(__future__, name):
            flags |= feature.compiler_flag
    return flags


def _normalize_module(module, depth=2):
    """Module object for `module` (a module, a name, or None meaning the
    caller `depth` frames up)."""
    if inspect.ismodule(module):
        return module
    if isinstance(module, str):
        return __import__(module, globals(), locals(), ["*"])
    if module is None:
        try:
            frame = sys._getframe(depth)
        except AttributeError:
            frame = None
        if frame is None:
            return sys.modules['__main__']
        return sys.modules[frame.f_globals['__name__']]
    raise TypeError("Expected a module, string, or None")


def _newline_convert(data):
    # universal newlines for text that was read in binary mode
    return IncrementalNewlineDecoder(None, True).decode(data, True)


def _load_testfile(filename, package, module_relative, encoding):
    if module_relative:
        package = _normalize_module(package, 3)
        filename = _module_relative_path(package, filename)
        loader = getattr(package, '__loader__', None)
        if loader is None:
            spec = getattr(package, '__spec__', None)
            loader = getattr(spec, 'loader', None) if spec else None
        if loader is not None and hasattr(loader, 'get_data'):
            try:
                data = loader.get_data(filename)
            except OSError:
                pass
            else:
                text = data.decode(encoding)
                return _newline_convert(text), filename
    with open(filename, encoding=encoding) as stream:
        return stream.read(), filename


def _indent(s, indent=4):
    """Indent every non-empty line of `s` by `indent` spaces."""
    pad = ' ' * indent
    return re.sub('(?m)^(?!$)', pad, s)


def _exception_traceback(exc_info):
    """The formatted traceback of an exc_info triple."""
    out = StringIO()
    exc_type, exc_value, exc_tb = exc_info
    traceback.print_exception(exc_type, exc_value, exc_tb, file=out, colorize=False)
    return out.getvalue()


class _SpoofOut(StringIO):
    """Captures example output; getvalue() makes sure non-empty output ends
    with a newline."""

    def getvalue(self):
        text = StringIO.getvalue(self)
        if text and not text.endswith("\n"):
            text += "\n"
        return text

    def truncate(self, size=None):
        self.seek(size if size is not None else 0)
        StringIO.truncate(self)


def _ellipsis_match(want, got):
    """True if `got` matches `want`, in which '...' matches any substring."""
    if ELLIPSIS_MARKER not in want:
        return want == got
    pieces = want.split(ELLIPSIS_MARKER)
    head, tail = pieces[0], pieces[-1]
    if not got.startswith(head):
        return False
    start = len(head)
    if tail:
        if not got.endswith(tail):
            return False
        end = len(got) - len(tail)
    else:
        end = len(got)
    if start > end:
        return False
    for piece in pieces[1:-1]:
        found = got.find(piece, start, end)
        if found < 0:
            return False
        start = found + len(piece)
    return True


def _comment_line(line):
    """The line commented out with '# '."""
    line = line.rstrip()
    return '# ' + line if line else '#'


def _strip_exception_details(msg):
    """Reduce an exception message to the bare exception name: drop the
    module path and everything after the first colon or newline."""
    end = len(msg)
    for stop in (msg.find('\n'), msg.find(':')):
        if 0 <= stop < end:
            end = stop
    head = msg[:end]
    dot = head.rfind('.')
    return head[dot + 1:]


class _OutputRedirectingPdb:
    """Placeholder replaced below by a pdb.Pdb subclass when pdb is
    importable."""


def _module_relative_path(module, test_path):
    if not inspect.ismodule(module):
        raise TypeError('Expected a module: %r' % module)
    if test_path.startswith('/'):
        raise ValueError('Module-relative files may not have absolute paths')
    test_path = os.path.join(*test_path.split('/'))
    if hasattr(module, '__file__') and module.__file__:
        basedir = os.path.split(module.__file__)[0]
    elif module.__name__ == '__main__':
        if len(sys.argv) > 0 and sys.argv[0] != '':
            basedir = os.path.split(sys.argv[0])[0]
        else:
            basedir = os.curdir
    else:
        if hasattr(module, '__path__'):
            for directory in module.__path__:
                candidate = os.path.join(directory, test_path)
                if os.path.exists(candidate):
                    return candidate
        raise ValueError("Can't resolve paths relative to the module "
                         "%r (it has no __file__)" % module.__name__)
    return os.path.join(basedir, test_path)


# ---------------------------------------------------------------- Example / DocTest

class Example:
    """One interactive example: `source` (ending in a newline), the
    expected output `want`, the expected exception message `exc_msg` (or
    None), its 0-based `lineno` within the docstring, the `indent` of its
    prompt and per-example `options` ({flag: bool})."""

    def __init__(self, source, want, exc_msg=None, lineno=0, indent=0,
                 options=None):
        if not source.endswith('\n'):
            source += '\n'
        if want and not want.endswith('\n'):
            want += '\n'
        if exc_msg is not None and not exc_msg.endswith('\n'):
            exc_msg += '\n'
        self.source = source
        self.want = want
        self.lineno = lineno
        self.indent = indent
        self.options = {} if options is None else options
        self.exc_msg = exc_msg

    def _key(self):
        return (self.source, self.want, self.lineno, self.indent,
                self.options, self.exc_msg)

    def __eq__(self, other):
        if type(self) is not type(other):
            return NotImplemented
        return self._key() == other._key()

    def __hash__(self):
        return hash((self.source, self.want, self.lineno, self.indent,
                     self.exc_msg))


class DocTest:
    """The examples of one docstring or file plus the namespace (`globs`)
    they run in, the test `name`, `filename`, 0-based `lineno` of the
    docstring in that file (or None) and the original `docstring`."""

    def __init__(self, examples, globs, name, filename, lineno, docstring):
        assert not isinstance(examples, str), "DocTest no longer accepts str; use DocTestParser instead"
        self.examples = examples
        self.docstring = docstring
        self.globs = globs.copy()
        self.name = name
        self.filename = filename
        self.lineno = lineno

    def __repr__(self):
        count = len(self.examples)
        if count == 0:
            examples = 'no examples'
        elif count == 1:
            examples = '1 example'
        else:
            examples = f'{count} examples'
        return f'<{type(self).__name__} {self.name} from {self.filename}:{self.lineno} ({examples})>'

    def _key(self):
        return (self.examples, self.docstring, self.globs, self.name,
                self.filename, self.lineno)

    def __eq__(self, other):
        if type(self) is not type(other):
            return NotImplemented
        return self._key() == other._key()

    def __hash__(self):
        return hash((self.docstring, self.name, self.filename, self.lineno))

    def __lt__(self, other):
        if not isinstance(other, DocTest):
            return NotImplemented
        mine = (self.name, self.filename, -1 if self.lineno is None else self.lineno, id(self))
        theirs = (other.name, other.filename, -1 if other.lineno is None else other.lineno, id(other))
        return mine < theirs


# ---------------------------------------------------------------- parser

class DocTestParser:
    """Splits a docstring into text and Example objects."""

    # A prompt line, its continuation lines, then the expected output (the
    # following lines up to a blank line or the next prompt).
    _EXAMPLE_RE = re.compile(r'''
        (?P<source>
            (?:^(?P<indent> [ ]*) >>>    .*)    # the prompt line
            (?:\n           [ ]*  \.\.\. .*)*)  # continuation lines
        \n?
        (?P<want> (?:(?![ ]*$)     # not a blank line
                     (?![ ]*>>>)   # not another prompt
                     .+$\n?        # any other line
                  )*)
        ''', re.MULTILINE | re.VERBOSE)

    # Expected output that is an exception: a traceback header, indented
    # stack lines, then the exception message.
    _EXCEPTION_RE = re.compile(r"""
        ^(?P<hdr> Traceback\ \(
            (?: most\ recent\ call\ last
            |   innermost\ last
            ) \) :
        )
        \s* $
        (?P<stack> .*?)
        ^ (?P<msg> \w+ .*)
        """, re.VERBOSE | re.MULTILINE | re.DOTALL)

    _IS_BLANK_OR_COMMENT = re.compile(r'^[ ]*(#.*)?$').match

    def parse(self, string, name='<string>'):
        """Return a list alternating between text strings and Examples."""
        string = string.expandtabs()
        margin = self._min_indent(string)
        if margin > 0:
            string = '\n'.join(line[margin:] for line in string.split('\n'))
        pieces = []
        pos = 0
        lineno = 0
        for match in self._EXAMPLE_RE.finditer(string):
            pieces.append(string[pos:match.start()])
            lineno += string.count('\n', pos, match.start())
            source, options, want, exc_msg = self._parse_example(match, name, lineno)
            if not self._IS_BLANK_OR_COMMENT(source):
                pieces.append(Example(source, want, exc_msg, lineno=lineno,
                                      indent=margin + len(match.group('indent')),
                                      options=options))
            lineno += string.count('\n', match.start(), match.end())
            pos = match.end()
        pieces.append(string[pos:])
        return pieces

    def get_doctest(self, string, globs, name, filename, lineno):
        """A DocTest for the examples in `string`."""
        return DocTest(self.get_examples(string, name), globs,
                       name, filename, lineno, string)

    def get_examples(self, string, name='<string>'):
        """Just the Example objects of `string`."""
        return [piece for piece in self.parse(string, name)
                if isinstance(piece, Example)]

    def _parse_example(self, m, name, lineno):
        indent = len(m.group('indent'))
        source_lines = m.group('source').split('\n')
        self._check_prompt_blank(source_lines, indent, name, lineno)
        self._check_prefix(source_lines[1:], ' ' * indent + '.', name, lineno)
        source = '\n'.join(line[indent + 4:] for line in source_lines)

        want = m.group('want')
        want_lines = want.split('\n')
        if len(want_lines) > 1 and re.match(r' *$', want_lines[-1]):
            del want_lines[-1]
        self._check_prefix(want_lines, ' ' * indent, name,
                           lineno + len(source_lines))
        want = '\n'.join(line[indent:] for line in want_lines)

        exc = self._EXCEPTION_RE.match(want)
        exc_msg = exc.group('msg') if exc else None
        options = self._find_options(source, name, lineno)
        return source, options, want, exc_msg

    _OPTION_DIRECTIVE_RE = re.compile(r'#\s*doctest:\s*([^\n\'"]*)$', re.MULTILINE)

    def _find_options(self, source, name, lineno):
        options = {}
        for m in self._OPTION_DIRECTIVE_RE.finditer(source):
            for option in m.group(1).replace(',', ' ').split():
                if (option[:1] not in '+-' or
                        option[1:] not in OPTIONFLAGS_BY_NAME):
                    raise ValueError('line %r of the doctest for %s '
                                     'has an invalid option: %r' %
                                     (lineno + 1, name, option))
                options[OPTIONFLAGS_BY_NAME[option[1:]]] = option[0] == '+'
        if options and self._IS_BLANK_OR_COMMENT(source):
            raise ValueError('line %r of the doctest for %s has an option '
                             'directive on a line with no example: %r' %
                             (lineno, name, source))
        return options

    _INDENT_RE = re.compile(r'^([ ]*)(?=\S)', re.MULTILINE)

    def _min_indent(self, s):
        indents = [len(indent) for indent in self._INDENT_RE.findall(s)]
        return min(indents) if indents else 0

    def _check_prompt_blank(self, lines, indent, name, lineno):
        for offset, line in enumerate(lines):
            if len(line) >= indent + 4 and line[indent + 3] != ' ':
                raise ValueError('line %r of the docstring for %s '
                                 'lacks blank after %s: %r' %
                                 (lineno + offset + 1, name,
                                  line[indent:indent + 3], line))

    def _check_prefix(self, lines, prefix, name, lineno):
        for offset, line in enumerate(lines):
            if line and not line.startswith(prefix):
                raise ValueError('line %r of the docstring for %s has '
                                 'inconsistent leading whitespace: %r' %
                                 (lineno + offset + 1, name, line))


# ---------------------------------------------------------------- finder

class DocTestFinder:
    """Collects the DocTests of an object and the objects it contains."""

    def __init__(self, verbose=False, parser=DocTestParser(),
                 recurse=True, exclude_empty=True):
        self._parser = parser
        self._verbose = verbose
        self._recurse = recurse
        self._exclude_empty = exclude_empty

    def find(self, obj, name=None, module=None, globs=None, extraglobs=None):
        """Return the DocTests for `obj` (and the objects it contains),
        sorted by name.

        name defaults to obj.__name__; module (for line numbers and for
        deciding what belongs to it) defaults to inspect.getmodule(obj);
        False means no module.  The tests run in a copy of `globs`
        (default: the module's namespace) updated with `extraglobs`."""
        if name is None:
            name = getattr(obj, '__name__', None)
            if name is None:
                raise ValueError("DocTestFinder.find: name must be given "
                                 "when obj.__name__ doesn't exist: %r" % (type(obj),))
        if module is False:
            module = None
        elif module is None:
            module = inspect.getmodule(obj)

        source_lines = self._source_lines(obj, module)
        if globs is None:
            globs = {} if module is None else module.__dict__.copy()
        else:
            globs = globs.copy()
        if extraglobs is not None:
            globs.update(extraglobs)
        if '__name__' not in globs:
            globs['__name__'] = '__main__'

        tests = []
        self._find(tests, obj, name, module, source_lines, globs, set())
        # deterministic order (names may repeat)
        tests.sort()
        return tests

    @staticmethod
    def _source_lines(obj, module):
        try:
            path = inspect.getsourcefile(obj)
        except TypeError:
            return None
        if not path:
            path = inspect.getfile(obj)
            if not path[0] + path[-2:] == '<]>':
                path = None
        if path is None:
            return None
        if module is not None:
            lines = linecache.getlines(path, module.__dict__)
        else:
            lines = linecache.getlines(path)
        return lines or None

    def _from_module(self, module, obj):
        """Whether `obj` is defined in `module`."""
        if module is None:
            return True
        if inspect.getmodule(obj) is not None:
            return module is inspect.getmodule(obj)
        if inspect.isfunction(obj):
            return module.__dict__ is obj.__globals__
        if (inspect.ismethoddescriptor(obj) or inspect.ismethodwrapper(obj)):
            if hasattr(obj, '__objclass__'):
                owner = obj.__objclass__.__module__
            elif hasattr(obj, '__module__'):
                owner = obj.__module__
            else:
                return True
            return module.__name__ == owner
        if inspect.isclass(obj):
            return module.__name__ == obj.__module__
        if hasattr(obj, '__module__'):
            return module.__name__ == obj.__module__
        if isinstance(obj, property):
            return True
        raise ValueError("object must be a class or function")

    def _is_routine(self, obj):
        maybe = obj
        try:
            maybe = inspect.unwrap(maybe)
        except ValueError:
            pass
        return inspect.isroutine(maybe)

    def _find(self, tests, obj, name, module, source_lines, globs, seen):
        if self._verbose:
            print('Finding tests in %s' % name)
        if id(obj) in seen:
            return
        seen.add(id(obj))

        test = self._get_test(obj, name, module, globs, source_lines)
        if test is not None:
            tests.append(test)
        if not self._recurse:
            return

        if inspect.ismodule(obj):
            for key, value in obj.__dict__.items():
                child = f'{name}.{key}'
                if ((self._is_routine(value) or inspect.isclass(value))
                        and self._from_module(module, value)):
                    self._find(tests, value, child, module, source_lines, globs, seen)

        if inspect.ismodule(obj):
            for key, value in getattr(obj, '__test__', {}).items():
                if not isinstance(key, str):
                    raise ValueError("DocTestFinder.find: __test__ keys "
                                     "must be strings: %r" % (type(key),))
                if not (inspect.isroutine(value) or inspect.isclass(value) or
                        inspect.ismodule(value) or isinstance(value, str)):
                    raise ValueError("DocTestFinder.find: __test__ values "
                                     "must be strings, functions, methods, "
                                     "classes, or modules: %r" % (type(value),))
                self._find(tests, value, f'{name}.__test__.{key}', module,
                           source_lines, globs, seen)

        if inspect.isclass(obj):
            for key, value in obj.__dict__.items():
                if isinstance(value, (staticmethod, classmethod)):
                    value = value.__func__
                if ((inspect.isroutine(value) or inspect.isclass(value) or
                        isinstance(value, property)) and
                        self._from_module(module, value)):
                    self._find(tests, value, f'{name}.{key}', module,
                               source_lines, globs, seen)

    def _get_test(self, obj, name, module, globs, source_lines):
        if isinstance(obj, str):
            docstring = obj
        else:
            try:
                docstring = obj.__doc__
                if docstring is None:
                    docstring = ''
                elif not isinstance(docstring, str):
                    docstring = str(docstring)
            except (TypeError, AttributeError):
                docstring = ''
        if self._exclude_empty and not docstring:
            return None
        lineno = self._find_lineno(obj, source_lines)
        if module is None:
            filename = None
        else:
            filename = getattr(module, '__file__', None) or module.__name__
            if filename.endswith('.pyc'):
                filename = filename[:-1]
        return self._parser.get_doctest(docstring, globs, name, filename, lineno)

    @staticmethod
    def _find_string_lineno(text, source_lines):
        """Line (0-based) of the string literal `text` (e.g. a __test__
        entry) in the source: the place where all its non-blank lines
        appear consecutively."""
        if not source_lines:
            return None
        wanted = [(offset, line.strip())
                  for offset, line in enumerate(text.split('\n')) if line.strip()]
        if not wanted:
            return None
        stripped = [line.strip() for line in source_lines]
        first_offset, first_line = wanted[0]
        for start, line in enumerate(stripped):
            if not line.endswith(first_line):
                continue
            position = start
            matched = True
            for offset, expected in wanted[1:]:
                position = start + (offset - first_offset)
                if position >= len(stripped) or stripped[position] != expected:
                    matched = False
                    break
            if matched:
                return start - first_offset
        return None

    _DOCSTRING_START = re.compile(r'(^|.*:)\s*\w*("|\')')

    def _find_lineno(self, obj, source_lines):
        """0-based line of the docstring of `obj` in its source, or None."""
        lineno = None
        if isinstance(obj, str):
            return self._find_string_lineno(obj, source_lines)
        docstring = getattr(obj, '__doc__', None)

        if inspect.ismodule(obj) and docstring is not None:
            lineno = 0

        if inspect.isclass(obj) and docstring is not None:
            if source_lines is None:
                return None
            header = re.compile(r'^\s*class\s*%s\b' % re.escape(obj.__name__))
            for index, line in enumerate(source_lines):
                if header.match(line):
                    lineno = index
                    break

        if isinstance(obj, functools.cached_property):
            obj = obj.func
        if inspect.ismethod(obj):
            obj = obj.__func__
        if isinstance(obj, property):
            obj = obj.fget
        if inspect.isroutine(obj) and getattr(obj, '__doc__', None):
            try:
                obj = inspect.unwrap(obj)
            except ValueError:
                pass
            try:
                obj = obj.__code__
            except AttributeError:
                pass
        if inspect.istraceback(obj):
            obj = obj.tb_frame
        if inspect.isframe(obj):
            obj = obj.f_code
        if inspect.iscode(obj):
            lineno = obj.co_firstlineno - 1

        if lineno is not None:
            if source_lines is None:
                return lineno + 1
            for index in range(lineno, len(source_lines)):
                if self._DOCSTRING_START.match(source_lines[index]):
                    return index
        return None


# ---------------------------------------------------------------- output checking

class OutputChecker:
    """Compares actual with expected output under the option flags."""

    def _to_str(self, text):
        # Non-ASCII characters compare equal to their backslash escapes.
        text = text if isinstance(text, str) else str(text)
        return text.encode("ascii", "backslashreplace").decode("ascii")

    def check_output(self, want, got, optionflags):
        """True if `got` is acceptable as the output `want`."""
        want = self._to_str(want)
        got = self._to_str(got)
        if got == want:
            return True

        if not optionflags & DONT_ACCEPT_TRUE_FOR_1:
            if (got, want) in (("True\n", "1\n"), ("False\n", "0\n")):
                return True

        if not optionflags & DONT_ACCEPT_BLANKLINE:
            # <BLANKLINE> stands for an empty line in the output
            want = re.sub(r'(?m)^%s\s*?$' % re.escape(BLANKLINE_MARKER), '', want)
            got = re.sub(r'(?m)^[^\S\n]+$', '', got)
            if got == want:
                return True

        if optionflags & NORMALIZE_WHITESPACE:
            got = ' '.join(got.split())
            want = ' '.join(want.split())
            if got == want:
                return True

        if optionflags & ELLIPSIS:
            if _ellipsis_match(want, got):
                return True

        return False

    def _do_a_fancy_diff(self, want, got, optionflags):
        if not optionflags & (REPORT_UDIFF | REPORT_CDIFF | REPORT_NDIFF):
            return False
        if optionflags & REPORT_NDIFF:
            return True
        # short outputs are clearer without a diff
        return want.count('\n') > 2 and got.count('\n') > 2

    def output_difference(self, example, got, optionflags):
        """Text describing how `got` differs from example.want."""
        want = example.want
        if not optionflags & DONT_ACCEPT_BLANKLINE:
            got = re.sub(r'(?m)^[ ]*(?=\n)', BLANKLINE_MARKER, got)

        if self._do_a_fancy_diff(want, got, optionflags):
            want_lines = want.splitlines(keepends=True)
            got_lines = got.splitlines(keepends=True)
            if optionflags & REPORT_UDIFF:
                diff = list(difflib.unified_diff(want_lines, got_lines, n=2))[2:]
                kind = 'unified diff with -expected +actual'
            elif optionflags & REPORT_CDIFF:
                diff = list(difflib.context_diff(want_lines, got_lines, n=2))[2:]
                kind = 'context diff with expected followed by actual'
            else:
                engine = difflib.Differ(charjunk=difflib.IS_CHARACTER_JUNK)
                diff = list(engine.compare(want_lines, got_lines))
                kind = 'ndiff with -expected +actual'
            body = ''.join(line.rstrip() + '\n' for line in diff)
            return f'Differences ({kind}):\n' + _indent(body)

        if want and got:
            return 'Expected:\n%sGot:\n%s' % (_indent(want), _indent(got))
        if want:
            return 'Expected:\n%sGot nothing\n' % _indent(want)
        if got:
            return 'Expected nothing\nGot:\n%s' % _indent(got)
        return 'Expected nothing\nGot nothing\n'


# ---------------------------------------------------------------- runner

SUCCESS, FAILURE, BOOM = range(3)


class DocTestRunner:
    """Runs DocTests and accumulates statistics.

    DocTestRunner(checker=None, verbose=None, optionflags=0)

    run(test) executes the examples of one DocTest and reports to `out`
    (default: sys.stdout); summarize() prints totals.  The report_*
    methods may be overridden to customize the output.
    """

    DIVIDER = "*" * 70

    def __init__(self, checker=None, verbose=None, optionflags=0):
        self._checker = checker or OutputChecker()
        if verbose is None:
            verbose = '-v' in sys.argv
        self._verbose = verbose
        self.optionflags = optionflags
        self.original_optionflags = optionflags
        self.tries = 0
        self.failures = 0
        self.skips = 0
        self._stats = {}
        self._fakeout = _SpoofOut()

    # ---- reporting

    def report_start(self, out, test, example):
        """Called before running an example (verbose mode only)."""
        if self._verbose:
            if example.want:
                out('Trying:\n' + _indent(example.source) +
                    'Expecting:\n' + _indent(example.want))
            else:
                out('Trying:\n' + _indent(example.source) +
                    'Expecting nothing\n')

    def report_success(self, out, test, example, got):
        """Called when an example produced the expected output."""
        if self._verbose:
            out("ok\n")

    def report_failure(self, out, test, example, got):
        """Called when an example produced the wrong output."""
        out(self._failure_header(test, example) +
            self._checker.output_difference(example, got, self.optionflags))

    def report_unexpected_exception(self, out, test, example, exc_info):
        """Called when an example raised an unexpected exception."""
        out(self._failure_header(test, example) +
            'Exception raised:\n' + _indent(_exception_traceback(exc_info)))

    def _failure_header(self, test, example):
        lines = [self.DIVIDER]
        if test.filename:
            if test.lineno is not None and example.lineno is not None:
                lineno = test.lineno + example.lineno + 1
            else:
                lineno = '?'
            lines.append('File "%s", line %s, in %s' % (test.filename, lineno, test.name))
        else:
            lines.append('Line %s, in %s' % (example.lineno + 1, test.name))
        lines.append('Failed example:')
        lines.append(_indent(example.source))
        return '\n'.join(lines)

    # ---- running

    def _run_examples(self, test, compileflags, out):
        failures = tries = skips = 0
        original = self.optionflags
        check = self._checker.check_output

        for index, example in enumerate(test.examples):
            quiet = bool(self.optionflags & REPORT_ONLY_FIRST_FAILURE and failures)
            self.optionflags = original
            for flag, enabled in example.options.items():
                if enabled:
                    self.optionflags |= flag
                else:
                    self.optionflags &= ~flag

            if self.optionflags & SKIP:
                skips += 1
                self._example_skipped(test, index, example)
                continue

            tries += 1
            if not quiet:
                self.report_start(out, test, example)

            filename = '<doctest %s[%d]>' % (test.name, index)
            try:
                code = compile(example.source, filename, "single",
                               compileflags, True)
                exec(code, test.globs)
                self.debugger.set_continue()
                exc_info = None
            except KeyboardInterrupt:
                raise
            except BaseException as exc:
                if isinstance(exc, SyntaxError) or exc.__traceback__ is None:
                    tb = exc.__traceback__
                else:
                    # drop this frame (the exec() call) from the traceback
                    tb = exc.__traceback__.tb_next
                    if isinstance(exc, SyntaxError) is False and tb is None:
                        tb = exc.__traceback__
                exc_info = (type(exc), exc, tb)
                self.debugger.set_continue()

            got = self._fakeout.getvalue()
            self._fakeout.truncate(0)

            outcome = FAILURE
            if exc_info is None:
                if check(example.want, got, self.optionflags):
                    outcome = SUCCESS
            else:
                formatted = traceback.format_exception_only(*exc_info[:2])
                if issubclass(exc_info[0], SyntaxError):
                    # keep only the "SyntaxError: ..." line(s), not the
                    # source excerpt
                    exc_type = exc_info[0]
                    prefixes = (f"{exc_type.__qualname__}:",
                                f"{exc_type.__module__}.{exc_type.__qualname__}:")
                    for start, line in enumerate(formatted):
                        if line.startswith(prefixes):
                            formatted = formatted[start:]
                            break
                    exc_msg = ''.join(formatted)
                else:
                    exc_msg = ''.join(formatted)
                    # the message proper starts with the "Type: ..." line
                    header = traceback.format_exception_only(exc_info[0], exc_info[1])[0]
                    exc_msg = exc_msg[exc_msg.index(header):] if header in exc_msg else exc_msg
                if not quiet:
                    got += _exception_traceback(exc_info)
                if example.exc_msg is None:
                    outcome = BOOM
                elif check(example.exc_msg, exc_msg, self.optionflags):
                    outcome = SUCCESS
                elif self.optionflags & IGNORE_EXCEPTION_DETAIL:
                    if check(_strip_exception_details(example.exc_msg),
                             _strip_exception_details(exc_msg),
                             self.optionflags):
                        outcome = SUCCESS

            if outcome is SUCCESS:
                if not quiet:
                    self.report_success(out, test, example, got)
                self._example_passed(test, index, example)
            elif outcome is FAILURE:
                if not quiet:
                    self.report_failure(out, test, example, got)
                failures += 1
                self._example_failed(test, index, example, got)
            else:
                if not quiet:
                    self.report_unexpected_exception(out, test, example, exc_info)
                failures += 1
                self._example_raised(test, index, example, exc_info)

            if failures and self.optionflags & FAIL_FAST:
                break

        self.optionflags = original
        self._record_outcome(test, failures, tries, skips)
        return TestResults(failures, tries, skipped=skips)

    # hooks used by the unittest integration
    def _example_skipped(self, test, index, example):
        pass

    def _example_passed(self, test, index, example):
        pass

    def _example_failed(self, test, index, example, got):
        pass

    def _example_raised(self, test, index, example, exc_info):
        pass

    def _record_outcome(self, test, failures, tries, skips):
        previous = self._stats.get(test.name, (0, 0, 0))
        self._stats[test.name] = (previous[0] + failures, previous[1] + tries,
                                  previous[2] + skips)
        self.failures += failures
        self.tries += tries
        self.skips += skips

    _SOURCE_NAME_RE = re.compile(r'<doctest (?P<name>.+)\[(?P<examplenum>\d+)\]>$')

    def _patched_getlines(self, filename, module_globals=None):
        m = self._SOURCE_NAME_RE.match(filename)
        if m and m.group('name') == self.test.name:
            example = self.test.examples[int(m.group('examplenum'))]
            return example.source.splitlines(keepends=True)
        return self._saved_getlines(filename, module_globals)

    def run(self, test, compileflags=None, out=None, clear_globs=True):
        """Run the examples of `test`; returns TestResults(failed, attempted).

        compileflags defaults to the __future__ flags found in test.globs.
        `out` receives the report text (default: sys.stdout.write).  The
        namespace test.globs is cleared afterwards unless clear_globs is
        false."""
        self.test = test
        if compileflags is None:
            compileflags = _extract_future_flags(test.globs)

        real_stdout = sys.stdout
        if out is None:
            encoding = real_stdout.encoding
            if encoding is None or encoding.lower() == 'utf-8':
                out = real_stdout.write
            else:
                def out(text):
                    text = str(text.encode(encoding, 'backslashreplace'), encoding)
                    real_stdout.write(text)
        sys.stdout = self._fakeout

        # pdb.set_trace() inside examples must talk to the real stdout
        try:
            import pdb
        except ImportError:
            pdb = None
        if pdb is not None:
            saved_set_trace = pdb.set_trace
            self.debugger = _OutputRedirectingPdb(real_stdout)
            self.debugger.reset()
            pdb.set_trace = self.debugger.set_trace
        else:
            self.debugger = _NoDebugger()

        self._saved_getlines = linecache.getlines
        linecache.getlines = self._patched_getlines

        saved_displayhook = sys.displayhook
        sys.displayhook = sys.__displayhook__
        saved_flags = self.optionflags
        try:
            return self._run_examples(test, compileflags, out)
        finally:
            sys.stdout = real_stdout
            if pdb is not None:
                pdb.set_trace = saved_set_trace
            linecache.getlines = self._saved_getlines
            sys.displayhook = saved_displayhook
            self.optionflags = saved_flags
            if clear_globs:
                test.globs.clear()
                import builtins
                builtins._ = None

    # ---- summaries

    def summarize(self, verbose=None):
        """Print a summary of all tests run by this runner and return
        TestResults(failed, attempted)."""
        if verbose is None:
            verbose = self._verbose

        empty, passed, failed = [], [], []
        total_tries = total_failures = total_skips = 0
        for name, (failures, tries, skips) in self._stats.items():
            total_tries += tries
            total_failures += failures
            total_skips += skips
            if tries == 0:
                empty.append(name)
            elif failures == 0:
                passed.append((name, tries))
            else:
                failed.append((name, (failures, tries, skips)))

        def items(count):
            return f"{count} item{'' if count == 1 else 's'}"

        if verbose:
            if empty:
                print(f"{items(len(empty))} had no tests:")
                for name in sorted(empty):
                    print(f"    {name}")
            if passed:
                print(f"{items(len(passed))} passed all tests:")
                for name, count in sorted(passed):
                    print(f" {count:3d} test{'' if count == 1 else 's'} in {name}")
        if failed:
            print(self.DIVIDER)
            print(f"{items(len(failed))} had failures:")
            for name, (failures, tries, skips) in sorted(failed):
                print(f" {failures:3d} of {tries:3d} in {name}")
        if verbose:
            print(f"{total_tries} test{'' if total_tries == 1 else 's'} "
                  f"in {items(len(self._stats))}.")
            summary = f"{total_tries - total_failures} passed"
            if total_failures:
                summary += f", {total_failures} failed"
            if total_skips:
                summary += f", {total_skips} skipped"
            print(summary + ".")
        if total_failures:
            message = (f"***Test Failed*** {total_failures} "
                       f"failure{'' if total_failures == 1 else 's'}")
            if total_skips:
                message += (f" and {total_skips} skipped "
                            f"test{'' if total_skips == 1 else 's'}")
            print(message + ".")
        elif verbose:
            print("Test passed.")
        return TestResults(total_failures, total_tries, skipped=total_skips)

    def merge(self, other):
        """Add the statistics of another runner to this one."""
        for name, (failures, tries, skips) in other._stats.items():
            previous = self._stats.get(name, (0, 0, 0))
            self._stats[name] = (previous[0] + failures, previous[1] + tries,
                                 previous[2] + skips)
        self.failures += other.failures
        self.tries += other.tries
        self.skips += other.skips


class _NoDebugger:
    def set_continue(self):
        pass


def _make_redirecting_pdb():
    try:
        import pdb
    except ImportError:
        return None

    class _RedirectingPdb(pdb.Pdb):
        """pdb.Pdb that temporarily restores the real stdout so that the
        debugger works inside examples whose output is being captured."""

        def __init__(self, out):
            self.__out = out
            self.__debugger_used = False
            pdb.Pdb.__init__(self, stdout=out, nosigint=True)
            self.use_rawinput = 1

        def set_trace(self, frame=None, *, commands=None):
            self.__debugger_used = True
            if frame is None:
                frame = sys._getframe().f_back
            pdb.Pdb.set_trace(self, frame, commands=commands)

        def set_continue(self):
            # only switch off tracing if the debugger was actually used
            if self.__debugger_used:
                pdb.Pdb.set_continue(self)

        def trace_dispatch(self, *args):
            captured = sys.stdout
            sys.stdout = self.__out
            try:
                return pdb.Pdb.trace_dispatch(self, *args)
            finally:
                sys.stdout = captured

    return _RedirectingPdb


def _OutputRedirectingPdb(out):
    cls = _make_redirecting_pdb()
    if cls is None:
        return _NoDebugger()
    return cls(out)


# ---------------------------------------------------------------- debugging runner

class DocTestFailure(Exception):
    """Raised by DebugRunner when an example produced the wrong output.

    Attributes: test, example, got."""

    def __init__(self, test, example, got):
        self.test = test
        self.example = example
        self.got = got

    def __str__(self):
        return str(self.test)


class UnexpectedException(Exception):
    """Raised by DebugRunner when an example raised an exception.

    Attributes: test, example, exc_info."""

    def __init__(self, test, example, exc_info):
        self.test = test
        self.example = example
        self.exc_info = exc_info

    def __str__(self):
        return str(self.test)


class DebugRunner(DocTestRunner):
    """A DocTestRunner that raises at the first problem (DocTestFailure or
    UnexpectedException) instead of reporting it, so that it can be
    debugged; the test namespace is then not cleared."""

    def run(self, test, compileflags=None, out=None, clear_globs=True):
        result = DocTestRunner.run(self, test, compileflags, out, False)
        if clear_globs:
            test.globs.clear()
        return result

    def report_unexpected_exception(self, out, test, example, exc_info):
        raise UnexpectedException(test, example, exc_info)

    def report_failure(self, out, test, example, got):
        raise DocTestFailure(test, example, got)


# ---------------------------------------------------------------- convenience functions

master = None


def testmod(m=None, name=None, globs=None, verbose=None,
            report=True, optionflags=0, extraglobs=None,
            raise_on_error=False, exclude_empty=False):
    """Test the examples in the docstrings of module m (default __main__),
    including m.__test__.  Returns TestResults(failed, attempted).

    name: module name used in reports; globs/extraglobs: namespace for the
    examples (default: the module's globals); verbose: print every example;
    report: print a summary; optionflags: comparison/reporting flags;
    raise_on_error: stop at the first problem with an exception;
    exclude_empty: skip objects without docstrings."""
    global master
    if m is None:
        m = sys.modules.get('__main__')
    if not inspect.ismodule(m):
        raise TypeError("testmod: module required; %r" % (m,))
    if name is None:
        name = m.__name__

    finder = DocTestFinder(exclude_empty=exclude_empty)
    if raise_on_error:
        runner = DebugRunner(verbose=verbose, optionflags=optionflags)
    else:
        runner = DocTestRunner(verbose=verbose, optionflags=optionflags)
    for test in finder.find(m, name, globs=globs, extraglobs=extraglobs):
        runner.run(test)
    if report:
        runner.summarize()
    if master is None:
        master = runner
    else:
        master.merge(runner)
    return TestResults(runner.failures, runner.tries, skipped=runner.skips)


def testfile(filename, module_relative=True, name=None, package=None,
             globs=None, verbose=None, report=True, optionflags=0,
             extraglobs=None, raise_on_error=False, parser=DocTestParser(),
             encoding=None):
    """Test the examples in a text file; returns TestResults.

    With module_relative=True the '/'-separated path is relative to the
    calling module (or to `package`); otherwise it is an OS path.  The
    other arguments are as for testmod(); `encoding` defaults to UTF-8."""
    global master
    if package and not module_relative:
        raise ValueError("Package may only be specified for module-"
                         "relative paths.")
    text, filename = _load_testfile(filename, package, module_relative,
                                    encoding or "utf-8")
    if name is None:
        name = os.path.basename(filename)
    globs = {} if globs is None else globs.copy()
    if extraglobs is not None:
        globs.update(extraglobs)
    if '__name__' not in globs:
        globs['__name__'] = '__main__'
    if raise_on_error:
        runner = DebugRunner(verbose=verbose, optionflags=optionflags)
    else:
        runner = DocTestRunner(verbose=verbose, optionflags=optionflags)
    test = parser.get_doctest(text, globs, name, filename, 0)
    runner.run(test)
    if report:
        runner.summarize()
    if master is None:
        master = runner
    else:
        master.merge(runner)
    return TestResults(runner.failures, runner.tries, skipped=runner.skips)


def run_docstring_examples(f, globs, verbose=False, name="NoName",
                           compileflags=None, optionflags=0):
    """Run the examples in the docstring of `f` (not recursively) in a copy
    of `globs`."""
    finder = DocTestFinder(verbose=verbose, recurse=False)
    runner = DocTestRunner(verbose=verbose, optionflags=optionflags)
    for test in finder.find(f, name, globs=globs):
        runner.run(test, compileflags=compileflags)


# ---------------------------------------------------------------- unittest integration

_unittest_reportflags = 0


def set_unittest_reportflags(flags):
    """Set the reporting flags used by DocTestSuite/DocFileSuite tests that
    were created without any; returns the previous value."""
    global _unittest_reportflags
    if (flags & REPORTING_FLAGS) != flags:
        raise ValueError("Only reporting flags allowed", flags)
    previous = _unittest_reportflags
    _unittest_reportflags = flags
    return previous


class _CollectingRunner(DocTestRunner):
    """Runner that records per-example outcomes for DocTestCase."""

    def __init__(self, *args, **kwargs):
        DocTestRunner.__init__(self, *args, **kwargs)
        self.outcomes = []

    def _example_skipped(self, test, index, example):
        self.outcomes.append(('skip', index, example, None))

    def _example_failed(self, test, index, example, got):
        message = ('Failed example:\n' + _indent(example.source) +
                   self._checker.output_difference(example, got, self.optionflags))
        self.outcomes.append(('fail', index, example, message))

    def _example_raised(self, test, index, example, exc_info):
        self.outcomes.append(('error', index, example, exc_info))

    def report_start(self, out, test, example):
        pass

    def report_success(self, out, test, example, got):
        pass

    def report_failure(self, out, test, example, got):
        pass

    def report_unexpected_exception(self, out, test, example, exc_info):
        pass


def _with_doctest_frame(exc, test, example, tb_next):
    """Attach a traceback whose first frame points at the example's line in
    the doctest source (followed by `tb_next`)."""
    if not test.filename or test.lineno is None or example.lineno is None:
        exc.__traceback__ = tb_next
        return exc
    lineno = test.lineno + example.lineno + 1
    try:
        code = compile('\n' * (lineno - 1) + 'raise __exc__',
                       test.filename, 'exec').replace(co_name=test.name)
        try:
            # no column information: the frame shows the example line only
            import marshal
            code = marshal._loads_without_positions(marshal.dumps(code))
        except (ImportError, AttributeError):
            pass
        try:
            exec(code, {'__exc__': exc})
        except BaseException as raised:
            here = raised.__traceback__
        synthetic = here.tb_next
        while synthetic is not None and synthetic.tb_frame.f_code is not code:
            synthetic = synthetic.tb_next
        if synthetic is None:
            raise LookupError
        exc.__traceback__ = types.TracebackType(tb_next, synthetic.tb_frame,
                                                synthetic.tb_lasti,
                                                synthetic.tb_lineno)
    except Exception:
        exc.__traceback__ = tb_next
    return exc


class DocTestCase(unittest.TestCase):
    """A unittest test case running the examples of one DocTest; each
    example is reported as a subtest."""

    def __init__(self, test, optionflags=0, setUp=None, tearDown=None,
                 checker=None):
        unittest.TestCase.__init__(self)
        self._dt_optionflags = optionflags
        self._dt_checker = checker
        self._dt_test = test
        self._dt_setUp = setUp
        self._dt_tearDown = tearDown

    def setUp(self):
        test = self._dt_test
        self._dt_globs = test.globs.copy()
        if self._dt_setUp is not None:
            self._dt_setUp(test)

    def tearDown(self):
        test = self._dt_test
        if self._dt_tearDown is not None:
            self._dt_tearDown(test)
        # restore the namespace for the next run of this test
        test.globs.clear()
        test.globs.update(self._dt_globs)

    def _effective_flags(self):
        flags = self._dt_optionflags
        if not flags & REPORTING_FLAGS:
            flags |= _unittest_reportflags
        return flags

    def runTest(self):
        test = self._dt_test
        runner = _CollectingRunner(optionflags=self._effective_flags(),
                                   checker=self._dt_checker, verbose=False)
        saved_stdout = sys.stdout
        captured = StringIO()
        try:
            runner.DIVIDER = "-" * 70
            runner.run(test, out=captured.write, clear_globs=False)
        finally:
            sys.stdout = saved_stdout

        examples = [e for e in test.examples]
        skipped = [o for o in runner.outcomes if o[0] == 'skip']
        for kind, index, example, detail in runner.outcomes:
            with self.subTest(index):
                if kind == 'skip':
                    self.skipTest('example skipped')
                elif kind == 'fail':
                    raise _with_doctest_frame(self.failureException(detail.removesuffix("\n")),
                                              test, example, None)
                else:
                    exc = detail[1]
                    raise _with_doctest_frame(exc, test, example, detail[2])
        if examples and len(skipped) == len(examples):
            self.skipTest('all examples were skipped')

    def format_failure(self, err):
        test = self._dt_test
        lineno = 'unknown line number' if test.lineno is None else '%s' % test.lineno
        lname = test.name.split('.')[-1]
        return ('Failed doctest test for %s\n'
                '  File "%s", line %s, in %s\n\n%s'
                % (test.name, test.filename, lineno, lname, err))

    def debug(self):
        """Run the test without catching problems (DebugRunner)."""
        self.setUp()
        runner = DebugRunner(optionflags=self._dt_optionflags,
                             checker=self._dt_checker, verbose=False)
        runner.run(self._dt_test, clear_globs=False)
        self.tearDown()

    def id(self):
        return self._dt_test.name

    def __eq__(self, other):
        if type(self) is not type(other):
            return NotImplemented
        return (self._dt_test == other._dt_test and
                self._dt_optionflags == other._dt_optionflags and
                self._dt_setUp == other._dt_setUp and
                self._dt_tearDown == other._dt_tearDown and
                self._dt_checker == other._dt_checker)

    def __hash__(self):
        return hash((self._dt_optionflags, self._dt_setUp, self._dt_tearDown,
                     self._dt_checker))

    def __repr__(self):
        name = self._dt_test.name.split('.')
        return "%s (%s)" % (name[-1], '.'.join(name[:-1]))

    __str__ = object.__str__

    def shortDescription(self):
        return "Doctest: " + self._dt_test.name


DocTestCase.__str__ = DocTestCase.__repr__


class SkipDocTestCase(DocTestCase):
    """Stands in for a module whose docstrings were stripped (-OO)."""

    def __init__(self, module):
        self.module = module
        DocTestCase.__init__(self, None)

    def setUp(self):
        self.skipTest("DocTestSuite will not work with -O2 and above")

    def test_skip(self):
        pass

    def shortDescription(self):
        return "Skipping tests from %s" % self.module.__name__

    __str__ = shortDescription


class _DocTestSuite(unittest.TestSuite):
    def _removeTestAtIndex(self, index):
        # keep the tests: the suite may be run several times
        pass


def DocTestSuite(module=None, globs=None, extraglobs=None, test_finder=None,
                 **options):
    """A unittest suite with one test case per docstring with examples in
    `module` (a module or module name; default: the caller's module).

    globs, extraglobs: namespace for the examples; test_finder: a
    DocTestFinder; other keyword arguments (setUp, tearDown, checker,
    optionflags) are passed to DocTestCase."""
    if test_finder is None:
        test_finder = DocTestFinder()
    module = _normalize_module(module)
    tests = test_finder.find(module, globs=globs, extraglobs=extraglobs)
    if not tests and sys.flags.optimize >= 2:
        suite = _DocTestSuite()
        suite.addTest(SkipDocTestCase(module))
        return suite
    tests.sort()
    suite = _DocTestSuite()
    for test in tests:
        if not test.examples:
            continue
        if not test.filename:
            filename = module.__file__
            if filename.endswith(".pyc"):
                filename = filename[:-1]
            test.filename = filename
        suite.addTest(DocTestCase(test, **options))
    return suite


class DocFileCase(DocTestCase):
    """DocTestCase for a doctest text file."""

    def id(self):
        return '_'.join(self._dt_test.name.split('.'))

    def __repr__(self):
        return self._dt_test.filename

    __str__ = __repr__

    def format_failure(self, err):
        return ('Failed doctest test for %s\n  File "%s", line 0\n\n%s'
                % (self._dt_test.name, self._dt_test.filename, err))


def DocFileTest(path, module_relative=True, package=None,
                globs=None, parser=DocTestParser(),
                encoding=None, **options):
    globs = {} if globs is None else globs.copy()
    if package and not module_relative:
        raise ValueError("Package may only be specified for module-"
                         "relative paths.")
    doc, path = _load_testfile(path, package, module_relative,
                               encoding or "utf-8")
    if "__file__" not in globs:
        globs["__file__"] = path
    name = os.path.basename(path)
    test = parser.get_doctest(doc, globs, name, path, 0)
    return DocFileCase(test, **options)


def DocFileSuite(*paths, **kw):
    """A unittest suite with one test case per doctest text file.

    Keyword arguments: module_relative, package, setUp, tearDown, globs,
    optionflags, parser, encoding (as for testfile() and DocTestSuite())."""
    suite = _DocTestSuite()
    if kw.get('module_relative', True):
        kw['package'] = _normalize_module(kw.get('package'))
    for path in paths:
        suite.addTest(DocFileTest(path, **kw))
    return suite


# ---------------------------------------------------------------- debugging support

def script_from_examples(s):
    r"""Turn a doctest text into a Python script: examples become code,
    expected output and other text become comments."""
    output = []
    for piece in DocTestParser().parse(s):
        if isinstance(piece, Example):
            output.append(piece.source[:-1])
            want = piece.want
            if want:
                output.append('# Expected:')
                output += ['## ' + line for line in want.split('\n')[:-1]]
        else:
            output += [_comment_line(line) for line in piece.split('\n')[:-1]]
    # drop leading and trailing comment-only lines
    while output and output[-1] == '#':
        output.pop()
    while output and output[0] == '#':
        output.pop(0)
    return '\n'.join(output) + '\n'


def testsource(module, name):
    """The script_from_examples() of the docstring of `name` in `module`."""
    module = _normalize_module(module)
    tests = DocTestFinder().find(module)
    matching = [t for t in tests if t.name == name]
    if not matching:
        raise ValueError(name, "not found in tests")
    return script_from_examples(matching[0].docstring)


def debug_src(src, pm=False, globs=None):
    """Debug the examples in the string `src`."""
    debug_script(script_from_examples(src), pm, globs)


def debug_script(src, pm=False, globs=None):
    """Run the script `src` under pdb (post-mortem if pm is true)."""
    import pdb
    globs = {} if globs is None else globs.copy()
    if pm:
        try:
            exec(src, globs, globs)
        except:
            print(sys.exc_info()[1])
            pdb.post_mortem(sys.exc_info()[2])
    else:
        pdb.Pdb(nosigint=True).run("exec(%r)" % src, globs, globs)


def debug(module, name, pm=False):
    """Debug the examples in the docstring of `name` in `module`."""
    module = _normalize_module(module)
    debug_script(testsource(module, name), pm, module.__dict__)


# ---------------------------------------------------------------- command line

def _test():
    import argparse

    parser = argparse.ArgumentParser(description="doctest runner", color=True)
    parser.add_argument('-v', '--verbose', action='store_true', default=False,
                        help='print very verbose output for all tests')
    parser.add_argument('-o', '--option', action='append',
                        choices=OPTIONFLAGS_BY_NAME.keys(), default=[],
                        help=('specify a doctest option flag to apply'
                              ' to the test run; may be specified more'
                              ' than once to apply multiple options'))
    parser.add_argument('-f', '--fail-fast', action='store_true',
                        help=('stop running tests after first failure (this'
                              ' is a shorthand for -o FAIL_FAST, and is'
                              ' in addition to any other -o options)'))
    parser.add_argument('file', nargs='+',
                        help='file containing the tests to run')
    args = parser.parse_args()
    testfiles = args.file
    verbose = args.verbose
    options = 0
    for option in args.option:
        options |= OPTIONFLAGS_BY_NAME[option]
    if args.fail_fast:
        options |= FAIL_FAST
    for filename in testfiles:
        if filename.endswith(".py"):
            # a module: import it from its directory
            dirname, filename = os.path.split(filename)
            sys.path.insert(0, dirname)
            module = __import__(filename[:-3])
            del sys.path[0]
            failures, _ = testmod(module, verbose=verbose, optionflags=options)
        else:
            failures, _ = testfile(filename, module_relative=False,
                                   verbose=verbose, optionflags=options)
        if failures:
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(_test())
