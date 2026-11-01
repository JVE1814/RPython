/* Self-test extension for the C API (written for this project): exercises the
   patterns real-world extensions (numpy, Cython output) rely on.
   Built by tools/capi_selftest/run.sh, driven by tools/capi_selftest/test.py. */
#define PY_SSIZE_T_CLEAN
#include "Python.h"
#include "structmember.h"
#include <pthread.h>

/* ------------------------------------------------------------------ static GC type */

typedef struct {
    PyObject_HEAD
    PyObject *payload;
    double value;
    Py_ssize_t shape[2];
    char data[16];
    PyObject *weakreflist;
} VecObject;

static int Vec_traverse(VecObject *self, visitproc visit, void *arg) {
    Py_VISIT(self->payload);
    return 0;
}
static int Vec_clear(VecObject *self) {
    Py_CLEAR(self->payload);
    return 0;
}
static void Vec_dealloc(VecObject *self) {
    PyObject_GC_UnTrack(self);
    if (self->weakreflist) PyObject_ClearWeakRefs((PyObject *)self);
    Vec_clear(self);
    Py_TYPE(self)->tp_free((PyObject *)self);
}
static PyObject *Vec_new(PyTypeObject *type, PyObject *args, PyObject *kw) {
    static char *kwlist[] = {"value", "payload", NULL};
    double value = 0.0;
    PyObject *payload = Py_None;
    if (!PyArg_ParseTupleAndKeywords(args, kw, "|dO:Vec", kwlist, &value, &payload))
        return NULL;
    VecObject *self = (VecObject *)type->tp_alloc(type, 0);
    if (self == NULL) return NULL;
    self->value = value;
    self->payload = Py_NewRef(payload);
    self->shape[0] = 4; self->shape[1] = 4;
    memcpy(self->data, "0123456789abcdef", 16);
    return (PyObject *)self;
}
static PyObject *Vec_add(PyObject *a, PyObject *b) {
    extern PyTypeObject Vec_Type;
    if (!PyObject_TypeCheck(a, &Vec_Type) || !PyObject_TypeCheck(b, &Vec_Type))
        Py_RETURN_NOTIMPLEMENTED;
    PyObject *args = Py_BuildValue("(d)", ((VecObject *)a)->value + ((VecObject *)b)->value);
    if (!args) return NULL;
    PyObject *r = PyObject_Call((PyObject *)Py_TYPE(a), args, NULL);
    Py_DECREF(args);
    return r;
}
static PyObject *Vec_float(PyObject *a) { return PyFloat_FromDouble(((VecObject *)a)->value); }
static int Vec_bool(PyObject *a) { return ((VecObject *)a)->value != 0.0; }
static PyObject *Vec_repr(VecObject *self) {
    return PyUnicode_FromFormat("Vec(%R, payload=%R)", PyFloat_FromDouble(self->value), self->payload);
}
static int Vec_getbuffer(PyObject *o, Py_buffer *view, int flags) {
    VecObject *self = (VecObject *)o;
    return PyBuffer_FillInfo(view, o, self->data, 16, 1, flags);
}
static PyObject *Vec_scale(VecObject *self, PyObject *const *args, Py_ssize_t nargs, PyObject *kwnames) {
    if (nargs != 1 || kwnames) {
        PyErr_SetString(PyExc_TypeError, "scale(factor)");
        return NULL;
    }
    double f = PyFloat_AsDouble(args[0]);
    if (f == -1.0 && PyErr_Occurred()) return NULL;
    self->value *= f;
    Py_RETURN_NONE;
}
static PyObject *Vec_get_double(VecObject *self, void *closure) {
    return PyFloat_FromDouble(self->value * 2);
}
static PyNumberMethods Vec_as_number = {
    .nb_add = Vec_add, .nb_float = Vec_float, .nb_bool = Vec_bool,
};
static PyBufferProcs Vec_as_buffer = { .bf_getbuffer = Vec_getbuffer };
static PyMethodDef Vec_methods[] = {
    {"scale", (PyCFunction)(void(*)(void))Vec_scale, METH_FASTCALL | METH_KEYWORDS, "scale in place"},
    {NULL}
};
static PyMemberDef Vec_members[] = {
    {"value", Py_T_DOUBLE, offsetof(VecObject, value), 0, "the value"},
    {"payload", Py_T_OBJECT_EX, offsetof(VecObject, payload), 0, NULL},
    {NULL}
};
static PyGetSetDef Vec_getset[] = {
    {"double", (getter)Vec_get_double, NULL, "twice", NULL},
    {NULL}
};
PyTypeObject Vec_Type = {
    PyVarObject_HEAD_INIT(NULL, 0)
    .tp_name = "capi_selftest.Vec",
    .tp_basicsize = sizeof(VecObject),
    .tp_dealloc = (destructor)Vec_dealloc,
    .tp_repr = (reprfunc)Vec_repr,
    .tp_as_number = &Vec_as_number,
    .tp_as_buffer = &Vec_as_buffer,
    .tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_HAVE_GC,
    .tp_doc = "Vec(value=0.0, payload=None)\n--\n\nA test vector.",
    .tp_traverse = (traverseproc)Vec_traverse,
    .tp_clear = (inquiry)Vec_clear,
    .tp_weaklistoffset = offsetof(VecObject, weakreflist),
    .tp_methods = Vec_methods,
    .tp_members = Vec_members,
    .tp_getset = Vec_getset,
    .tp_new = Vec_new,
};

