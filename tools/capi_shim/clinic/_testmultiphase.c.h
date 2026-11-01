/* Project-written argument-parsing wrappers for spec/Modules/_testmultiphase.c
 * (stand-in for the Argument Clinic output, which is not part of the spec).
 * Written by hand against the public C API: each wrapper is a
 * METH_METHOD|METH_FASTCALL|METH_KEYWORDS function receiving the defining class.
 */

static int
mp_no_args(const char *fname, Py_ssize_t nargs, PyObject *kwnames)
{
    if (nargs != 0 || (kwnames != NULL && PyTuple_GET_SIZE(kwnames) != 0)) {
        PyErr_Format(PyExc_TypeError, "%s() takes no arguments", fname);
        return -1;
    }
    return 0;
}

static PyObject *
_testmultiphase_StateAccessType_get_defining_module_impl(StateAccessTypeObject *self,
                                                         PyTypeObject *cls);

static PyObject *
mp_get_defining_module(PyObject *self, PyTypeObject *cls, PyObject *const *args,
                       Py_ssize_t nargs, PyObject *kwnames)
{
    (void)args;
    if (mp_no_args("get_defining_module", nargs, kwnames) < 0) {
        return NULL;
    }
    return _testmultiphase_StateAccessType_get_defining_module_impl(
        (StateAccessTypeObject *)self, cls);
}

#define _TESTMULTIPHASE_STATEACCESSTYPE_GET_DEFINING_MODULE_METHODDEF \
    {"get_defining_module", (PyCFunction)(void (*)(void))mp_get_defining_module, \
     METH_METHOD|METH_FASTCALL|METH_KEYWORDS, \
     "get_defining_module($self, /)\n--\n\nReturn the module of the defining class."},

static PyObject *
_testmultiphase_StateAccessType_getmodulebydef_bad_def_impl(StateAccessTypeObject *self,
                                                            PyTypeObject *cls);

static PyObject *
mp_getmodulebydef_bad_def(PyObject *self, PyTypeObject *cls, PyObject *const *args,
                          Py_ssize_t nargs, PyObject *kwnames)
{
    (void)args;
    if (mp_no_args("getmodulebydef_bad_def", nargs, kwnames) < 0) {
        return NULL;
    }
    return _testmultiphase_StateAccessType_getmodulebydef_bad_def_impl(
        (StateAccessTypeObject *)self, cls);
}

#define _TESTMULTIPHASE_STATEACCESSTYPE_GETMODULEBYDEF_BAD_DEF_METHODDEF \
    {"getmodulebydef_bad_def", (PyCFunction)(void (*)(void))mp_getmodulebydef_bad_def, \
     METH_METHOD|METH_FASTCALL|METH_KEYWORDS, \
     "getmodulebydef_bad_def($self, /)\n--\n\nPyType_GetModuleByDef with a bad def."},

static PyObject *
_testmultiphase_StateAccessType_increment_count_clinic_impl(StateAccessTypeObject *self,
                                                            PyTypeObject *cls,
                                                            int n, int twice);

/* increment_count_clinic($self, /, n=1, *, twice=False) */
static PyObject *
mp_increment_count_clinic(PyObject *self, PyTypeObject *cls, PyObject *const *args,
                          Py_ssize_t nargs, PyObject *kwnames)
{
    int n = 1;
    int twice = 0;
    if (nargs > 1) {
        PyErr_Format(PyExc_TypeError,
                     "increment_count_clinic() takes at most 1 positional argument (%zd given)",
                     nargs);
        return NULL;
    }
    if (nargs == 1) {
        n = PyLong_AsInt(args[0]);
        if (n == -1 && PyErr_Occurred()) {
            return NULL;
        }
    }
    Py_ssize_t nkw = kwnames ? PyTuple_GET_SIZE(kwnames) : 0;
    for (Py_ssize_t i = 0; i < nkw; i++) {
        PyObject *key = PyTuple_GET_ITEM(kwnames, i);
        PyObject *val = args[nargs + i];
        if (PyUnicode_EqualToUTF8(key, "n") && nargs == 0) {
            n = PyLong_AsInt(val);
            if (n == -1 && PyErr_Occurred()) {
                return NULL;
            }
        }
        else if (PyUnicode_EqualToUTF8(key, "twice")) {
            twice = PyObject_IsTrue(val);
            if (twice < 0) {
                return NULL;
            }
        }
        else {
            PyErr_Format(PyExc_TypeError,
                         "increment_count_clinic() got an unexpected keyword argument '%U'",
                         key);
            return NULL;
        }
    }
    return _testmultiphase_StateAccessType_increment_count_clinic_impl(
        (StateAccessTypeObject *)self, cls, n, twice);
}

#define _TESTMULTIPHASE_STATEACCESSTYPE_INCREMENT_COUNT_CLINIC_METHODDEF \
    {"increment_count_clinic", (PyCFunction)(void (*)(void))mp_increment_count_clinic, \
     METH_METHOD|METH_FASTCALL|METH_KEYWORDS, \
     "increment_count_clinic($self, /, n=1, *, twice=False)\n--\n\n" \
     "Add 'n' from the module-state counter."},

static PyObject *
_testmultiphase_StateAccessType_get_count_impl(StateAccessTypeObject *self,
                                               PyTypeObject *cls);

static PyObject *
mp_get_count(PyObject *self, PyTypeObject *cls, PyObject *const *args,
             Py_ssize_t nargs, PyObject *kwnames)
{
    (void)args;
    if (mp_no_args("get_count", nargs, kwnames) < 0) {
        return NULL;
    }
    return _testmultiphase_StateAccessType_get_count_impl((StateAccessTypeObject *)self, cls);
}

#define _TESTMULTIPHASE_STATEACCESSTYPE_GET_COUNT_METHODDEF \
    {"get_count", (PyCFunction)(void (*)(void))mp_get_count, \
     METH_METHOD|METH_FASTCALL|METH_KEYWORDS, \
     "get_count($self, /)\n--\n\nReturn the value of the module-state counter."},
