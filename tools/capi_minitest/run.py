"""Minimal harness to run spec/Lib/test/test_capi test modules before the real
`unittest` / `test.support` stdlib modules exist in this runtime.

    PYTHONPATH=<ext-dir> python3 tools/capi_minitest/run.py SPEC_LIB_TEST_DIR test_long test_set ...

It installs tiny stand-ins for `unittest` and `test.support` (just what the C-API tests
need) and reports per-test results. Temporary tooling: once Lib/unittest and
test.support work, use `python -m test test_capi` instead.
"""
import sys

SPEC = sys.argv[1]
NAMES = [n for n in sys.argv[2:] if n != "-v"]
VERBOSE = "-v" in sys.argv

ModuleType = type(sys)


class SkipTest(Exception):
    pass


class _AssertRaisesCtx:
    def __init__(self, tc, exc, regex=None):
        self.tc = tc
        self.expected = exc
        self.regex = regex
        self.exception = None

    def __enter__(self):
        return self

    def __exit__(self, et, ev, tb):
        if et is None:
            raise AssertionError("%s not raised" % getattr(self.expected, "__name__", self.expected))
        if not issubclass(et, self.expected):
            return False
        self.exception = ev
        if self.regex is not None and not _search(self.regex, str(ev)):
            raise AssertionError("%r does not match %r" % (self.regex, str(ev)))
        return True


class _AssertWarnsCtx:
    def __init__(self, tc, cat, regex=None):
        self.cat = cat
        self.regex = regex

    def __enter__(self):
        # record calls to warnings.warn directly (avoids depending on the
        # filters machinery of the warnings module)
        import warnings
        self._mod = warnings
        self._old = warnings.warn
        self.log = []
        log = self.log

        class _W:
            def __init__(self, message, category):
                self.message = message
                self.category = category

        def warn(message, category=None, stacklevel=1, source=None, **kw):
            if isinstance(message, Warning):
                category = type(message)
            log.append(_W(message, category or UserWarning))

        warnings.warn = warn
        return self

    def __exit__(self, et, ev, tb):
        self._mod.warn = self._old
        if et is not None:
            return False
        for w in self.log:
            if issubclass(w.category, self.cat) and (self.regex is None or _search(self.regex, str(w.message))):
                self.warning = w.message
                return False
        raise AssertionError("%s not triggered" % self.cat.__name__)


def _search(pattern, text):
    try:
        import re
        return re.search(pattern, text) is not None
    except ImportError:
        pass
    # crude fallback: drop regex escapes and anchors, compare literally
    p = pattern
    for a, b in (("\\(", "("), ("\\)", ")"), ("\\.", "."), ("\\[", "["), ("\\]", "]"), ("\\$", "$"), ("\\*", "*"), ("\\+", "+"), ("\\?", "?")):
        p = p.replace(a, b)
    if p.startswith("^"):
        p = p[1:]
    if p.endswith("$") and not p.endswith("\\$"):
        p = p[:-1]
    return p in text


class _SubTest:
    def __init__(self, tc, msg):
        self.tc = tc
        self.msg = msg

    def __enter__(self):
        return self

    def __exit__(self, et, ev, tb):
        if et is None:
            return False
        if issubclass(et, SkipTest):
            return True
        if issubclass(et, Exception):
            self.tc._subfailures.append("%s: %s: %s" % (self.msg, et.__name__, ev))
            return True
        return False


