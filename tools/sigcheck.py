"""List builtin callables whose signature cannot be introspected.

Run with *our* interpreter (see sigcheck.sh).  For every module checked it
walks the public callables (and the callables defined in the namespace of
each public class) and reports:

  MISSING  name       inspect.signature() fails although CPython has a signature
  EXTRA    name       expected to have no signature but one was found
  BADSIG   name       a text signature exists but does not parse (and should)

The "expected without signature" sets mirror the data in
spec/Lib/test/test_inspect/test_inspect.py (test_*_module_has_signatures),
which encodes where CPython itself has no usable signature.

Usage: sigcheck.py [--all] [--verbose] [module ...]
  --all   also walk every module in sys.builtin_module_names (private names
          included) in addition to the default list.
"""

import importlib
import inspect
import sys

# module -> (no_signature, unsupported_signature,
#            methods_no_signature, methods_unsupported_signature, good_exceptions)
EXPECT = {
    'builtins': (
        {'type', 'super', 'bytearray', 'bytes', 'dict', 'frozendict', 'int',
         'str', 'range', 'slice', 'dir', 'getattr', 'next', 'iter', 'vars',
         'min', 'max', '__build_class__'},
        {'anext'},
        {'dict': {'update'}, 'object': {'__class__'}},
        {'bytearray': {'count', 'endswith', 'find', 'hex', 'index', 'rfind',
                       'rindex', 'startswith'},
         'bytes': {'count', 'endswith', 'find', 'hex', 'index', 'rfind',
                   'rindex', 'startswith'},
         'dict': {'pop'},
         'memoryview': {'cast', 'hex'},
         'str': {'count', 'endswith', 'find', 'index', 'maketrans', 'rfind',
                 'rindex', 'startswith'}},
        ()),
    'types': (set(), {'CellType'},
              {'AsyncGeneratorType': {'athrow'}, 'CoroutineType': {'throw'},
               'GeneratorType': {'throw'},
               'FrameLocalsProxyType': {'setdefault', 'pop', 'get'}},
              {}, ()),
    'sys': ({'getsizeof', 'set_asyncgen_hooks', 'getobjects'}, set(), {}, {}, ()),
    'abc': (set(), set(), {}, {}, ()),
    'atexit': (set(), set(), {}, {}, ()),
    'codecs': (set(), set(), {'StreamReader': {'charbuffertype'}}, {}, ()),
    'collections': ({'OrderedDict', 'defaultdict'}, {'deque'},
                    {'OrderedDict': {'update'}},
                    {'deque': {'index'}, 'OrderedDict': {'pop'},
                     'UserString': {'maketrans'}}, ()),
    'collections.abc': (set(), set(), {}, {}, ()),
    'datetime': ({'tzinfo'}, {'timezone'}, {},
                 {'date': {'replace'}, 'time': {'replace'},
                  'datetime': {'replace', 'combine'}}, ()),
    'errno': (set(), set(), {}, {}, ()),
    'faulthandler': (set(), {'dump_traceback', 'dump_traceback_later', 'enable',
                             'dump_c_stack', 'register'}, {}, {}, ()),
    'functools': (set(), set(), {}, {}, ()),
    'gc': ({'set_threshold'}, set(), {}, {}, ()),
    'io': (set(), set(),
           {'BufferedRWPair': {'read', 'peek', 'read1', 'readinto',
                               'readinto1', 'write'}}, {}, ()),
    'itertools': ({'islice', 'repeat'}, set(), {}, {}, ()),
    'locale': (set(), set(), {}, {}, ()),
    'marshal': (set(), set(), {}, {}, ()),
    'operator': (set(), set(), {}, {}, ()),
    'os': (set(), {'chmod', 'utime', 'get_terminal_size', 'link',
                   'register_at_fork', 'startfile'}, {}, {}, ()),
    'pwd': (set(), set(), {}, {}, ()),
    're': (set(), set(), {'Match': {'group'}, 'Pattern': {'match'}}, {},
           {'error', 'PatternError'}),
    'signal': (set(), set(), {}, {}, ()),
    'stat': (set(), set(), {}, {}, ()),
    'struct': (set(), set(), {}, {}, ()),
    'string': (set(), set(), {}, {}, ()),
    'symtable': (set(), set(), {}, {}, ()),
    'sysconfig': (set(), set(), {}, {}, ()),
    'threading': (set(), set(), {}, {}, ()),
    '_thread': ({'RLock'}, set(), {}, {}, ()),
    'time': ({'asctime', 'ctime', 'get_clock_info', 'gmtime', 'localtime',
              'strftime', 'strptime', 'clock_getres', 'clock_settime',
              'clock_settime_ns', 'pthread_getcpuclockid'}, set(), {}, {}, ()),
    'tokenize': (set(), set(), {}, {}, ()),
    'tracemalloc': (set(), set(), {}, {}, ()),
    'typing': ({'ParamSpec', 'ParamSpecArgs', 'ParamSpecKwargs', 'Text',
                'TypeAliasType', 'TypeVar', 'TypeVarTuple'}, set(),
               {'Generic': {'__class_getitem__', '__init_subclass__'}},
               {'Text': {'count', 'find', 'index', 'rfind', 'rindex',
                         'startswith', 'endswith', 'maketrans'}}, ()),
    'warnings': (set(), {'warn_explicit'}, {}, {}, ()),
    'weakref': ({'ReferenceType', 'ref'}, set(), {}, {}, ()),
}

