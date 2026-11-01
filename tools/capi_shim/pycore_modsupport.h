/* Project-written stand-in for CPython's internal pycore_modsupport.h, providing
   only what Argument-Clinic output in spec/Modules uses, implemented on top of the
   public C API. (Internal headers are not part of the spec, see DECISIONS D2.) */
#ifndef RPY_SHIM_PYCORE_MODSUPPORT_H
#define RPY_SHIM_PYCORE_MODSUPPORT_H
#include <string.h>

#ifndef _PyCFunction_CAST
#  define _PyCFunction_CAST(func) ((PyCFunction)(void(*)(void))(func))
#endif

static inline int
_PyArg_CheckPositional(const char *name, Py_ssize_t nargs,
                       Py_ssize_t min, Py_ssize_t max)
{
    if (nargs >= min && nargs <= max) {
        return 1;
    }
    if (nargs < min) {
        if (name != NULL)
            PyErr_Format(PyExc_TypeError,
                         "%.200s expected %s%zd argument%s, got %zd",
                         name, (min == max ? "" : "at least "), min,
                         min == 1 ? "" : "s", nargs);
        else
            PyErr_Format(PyExc_TypeError,
                         "unpacked tuple should have %s%zd element%s, but has %zd",
                         (min == max ? "" : "at least "), min,
                         min == 1 ? "" : "s", nargs);
    }
    else {
        if (name != NULL)
            PyErr_Format(PyExc_TypeError,
                         "%.200s expected %s%zd argument%s, got %zd",
                         name, (min == max ? "" : "at most "), max,
                         max == 1 ? "" : "s", nargs);
        else
            PyErr_Format(PyExc_TypeError,
                         "unpacked tuple should have %s%zd element%s, but has %zd",
                         (min == max ? "" : "at most "), max,
                         max == 1 ? "" : "s", nargs);
    }
    return 0;
}

static inline void
_PyArg_BadArgument(const char *fname, const char *displayname,
                   const char *expected, PyObject *arg)
{
    PyErr_Format(PyExc_TypeError,
                 "%.200s() %.200s must be %.50s, not %.50s",
                 fname, displayname, expected,
                 arg == Py_None ? "None" : Py_TYPE(arg)->tp_name);
}

static inline int
_PyArg_NoKeywords(const char *funcname, PyObject *kwargs)
{
    if (kwargs == NULL || PyDict_GET_SIZE(kwargs) == 0) {
        return 1;
    }
    PyErr_Format(PyExc_TypeError, "%.200s() takes no keyword arguments", funcname);
    return 0;
}
static inline int
_PyArg_NoKwnames(const char *funcname, PyObject *kwnames)
{
    if (kwnames == NULL || PyTuple_GET_SIZE(kwnames) == 0) {
        return 1;
    }
    PyErr_Format(PyExc_TypeError, "%s() takes no keyword arguments", funcname);
    return 0;
}
static inline int
_PyArg_NoPositional(const char *funcname, PyObject *args)
{
    if (args == NULL || PyTuple_GET_SIZE(args) == 0) {
        return 1;
    }
    PyErr_Format(PyExc_TypeError, "%.200s() takes no positional arguments", funcname);
    return 0;
}

/* Collect positional + keyword arguments into `buf` in parameter order. */
static inline PyObject * const *
_PyArg_UnpackKeywords(PyObject *const *args, Py_ssize_t nargs,
                      PyObject *kwargs, PyObject *kwnames,
                      struct _PyArg_Parser *parser,
                      int minpos, int maxpos, int minkw, int varpos,
                      PyObject **buf)
{
    const char * const *kw = parser->keywords;
    const char *fname = parser->fname ? parser->fname : "function";
    Py_ssize_t nkwlist = 0, posonly = 0, i;
    while (kw[nkwlist] != NULL) {
        if (kw[nkwlist][0] == '\0' && posonly == nkwlist) {
            posonly++;
        }
        nkwlist++;
    }
    Py_ssize_t nkwargs = kwnames ? PyTuple_GET_SIZE(kwnames)
                                 : (kwargs ? PyDict_GET_SIZE(kwargs) : 0);
    Py_ssize_t maxargs = posonly + (nkwlist - posonly);
    if (!varpos && nargs > maxpos) {
        PyErr_Format(PyExc_TypeError,
                     "%.200s() takes %s %d positional argument%s (%zd given)",
                     fname, minpos < maxpos ? "at most" : "exactly", maxpos,
                     maxpos == 1 ? "" : "s", nargs);
        return NULL;
    }
    for (i = 0; i < maxargs; i++) {
        buf[i] = NULL;
    }
    Py_ssize_t npos = nargs < maxpos ? nargs : maxpos;
    for (i = 0; i < npos; i++) {
        buf[i] = args[i];
    }
    for (Py_ssize_t k = 0; k < nkwargs; k++) {
        PyObject *key, *value;
        if (kwnames) {
            key = PyTuple_GET_ITEM(kwnames, k);
            value = args[nargs + k];
        }
        else {
            Py_ssize_t pos = 0, n = 0;
            while (PyDict_Next(kwargs, &pos, &key, &value)) {
                if (n++ == k) break;
            }
        }
        if (!PyUnicode_Check(key)) {
            PyErr_SetString(PyExc_TypeError, "keywords must be strings");
            return NULL;
        }
        Py_ssize_t j;
        for (j = posonly; j < nkwlist; j++) {
            if (PyUnicode_EqualToUTF8(key, kw[j])) break;
        }
        if (j == nkwlist) {
            PyErr_Format(PyExc_TypeError,
                         "%.200s() got an unexpected keyword argument '%S'",
                         fname, key);
            return NULL;
        }
        if (buf[j] != NULL) {
            PyErr_Format(PyExc_TypeError,
                         "argument for %.200s() given by name ('%s') and position (%zd)",
                         fname, kw[j], j + 1);
            return NULL;
        }
        buf[j] = value;
    }
    for (i = 0; i < minpos + minkw && i < maxargs; i++) {
        if (buf[i] == NULL) {
            if (i < posonly || i < minpos) {
                if (i < nargs) continue;
                PyErr_Format(PyExc_TypeError,
                             "%.200s() takes %s %d positional argument%s (%zd given)",
                             fname, minpos < maxpos ? "at least" : "exactly", minpos,
                             minpos == 1 ? "" : "s", nargs);
            }
            else {
                PyErr_Format(PyExc_TypeError,
                             "%.200s() missing required argument '%s' (pos %zd)",
                             fname, kw[i], i + 1);
            }
            return NULL;
        }
    }
    return buf;
}

#define _PyArg_UnpackKeywordsWithVararg _PyArg_UnpackKeywords

static inline int
_PyArg_ParseStack(PyObject *const *args, Py_ssize_t nargs, const char *format, ...)
{
    va_list va;
    PyObject *t = PyTuple_New(nargs);
    if (t == NULL) return 0;
    for (Py_ssize_t i = 0; i < nargs; i++) {
        PyTuple_SET_ITEM(t, i, Py_NewRef(args[i]));
    }
    va_start(va, format);
    int r = PyArg_VaParse(t, format, va);
    va_end(va);
    Py_DECREF(t);
    return r;
}

#endif