class TestCase:
    maxDiff = 80 * 8
    longMessage = True
    failureException = AssertionError

    def __init__(self, name="runTest"):
        self._name = name
        self._cleanups = []
        self._subfailures = []

    def setUp(self):
        pass

    def tearDown(self):
        pass

    @classmethod
    def setUpClass(cls):
        pass

    @classmethod
    def tearDownClass(cls):
        pass

    def addCleanup(self, f, *a, **k):
        self._cleanups.append((f, a, k))

    def enterContext(self, cm):
        r = cm.__enter__()
        self.addCleanup(cm.__exit__, None, None, None)
        return r

    def skipTest(self, reason):
        raise SkipTest(reason)

    def fail(self, msg=None):
        raise AssertionError(msg)

    def subTest(self, msg=None, **params):
        return _SubTest(self, "%s %s" % (msg, params))

    def _fmt(self, msg, std):
        return std if msg is None else "%s : %s" % (std, msg)

    def assertEqual(self, a, b, msg=None):
        if not a == b:
            raise AssertionError(self._fmt(msg, "%r != %r" % (a, b)))

    def assertNotEqual(self, a, b, msg=None):
        if a == b:
            raise AssertionError(self._fmt(msg, "%r == %r" % (a, b)))

    def assertTrue(self, x, msg=None):
        if not x:
            raise AssertionError(self._fmt(msg, "%r is not true" % (x,)))

    def assertFalse(self, x, msg=None):
        if x:
            raise AssertionError(self._fmt(msg, "%r is not false" % (x,)))

    def assertIs(self, a, b, msg=None):
        if a is not b:
            raise AssertionError(self._fmt(msg, "%r is not %r" % (a, b)))

    def assertIsNot(self, a, b, msg=None):
        if a is b:
            raise AssertionError(self._fmt(msg, "unexpectedly identical: %r" % (a,)))

    def assertIsNone(self, x, msg=None):
        self.assertIs(x, None, msg)

    def assertIsNotNone(self, x, msg=None):
        self.assertIsNot(x, None, msg)

    def assertIn(self, a, b, msg=None):
        if a not in b:
            raise AssertionError(self._fmt(msg, "%r not found in %r" % (a, b)))

    def assertNotIn(self, a, b, msg=None):
        if a in b:
            raise AssertionError(self._fmt(msg, "%r unexpectedly found in %r" % (a, b)))

    def assertIsInstance(self, a, t, msg=None):
        if not isinstance(a, t):
            raise AssertionError(self._fmt(msg, "%r is not an instance of %r" % (a, t)))

    def assertNotIsInstance(self, a, t, msg=None):
        if isinstance(a, t):
            raise AssertionError(self._fmt(msg, "%r is an instance of %r" % (a, t)))

    def assertGreater(self, a, b, msg=None):
        if not a > b:
            raise AssertionError(self._fmt(msg, "%r not greater than %r" % (a, b)))

    def assertGreaterEqual(self, a, b, msg=None):
        if not a >= b:
            raise AssertionError(self._fmt(msg, "%r not >= %r" % (a, b)))

    def assertLess(self, a, b, msg=None):
        if not a < b:
            raise AssertionError(self._fmt(msg, "%r not less than %r" % (a, b)))

    def assertLessEqual(self, a, b, msg=None):
        if not a <= b:
            raise AssertionError(self._fmt(msg, "%r not <= %r" % (a, b)))

    def assertAlmostEqual(self, a, b, places=7, msg=None, delta=None):
        if a == b:
            return
        if delta is not None:
            if abs(a - b) <= delta:
                return
        elif round(abs(a - b), places) == 0:
            return
        raise AssertionError(self._fmt(msg, "%r != %r within tolerance" % (a, b)))

    def assertCountEqual(self, a, b, msg=None):
        a, b = list(a), list(b)
        for x in a:
            if x in b:
                b.remove(x)
            else:
                raise AssertionError(self._fmt(msg, "element counts differ"))
        if b:
            raise AssertionError(self._fmt(msg, "element counts differ"))

    def assertSequenceEqual(self, a, b, msg=None, seq_type=None):
        self.assertEqual(list(a), list(b), msg)

    assertListEqual = assertTupleEqual = assertSequenceEqual

    def assertDictEqual(self, a, b, msg=None):
        self.assertEqual(a, b, msg)

    def assertRegex(self, text, regex, msg=None):
        if not _search(regex, text):
            raise AssertionError(self._fmt(msg, "%r does not match %r" % (regex, text)))

    def assertRaises(self, exc, *args, **kwargs):
        ctx = _AssertRaisesCtx(self, exc)
        if not args:
            return ctx
        f, args = args[0], args[1:]
        with ctx:
            f(*args, **kwargs)
        return ctx

    def assertRaisesRegex(self, exc, regex, *args, **kwargs):
        ctx = _AssertRaisesCtx(self, exc, regex)
        if not args:
            return ctx
        f, args = args[0], args[1:]
        with ctx:
            f(*args, **kwargs)
        return ctx

    def assertWarns(self, cat, *args, **kwargs):
        ctx = _AssertWarnsCtx(self, cat)
        if not args:
            return ctx
        f, args = args[0], args[1:]
        with ctx:
            f(*args, **kwargs)
        return ctx

    def assertWarnsRegex(self, cat, regex, *args, **kwargs):
        ctx = _AssertWarnsCtx(self, cat, regex)
        if not args:
            return ctx
        f, args = args[0], args[1:]
        with ctx:
            f(*args, **kwargs)
        return ctx


def skip(reason):
    def deco(f):
        def skipped(*a, **k):
            raise SkipTest(reason)
        if isinstance(f, type):
            f.__unittest_skip__ = reason
            return f
        return skipped
    return deco


def skipIf(cond, reason):
    return skip(reason) if cond else (lambda f: f)


def skipUnless(cond, reason):
    return skip(reason) if not cond else (lambda f: f)


def expectedFailure(f):
    def wrapper(*a, **k):
        try:
            f(*a, **k)
        except Exception:
            raise SkipTest("expected failure")
        raise AssertionError("unexpected success")
    return wrapper


def main(*a, **k):
    pass


unittest = ModuleType("unittest")
for _n in ("TestCase", "SkipTest", "skip", "skipIf", "skipUnless", "expectedFailure", "main"):
    setattr(unittest, _n, globals()[_n])
sys.modules["unittest"] = unittest

# ---------------------------------------------------------------- test.support


def _passthrough(*a, **k):
    if len(a) == 1 and callable(a[0]) and not k:
        return a[0]
    return lambda f: f