# Extra modules not covered by test_inspect but worth keeping clean.
EXTRA_MODULES = [
    'math', 'cmath', 'posix', '_io', '_operator', '_functools',
    '_collections', 'array', 'binascii', 'zlib', '_struct', '_json',
    '_pickle', '_random', '_bisect', '_heapq', 'select', '_socket',
    'unicodedata', '_codecs', '_weakref', '_abc', '_warnings', '_string',
    'fcntl', 'resource', 'mmap', 'termios', 'syslog', '_datetime',
    '_contextvars', '_queue', '_csv', '_sre', '_locale', '_stat',
    '_signal', '_tracemalloc', '_tokenize', '_symtable', '_opcode',
    '_posixsubprocess', '_bz2', '_lzma', '_hashlib', '_md5', '_sha1',
    '_sha2', '_sha3', '_blake2', 'grp', '_types', '_typing',
]


# Accelerator modules share the expectations of their public module.
ALIASES = {
    '_types': 'types', '_typing': 'typing', '_io': 'io', '_signal': 'signal',
    '_weakref': 'weakref', '_datetime': 'datetime', '_collections': 'collections',
    '_functools': 'functools', '_warnings': 'warnings', '_struct': 'struct',
    '_operator': 'operator', '_locale': 'locale', 'posix': 'os', '_stat': 'stat',
    '_codecs': 'codecs', '_tracemalloc': 'tracemalloc',
    '_symtable': 'symtable', '_abc': 'abc', '_sre': 're',
}

# Modules test_inspect walks: legacy doc lines are no excuse there.
STRICT_MODULES = set(name.split(".")[0] for name in EXPECT) | set(ALIASES)


def is_public(name):
    return not name.startswith('_') or name.startswith('__') and name.endswith('__')


def has_sig(obj):
    try:
        inspect.signature(obj)
        return True
    except ValueError:
        return False
    except TypeError:
        return False


def text_sig(obj):
    try:
        return obj.__text_signature__
    except Exception:
        return None


