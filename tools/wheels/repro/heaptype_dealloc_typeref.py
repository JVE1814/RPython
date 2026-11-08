# Root cause (multidict's istr): an extension heap type that subclasses a builtin and has
# its own tp_dealloc calls the builtin's tp_dealloc directly and then drops the type
# reference itself (Py_DECREF(Py_TYPE(self)), the C-API contract for heap types). Our
# builtin deallocs also dropped it, so every instance cost the type one reference until
# the type was freed (segfault in pickle a few hundred istr objects later). The same
# happened below an extension *static* type (lxml's exception classes): subtype_dealloc
# drops the reference there, and the builtin dealloc dropped it again.
# Builds a tiny extension with the interpreter's own headers (needs cc).
import os, subprocess, sys, sysconfig, tempfile, importlib.util

SRC = r"""
#include <Python.h>
static void s_dealloc(PyObject *self) {
    PyTypeObject *tp = Py_TYPE(self);
    PyUnicode_Type.tp_dealloc(self);
    Py_DECREF(tp);
}
static void o_dealloc(PyObject *self) {
    PyTypeObject *tp = Py_TYPE(self);
    PyBaseObject_Type.tp_dealloc(self);
    Py_DECREF(tp);
}
static PyType_Slot s_slots[] = {{Py_tp_dealloc, s_dealloc}, {0, 0}};
static PyType_Spec s_spec = {"htd.S", 0, 0, Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE, s_slots};
static PyType_Slot o_slots[] = {{Py_tp_dealloc, o_dealloc}, {0, 0}};
static PyType_Spec o_spec = {"htd.O", sizeof(PyObject), 0, Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE, o_slots};
/* a static exception type with its own dealloc that calls its base's (lxml's
   `cdef class Error(Exception)` as Cython builds it) */
static void e_dealloc(PyObject *self) {
    PyObject_GC_UnTrack(self);
    ((PyTypeObject *)PyExc_Exception)->tp_dealloc(self);
}
static PyTypeObject E_Type = {
    PyVarObject_HEAD_INIT(NULL, 0)
    .tp_name = "htd.E",
    .tp_dealloc = e_dealloc,
    .tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_HAVE_GC,
};
static int exec_mod(PyObject *m) {
    E_Type.tp_base = (PyTypeObject *)PyExc_Exception;
    E_Type.tp_basicsize = ((PyTypeObject *)PyExc_Exception)->tp_basicsize;
    E_Type.tp_traverse = ((PyTypeObject *)PyExc_Exception)->tp_traverse;
    E_Type.tp_clear = ((PyTypeObject *)PyExc_Exception)->tp_clear;
    if (PyType_Ready(&E_Type) < 0 || PyModule_AddType(m, &E_Type) < 0) return -1;
    PyObject *base = PyTuple_Pack(1, (PyObject *)&PyUnicode_Type);
    PyObject *s = PyType_FromSpecWithBases(&s_spec, base);
    Py_DECREF(base);
    if (s == NULL || PyModule_Add(m, "S", s) < 0) return -1;
    return PyModule_Add(m, "O", PyType_FromSpec(&o_spec));
}
static PyModuleDef_Slot slots[] = {{Py_mod_exec, exec_mod},
    {Py_mod_gil, Py_MOD_GIL_NOT_USED}, {0, 0}};
static struct PyModuleDef def = {PyModuleDef_HEAD_INIT, "htd", 0, 0, 0, slots};
PyMODINIT_FUNC PyInit_htd(void) { return PyModuleDef_Init(&def); }
"""

d = tempfile.mkdtemp()
c = os.path.join(d, "htd.c")
so = os.path.join(d, "htd" + sysconfig.get_config_var("EXT_SUFFIX"))
with open(c, "w") as f:
    f.write(SRC)
subprocess.check_call(["cc", "-shared", "-fPIC", "-O1", "-I" + sysconfig.get_paths()["include"], c, "-o", so])
spec = importlib.util.spec_from_file_location("htd", so)
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


class Sub(m.S):  # a Python subclass: subtype_dealloc -> S's dealloc -> str's
    pass


class SubE(m.E):  # subtype_dealloc -> the static E's dealloc -> Exception's
    pass


for tp, make in ((m.S, lambda: m.S("abc")), (m.O, m.O), (Sub, lambda: Sub("x")),
                 (SubE, lambda: SubE("e"))):
    before = sys.getrefcount(tp)
    for _ in range(1000):
        x = make()
        del x
    after = sys.getrefcount(tp)
    assert after == before, (tp, before, after)
print("OK")