/* ------------------------------------------------------------------ heap type from spec */

typedef struct {
    PyObject_HEAD
    long count;
} CounterObject;

static PyObject *Counter_new(PyTypeObject *type, PyObject *args, PyObject *kw) {
    CounterObject *self = (CounterObject *)type->tp_alloc(type, 0);
    if (self) self->count = 0;
    return (PyObject *)self;
}
static int Counter_init(CounterObject *self, PyObject *args, PyObject *kw) {
    long start = 0;
    if (!PyArg_ParseTuple(args, "|l", &start)) return -1;
    self->count = start;
    return 0;
}
static void Counter_dealloc(CounterObject *self) {
    PyTypeObject *tp = Py_TYPE(self);
    PyObject_GC_UnTrack(self);
    tp->tp_free(self);
    Py_DECREF(tp);
}
static int Counter_traverse(PyObject *self, visitproc visit, void *arg) {
    Py_VISIT(Py_TYPE(self));
    return 0;
}
static PyObject *Counter_iternext(CounterObject *self) {
    if (self->count >= 3) return NULL;
    return PyLong_FromLong(self->count++);
}
static PyObject *Counter_call(CounterObject *self, PyObject *args, PyObject *kw) {
    return PyLong_FromLong(self->count * 10);
}
static PyObject *Counter_module(PyObject *self, PyTypeObject *cls, PyObject *const *args, Py_ssize_t n, PyObject *kw) {
    PyObject *m = PyType_GetModule(cls);
    if (!m) return NULL;
    long *state = PyModule_GetState(m);
    return PyLong_FromLong(*state);
}
static PyMethodDef Counter_methods[] = {
    {"module_state", (PyCFunction)(void(*)(void))Counter_module, METH_METHOD | METH_FASTCALL | METH_KEYWORDS, NULL},
    {NULL}
};
static PyMemberDef Counter_members[] = {
    {"count", Py_T_LONG, offsetof(CounterObject, count), 0, NULL},
    {NULL}
};
static PyType_Slot Counter_slots[] = {
    {Py_tp_new, Counter_new},
    {Py_tp_init, Counter_init},
    {Py_tp_dealloc, Counter_dealloc},
    {Py_tp_traverse, Counter_traverse},
    {Py_tp_iter, PyObject_SelfIter},
    {Py_tp_iternext, Counter_iternext},
    {Py_tp_call, Counter_call},
    {Py_tp_methods, Counter_methods},
    {Py_tp_members, Counter_members},
    {Py_tp_doc, "a counter"},
    {0, NULL}
};
static PyType_Spec Counter_spec = {
    .name = "capi_selftest.Counter",
    .basicsize = sizeof(CounterObject),
    .flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_IMMUTABLETYPE,
    .slots = Counter_slots,
};

