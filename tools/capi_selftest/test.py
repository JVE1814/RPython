"""Driver for capi_selftest.c (run via tools/capi_selftest/run.sh)."""
import capi_selftest as m

fails = 0


def check(name, got, exp):
    global fails
    if got != exp:
        fails += 1
        print("FAIL", name, repr(got), "!=", repr(exp))
    else:
        print("ok  ", name)


check("constants", (m.ANSWER, m.NAME), (42, "selftest"))
check("build", m.build(), ((1, -2, "ab"), [None], {"k": 1.5}, 7))
check("parse", m.parse(1), (1, "dflt", 0.0, None, 0))
check("parse kw", m.parse(1, None, 2.5, o="x", p=[1]), (1, None, 2.5, "x", 1))
try:
    m.parse()
except TypeError as e:
    check("parse missing", str(e), "parse() missing required argument 'i' (pos 1)")
try:
    m.parse(1, q=2)
except TypeError as e:
    check("parse bad kw", str(e), "parse() got an unexpected keyword argument 'q'")
try:
    m.err_format([1])
except ValueError as e:
    check("err_format", str(e), "bad value [1] (50%)")
check("call_method", m.call_method("abc"), "ABC")
check("dict_iter", m.dict_iter({"a": 1, "b": 2}), [("a", 1), ("b", 2)])
r = m.threads()
check("threads", (len(r), sorted(r)[:3], sum(r)), (400, [0, 0, 0], 4 * 4950))
check("capsule", m.capsule(), 42)
check("unicode", m.unicode_ops(), (11, ["héllo", "wörld"], "héllo-wörld", "héllo wörld", 0xE9))
check("vectorcall", m.vectorcall(lambda *a, sep: sep.join(a)), "a")
check("Error", (issubclass(m.Error, ValueError), m.Error.__doc__, m.Error.__module__), (True, "custom error", "capi_selftest"))

V = m.Vec
v = V(2.0, payload="p")
check("Vec repr", repr(v), "Vec(2.0, payload='p')")
check("Vec attrs", (v.value, v.payload, v.double), (2.0, "p", 4.0))
check("Vec add", (v + V(1.5)).value, 3.5)
check("Vec float", (float(v), bool(V())), (2.0, False))
v.scale(3)
check("Vec scale", v.value, 6.0)
check("Vec buffer", m.buffer_sum(v), sum(b"0123456789abcdef"))
check("Vec doc", V.__doc__, "A test vector.")
check("Vec text_signature", V.__text_signature__, "(value=0.0, payload=None)")
check("bytes buffer", m.buffer_sum(b"\x01\x02\x03"), 6)


class Sub(V):
    def __init__(self, *a, **k):
        self.extra = 1


s = Sub(5.0)
check("Vec subclass", (s.value, s.extra, isinstance(s, V), type(s).__mro__[1] is V), (5.0, 1, True, True))
v.payload = v  # cycle through the payload
del v

C = m.Counter
c = C(1)
check("Counter iter", list(c), [1, 2])
check("Counter call", c(), 30)
check("Counter module state", c.module_state(), 1234)
check("Counter type", (C.__module__, C.__name__, C.__qualname__, C.__doc__), ("capi_selftest", "Counter", "Counter", "a counter"))
try:
    C.x = 1
    check("Counter immutable", "no error", "TypeError")
except TypeError:
    check("Counter immutable", True, True)


class C2(C):
    pass


check("Counter subclass", list(C2()), [0, 1, 2])
print("fails:", fails)