class _NullCtx:
    def __init__(self, *a, **k):
        pass

    def __enter__(self):
        return self

    def __exit__(self, *a):
        return False


class catch_unraisable_exception:
    def __init__(self):
        self.unraisable = None

    def _hook(self, u):
        self.unraisable = u

    def __enter__(self):
        self._old = sys.unraisablehook
        sys.unraisablehook = self._hook
        return self

    def __exit__(self, *a):
        sys.unraisablehook = self._old
        return False


class swap_attr:
    def __init__(self, obj, attr, new):
        self.obj, self.attr, self.new = obj, attr, new

    def __enter__(self):
        self.had = hasattr(self.obj, self.attr)
        self.old = getattr(self.obj, self.attr, None)
        setattr(self.obj, self.attr, self.new)
        return self.old

    def __exit__(self, *a):
        if self.had:
            setattr(self.obj, self.attr, self.old)
        else:
            delattr(self.obj, self.attr)
        return False


def _import_module(name, deleted=False, *, required_on=()):
    try:
        return __import__(name, fromlist=["*"]) if "." in name else __import__(name)
    except ImportError as e:
        raise SkipTest(str(e))


support = ModuleType("test.support")
support.Py_GIL_DISABLED = True
support.Py_DEBUG = False
support.verbose = 0
support.MISSING_C_DOCSTRINGS = False
support.TEST_MODULES_ENABLED = True
support.is_apple_mobile = False
support.is_emscripten = False
support.is_wasi = False
support.built_with_c_assertions = lambda: True
support.with_pymalloc = lambda: False
support.with_mimalloc = lambda: False
support.cpython_only = _passthrough
support.requires_limited_api = _passthrough
support.requires_resource = _passthrough
support.requires_subprocess = _passthrough
support.requires_gil_enabled = skip("requires the GIL")
support.skip_wasi_stack_overflow = _passthrough
support.skip_emscripten_stack_overflow = _passthrough
support.skip_if_sanitizer = _passthrough
support.force_not_colorized_test_class = _passthrough
support.expected_failure_if_gil_disabled = _passthrough
support.requires_IEEE_754 = _passthrough
support.bigmemtest = lambda *a, **k: (lambda f: (lambda self: f(self, 1 << 20)))
support.nomemtest = _passthrough
support.catch_unraisable_exception = catch_unraisable_exception
support.swap_attr = swap_attr
support.SuppressCrashReport = _NullCtx
support.adjust_int_max_str_digits = _NullCtx
support.subTests = _passthrough
support.gc_collect = lambda: None
support.check_impl_detail = lambda **k: True
support.import_module = _import_module
support.load_package_tests = lambda *a: None
import_helper = ModuleType("test.support.import_helper")
import_helper.import_module = _import_module
support.import_helper = import_helper

test_pkg = ModuleType("test")
test_pkg.__path__ = [SPEC]
test_pkg.support = support
capi_pkg = ModuleType("test.test_capi")
capi_pkg.__path__ = [SPEC + "/test_capi"]
test_pkg.test_capi = capi_pkg
sys.modules["test"] = test_pkg
sys.modules["test.support"] = support
sys.modules["test.support.import_helper"] = import_helper
sys.modules["test.test_capi"] = capi_pkg

# ---------------------------------------------------------------- runner

totals = {"ok": 0, "fail": 0, "error": 0, "skip": 0}
for name in NAMES:
    try:
        mod = __import__("test.test_capi." + name, fromlist=["*"])
    except SkipTest as e:
        print("MODULE SKIP", name, e)
        continue
    except BaseException as e:
        print("MODULE ERROR", name, type(e).__name__, e)
        totals["error"] += 1
        continue
    for cname in sorted(dir(mod)):
        cls = getattr(mod, cname)
        if not (isinstance(cls, type) and issubclass(cls, TestCase) and cls is not TestCase):
            continue
        if getattr(cls, "__unittest_skip__", None):
            continue
        try:
            cls.setUpClass()
        except BaseException as e:
            print("CLASS ERROR", name, cname, type(e).__name__, e)
            continue
        for mname in sorted(dir(cls)):
            if not mname.startswith("test"):
                continue
            meth = getattr(cls, mname)
            if not callable(meth):
                continue
            if VERBOSE:
                print("RUN", name, cname + "." + mname, flush=True)
            tc = cls(mname)
            status = "ok"
            detail = ""
            try:
                tc.setUp()
                try:
                    getattr(tc, mname)()
                finally:
                    tc.tearDown()
                    for f, a, k in reversed(tc._cleanups):
                        f(*a, **k)
                if tc._subfailures:
                    status, detail = "fail", "; ".join(tc._subfailures[:3])
            except SkipTest as e:
                status, detail = "skip", str(e)
            except AssertionError as e:
                status, detail = "fail", str(e)
            except BaseException as e:
                status, detail = "error", "%s: %s" % (type(e).__name__, e)
            totals[status] += 1
            if status != "ok":
                print(status.upper(), name, cname + "." + mname, "::", detail[:300])
print("TOTALS", totals)