/* ------------------------------------------------------------------ module functions */

static PyObject *build(PyObject *self, PyObject *args) {
    return Py_BuildValue("(iis#)[O]{s:d}N", 1, -2, "abc", (Py_ssize_t)2, Py_None, "k", 1.5, PyLong_FromLong(7));
}
static PyObject *parse(PyObject *self, PyObject *args, PyObject *kw) {
    static char *kwlist[] = {"i", "s", "f", "o", "p", NULL};
    int i = 0; const char *s = "dflt"; double f = 0; PyObject *o = NULL; int p = 0;
    if (!PyArg_ParseTupleAndKeywords(args, kw, "i|zd$Op:parse", kwlist, &i, &s, &f, &o, &p))
        return NULL;
    return Py_BuildValue("(iz d O i)", i, s, f, o ? o : Py_None, p);
}
static PyObject *err_format(PyObject *self, PyObject *arg) {
    return PyErr_Format(PyExc_ValueError, "bad value %R (%d%%)", arg, 50);
}
static PyObject *call_method(PyObject *self, PyObject *args) {
    PyObject *o;
    if (!PyArg_ParseTuple(args, "O", &o)) return NULL;
    return PyObject_CallMethod(o, "upper", NULL);
}
static PyObject *dict_iter(PyObject *self, PyObject *d) {
    PyObject *k, *v, *r = PyList_New(0);
    Py_ssize_t pos = 0;
    Py_BEGIN_CRITICAL_SECTION(d);
    while (PyDict_Next(d, &pos, &k, &v)) {
        PyObject *t = PyTuple_Pack(2, k, v);
        PyList_Append(r, t);
        Py_DECREF(t);
    }
    Py_END_CRITICAL_SECTION();
    return r;
}
static void *thread_body(void *arg) {
    PyGILState_STATE st = PyGILState_Ensure();
    PyObject *l = (PyObject *)arg;
    for (int i = 0; i < 100; i++) {
        PyObject *x = PyLong_FromLong(i);
        Py_BEGIN_CRITICAL_SECTION(l);
        PyList_Append(l, x);
        Py_END_CRITICAL_SECTION();
        Py_DECREF(x);
    }
    PyGILState_Release(st);
    return NULL;
}
static PyObject *threads(PyObject *self, PyObject *noargs) {
    PyObject *l = PyList_New(0);
    pthread_t t[4];
    Py_BEGIN_ALLOW_THREADS
    for (int i = 0; i < 4; i++) pthread_create(&t[i], NULL, thread_body, l);
    for (int i = 0; i < 4; i++) pthread_join(t[i], NULL);
    Py_END_ALLOW_THREADS
    return l;
}
static void capsule_destructor(PyObject *cap) {
    int *p = PyCapsule_GetPointer(cap, "capi_selftest.cap");
    *p = 42;
}
static int destructed;
static PyObject *capsule(PyObject *self, PyObject *noargs) {
    PyObject *c = PyCapsule_New(&destructed, "capi_selftest.cap", capsule_destructor);
    if (!c) return NULL;
    int *p = PyCapsule_GetPointer(c, "capi_selftest.cap");
    Py_DECREF(c);
    return PyLong_FromLong(p == &destructed ? destructed : -1);
}
static PyObject *unicode_ops(PyObject *self, PyObject *noargs) {
    PyObject *s = PyUnicode_FromString("h\xc3\xa9llo w\xc3\xb6rld");
    Py_ssize_t n;
    const char *u = PyUnicode_AsUTF8AndSize(s, &n);
    PyObject *parts = PyUnicode_Split(s, NULL, -1);
    PyObject *joined = PyUnicode_Join(PyUnicode_FromString("-"), parts);
    PyObject *r = Py_BuildValue("(nNNs#i)", PyUnicode_GetLength(s), parts, joined, u, n,
                                (int)PyUnicode_READ_CHAR(s, 1));
    Py_DECREF(s);
    return r;
}
static PyObject *buffer_sum(PyObject *self, PyObject *o) {
    Py_buffer view;
    if (PyObject_GetBuffer(o, &view, PyBUF_SIMPLE) < 0) return NULL;
    long sum = 0;
    for (Py_ssize_t i = 0; i < view.len; i++) sum += ((unsigned char *)view.buf)[i];
    PyBuffer_Release(&view);
    return PyLong_FromLong(sum);
}
static PyObject *vectorcall(PyObject *self, PyObject *const *args, Py_ssize_t nargs) {
    if (nargs < 1) return NULL;
    PyObject *kwnames = Py_BuildValue("(s)", "sep");
    PyObject *argv[3] = {NULL, PyUnicode_FromString("a"), PyUnicode_FromString("|")};
    argv[0] = NULL;
    PyObject *r = PyObject_Vectorcall(args[0], argv + 1, 1 | PY_VECTORCALL_ARGUMENTS_OFFSET, kwnames);
    Py_DECREF(kwnames); Py_DECREF(argv[1]); Py_DECREF(argv[2]);
    return r;
}

