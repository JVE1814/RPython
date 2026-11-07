/* Project-written stand-in (see tools/build_test_exts.sh) for the one internal int helper
 * _testcapi/immortal.c uses, expressed with the public API: a "small int" is one of the
 * runtime's cached, immortal ints -5..1024. */
#ifndef RPY_SHIM_PYCORE_LONG_H
#define RPY_SHIM_PYCORE_LONG_H
#include "Python.h"

static inline int
_PyLong_IsSmallInt(PyLongObject *op)
{
    PyObject *o = (PyObject *)op;
    if (!PyLong_CheckExact(o) || !_Py_IsImmortal(o) || !PyUnstable_Long_IsCompact(op)) {
        return 0;
    }
    Py_ssize_t v = PyUnstable_Long_CompactValue(op);
    return v >= -5 && v <= 1024;
}
#endif