class Report:
    def __init__(self, verbose):
        self.verbose = verbose
        self.problems = []
        self.checked = 0

    def add(self, kind, name, detail=''):
        self.problems.append((kind, name, detail))

    def check_supported(self, qual, obj):
        self.checked += 1
        if not has_sig(obj):
            ts = text_sig(obj)
            if ts and ('<unrepresentable>' in ts or '[' in ts) and \
                    qual.split('.')[0] not in STRICT_MODULES:
                # Same as CPython: a default without a Python spelling, or
                # optional groups, make the signature unsupported by inspect.
                if self.verbose:
                    self.add('UNSUPP', qual, repr(ts))
            elif ts:
                self.add('BADSIG', qual, repr(ts))
            elif self.old_style(qual, obj):
                # A legacy "name(args) -> result" doc line: CPython has no
                # introspectable signature for these either.
                if self.verbose:
                    self.add('OLDSTYLE', qual)
            else:
                self.add('MISSING', qual)
        elif self.verbose:
            print('ok', qual, inspect.signature(obj))

    @staticmethod
    def old_style(qual, obj):
        if qual.split('.')[0] in STRICT_MODULES:
            return False
        try:
            doc = obj.__doc__
            name = obj.__name__
        except Exception:
            return False
        return isinstance(doc, str) and doc.startswith(name + '(')

    def check_none(self, qual, obj):
        self.checked += 1
        ts = text_sig(obj)
        if ts is not None or has_sig(obj):
            self.add('EXTRA', qual, 'expected no signature, got %r' % (ts,))

    def check_unsupported(self, qual, obj):
        self.checked += 1
        ts = text_sig(obj)
        if ts is None:
            self.add('MISSING', qual, '(expected an unparsable text signature)')
        elif has_sig(obj):
            self.add('EXTRA', qual, 'expected unparsable, got %r' % (ts,))


def check_class(rep, modname, cname, cls, no_sig, unsupported, all_names):
    for name in list(vars(cls)):
        obj = getattr(cls, name, None)
        if not callable(obj) or isinstance(obj, type):
            continue
        if name in no_sig or name in unsupported:
            continue
        rep.check_supported('%s.%s.%s' % (modname, cname, name), obj)
    for name in no_sig:
        if hasattr(cls, name):
            rep.check_none('%s.%s.%s' % (modname, cname, name), getattr(cls, name))
    for name in unsupported:
        if hasattr(cls, name):
            rep.check_unsupported('%s.%s.%s' % (modname, cname, name), getattr(cls, name))


def check_module(rep, modname, all_names=False):
    try:
        module = importlib.import_module(modname)
    except ImportError as e:
        rep.add('NOMODULE', modname, str(e))
        return
    no_sig, unsupported, mno, munsup, good_exc = EXPECT.get(
        ALIASES.get(modname, modname), (set(), set(), {}, {}, ()))
    no_sig = set(no_sig)
    ns = vars(module)
    try:
        names = set(module.__all__)
    except AttributeError:
        names = set(n for n in ns if all_names or is_public(n))
    for name, obj in sorted(ns.items()):
        if name not in names or not callable(obj):
            continue
        if (isinstance(obj, type) and issubclass(obj, BaseException)
                and name not in good_exc):
            no_sig.add(name)
        qual = '%s.%s' % (modname, name)
        if name not in no_sig and name not in unsupported:
            rep.check_supported(qual, obj)
        if isinstance(obj, type):
            check_class(rep, modname, name, obj, mno.get(name, ()),
                        munsup.get(name, ()), all_names)
    for name in no_sig:
        if name in ns and not (isinstance(ns[name], type)
                               and issubclass(ns[name], BaseException)):
            rep.check_none('%s.%s' % (modname, name), ns[name])
    for name in unsupported:
        if name in ns:
            rep.check_unsupported('%s.%s' % (modname, name), ns[name])


def main(argv):
    verbose = '--verbose' in argv
    walk_all = '--all' in argv
    mods = [a for a in argv if not a.startswith('--')]
    rep = Report(verbose)
    if not mods:
        mods = list(EXPECT) + EXTRA_MODULES
        if walk_all:
            for m in sorted(sys.builtin_module_names):
                if m not in mods:
                    mods.append(m)
    for m in mods:
        check_module(rep, m, all_names=walk_all and m not in EXPECT)
    seen = set()
    for kind, name, detail in rep.problems:
        if (kind, name) in seen:
            continue
        seen.add((kind, name))
        print('%-8s %s %s' % (kind, name, detail))
    n = len(seen)
    print('# checked %d callables, %d problems' % (rep.checked, n))
    return 1 if n else 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