static PyMethodDef module_methods[] = {
    {"build", build, METH_NOARGS, NULL},
    {"parse", (PyCFunction)(void(*)(void))parse, METH_VARARGS | METH_KEYWORDS, NULL},
    {"err_format", err_format, METH_O, NULL},
    {"call_method", call_method, METH_VARARGS, NULL},
    {"dict_iter", dict_iter, METH_O, NULL},
    {"threads", threads, METH_NOARGS, NULL},
    {"capsule", capsule, METH_NOARGS, NULL},
    {"unicode_ops", unicode_ops, METH_NOARGS, NULL},
    {"buffer_sum", buffer_sum, METH_O, NULL},
    {"vectorcall", (PyCFunction)(void(*)(void))vectorcall, METH_FASTCALL, NULL},
    {NULL}
};

static int module_exec(PyObject *m) {
    long *state = PyModule_GetState(m);
    *state = 1234;
    if (PyType_Ready(&Vec_Type) < 0) return -1;
    if (PyModule_AddType(m, &Vec_Type) < 0) return -1;
    PyObject *counter = PyType_FromModuleAndSpec(m, &Counter_spec, NULL);
    if (!counter) return -1;
    if (PyModule_Add(m, "Counter", counter) < 0) return -1;
    PyObject *err = PyErr_NewExceptionWithDoc("capi_selftest.Error", "custom error", PyExc_ValueError, NULL);
    if (PyModule_Add(m, "Error", err) < 0) return -1;
    if (PyModule_AddIntConstant(m, "ANSWER", 42) < 0) return -1;
    if (PyModule_AddStringConstant(m, "NAME", "selftest") < 0) return -1;
    return 0;
}
static PyModuleDef_Slot module_slots[] = {
    {Py_mod_exec, module_exec},
    {Py_mod_multiple_interpreters, Py_MOD_PER_INTERPRETER_GIL_SUPPORTED},
    {Py_mod_gil, Py_MOD_GIL_NOT_USED},
    {0, NULL}
};
static struct PyModuleDef module_def = {
    PyModuleDef_HEAD_INIT, "capi_selftest", "C-API self test", sizeof(long), module_methods, module_slots,
};
PyMODINIT_FUNC PyInit_capi_selftest(void) { return PyModuleDef_Init(&module_def); }
