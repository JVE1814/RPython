# Three root causes found with cffi's own test suite (needs cc):
# 1. `from obj import missing` where sys.modules holds an object that is not a module
#    (cffi's `lib`) read a module dict out of it: segfault (import.rs cannot_import_name).
# 2. Extension modules were always loaded RTLD_LOCAL, ignoring sys.setdlopenflags():
#    cffi's verify() loads a library RTLD_GLOBAL so that a later one can use its symbols
#    ("undefined symbol" ImportError).
# 3. Call errors of a builtin function name it `module.qualname()` when its __module__ is
#    a str other than "builtins", also when it has a __self__ (cffi: "_mod.Lib.foo0() takes
#    no arguments (1 given)"); its repr names __self__'s type by tp_name.
import os, subprocess, sys, sysconfig, tempfile, importlib.util

# --- 1
class NotAModule:
    pass

sys.modules["_repro_not_a_module"] = NotAModule()
try:
    from _repro_not_a_module import missing  # noqa: F401
except ImportError as e:
    assert "cannot import name 'missing'" in str(e), e
else:
    raise AssertionError("no ImportError")

# --- 2 and 3
A = r"""
#include <Python.h>
int repro_shared_symbol(void) { return 42; }
static PyObject *noargs(PyObject *self, PyObject *unused) { Py_RETURN_NONE; }
static PyMethodDef fdef = {"foo0", noargs, METH_NOARGS, NULL};
static PyObject *make(PyObject *m, PyObject *self) {
    PyObject *name = PyUnicode_FromString("_repro_mod");
    PyObject *f = PyCFunction_NewEx(&fdef, self, name);
    Py_DECREF(name);
    return f;
}
static PyMethodDef methods[] = {{"make", make, METH_O, NULL}, {NULL}};
static PyModuleDef_Slot slots[] = {{Py_mod_gil, Py_MOD_GIL_NOT_USED}, {0, 0}};
static struct PyModuleDef def = {PyModuleDef_HEAD_INIT, "repro_a", 0, 0, methods, slots};
PyMODINIT_FUNC PyInit_repro_a(void) { return PyModuleDef_Init(&def); }
"""
B = r"""
#include <Python.h>
extern int repro_shared_symbol(void);
static PyObject *get(PyObject *m, PyObject *u) { return PyLong_FromLong(repro_shared_symbol()); }
static PyMethodDef methods[] = {{"get", get, METH_NOARGS, NULL}, {NULL}};
static PyModuleDef_Slot slots[] = {{Py_mod_gil, Py_MOD_GIL_NOT_USED}, {0, 0}};
static struct PyModuleDef def = {PyModuleDef_HEAD_INIT, "repro_b", 0, 0, methods, slots};
PyMODINIT_FUNC PyInit_repro_b(void) { return PyModuleDef_Init(&def); }
"""
d = tempfile.mkdtemp()
suffix = sysconfig.get_config_var("EXT_SUFFIX")
inc = "-I" + sysconfig.get_paths()["include"]


def build(name, src):
    c = os.path.join(d, name + ".c")
    so = os.path.join(d, name + suffix)
    with open(c, "w") as f:
        f.write(src)
    # repro_b leaves its symbol unresolved: it must come from repro_a at load time
    subprocess.check_call(["cc", "-shared", "-fPIC", "-O1", inc, c, "-o", so])
    return so


def load(name, so):
    spec = importlib.util.spec_from_file_location(name, so)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


a_so, b_so = build("repro_a", A), build("repro_b", B)
old = sys.getdlopenflags()
sys.setdlopenflags(os.RTLD_NOW | os.RTLD_GLOBAL)
try:
    a = load("repro_a", a_so)
finally:
    sys.setdlopenflags(old)
b = load("repro_b", b_so)
assert b.get() == 42


class Holder:
    pass


f = a.make(Holder())
try:
    f(1)
except TypeError as e:
    assert str(e) == "_repro_mod.Holder.foo0() takes no arguments (1 given)", e
assert repr(f).startswith("<built-in method foo0 of Holder object at"), repr(f)
print("OK")
