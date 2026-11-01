/* RPython stand-in for the internal argument-parsing helpers that the
   Argument-Clinic-generated code in spec/Modules calls. Everything here is
   built on the public C API; only the helper names and the TypeError texts
   (observable by tests) follow the reference implementation.
   Internal headers are not part of the spec (DECISIONS D2). */
#ifndef RPY_SHIM_PYCORE_MODSUPPORT_H
#define RPY_SHIM_PYCORE_MODSUPPORT_H
#include <string.h>

#ifndef _PyCFunction_CAST
#  define _PyCFunction_CAST(func) ((PyCFunction)(void(*)(void))(func))
#endif

/* "s" when n != 1. */
static inline const char *rpy_plural(Py_ssize_t n) { return n == 1 ? "" : "s"; }

/* TypeError for a positional count outside [lo, hi]; `bound` is the violated end. */
static inline void
rpy_count_error(const char *name, Py_ssize_t nargs, Py_ssize_t lo, Py_ssize_t hi)
{
    int too_few = nargs < lo;
    Py_ssize_t bound = too_few ? lo : hi;
    const char *qual = lo == hi ? "" : (too_few ? "at least " : "at most ");
    if (name == NULL) {
        PyErr_Format(PyExc_TypeError, "unpacked tuple should have %s%zd element%s, but has %zd",
                     qual, bound, rpy_plural(bound), nargs);
    } else {
        PyErr_Format(PyExc_TypeError, "%s expected %s%zd argument%s, got %zd",
                     name, qual, bound, rpy_plural(bound), nargs);
    }
}

static inline int
_PyArg_CheckPositional(const char *name, Py_ssize_t nargs, Py_ssize_t min, Py_ssize_t max)
{
    if (min <= nargs && nargs <= max) return 1;
    rpy_count_error(name, nargs, min, max);
    return 0;
}

static inline void
_PyArg_BadArgument(const char *fname, const char *displayname, const char *expected, PyObject *arg)
{
    const char *got = Py_IsNone(arg) ? "None" : Py_TYPE(arg)->tp_name;
    PyErr_Format(PyExc_TypeError, "%s() %s must be %s, not %s", fname, displayname, expected, got);
}

/* Shared body of the three "takes no ..." checks: `count` is the number of
   offending arguments, `what` names their kind. */
static inline int
rpy_reject_args(const char *funcname, Py_ssize_t count, const char *what)
{
    if (count == 0) return 1;
    PyErr_Format(PyExc_TypeError, "%s() takes no %s arguments", funcname, what);
    return 0;
}
static inline int _PyArg_NoKeywords(const char *funcname, PyObject *kwargs)
{ return rpy_reject_args(funcname, kwargs ? PyDict_GET_SIZE(kwargs) : 0, "keyword"); }
static inline int _PyArg_NoKwnames(const char *funcname, PyObject *kwnames)
{ return rpy_reject_args(funcname, kwnames ? PyTuple_GET_SIZE(kwnames) : 0, "keyword"); }
static inline int _PyArg_NoPositional(const char *funcname, PyObject *args)
{ return rpy_reject_args(funcname, args ? PyTuple_GET_SIZE(args) : 0, "positional"); }

/* The k-th keyword argument (name and value), from either calling convention. */
static inline void
rpy_kwarg_at(PyObject *const *args, Py_ssize_t nargs, PyObject *kwargs, PyObject *kwnames,
             Py_ssize_t k, PyObject **name, PyObject **value)
{
    if (kwnames != NULL) {
        *name = PyTuple_GET_ITEM(kwnames, k);
        *value = args[nargs + k];
        return;
    }
    Py_ssize_t pos = 0;
    for (Py_ssize_t seen = 0; PyDict_Next(kwargs, &pos, name, value) && seen < k; seen++) {
    }
}

/* Lay positional and keyword arguments out in `buf`, one slot per parameter in
   declaration order (NULL for parameters not given). Returns `buf`, or NULL with
   a TypeError set. Positional-only parameters have empty names in the keyword list. */
static inline PyObject * const *
_PyArg_UnpackKeywords(PyObject *const *args, Py_ssize_t nargs,
                      PyObject *kwargs, PyObject *kwnames,
                      struct _PyArg_Parser *parser,
                      int minpos, int maxpos, int minkw, int varpos,
                      PyObject **buf)
{
    const char *const *names = parser->keywords;
    const char *fn = parser->fname ? parser->fname : "function";
    Py_ssize_t nparams = 0, first_named = -1;
    for (; names[nparams] != NULL; nparams++) {
        if (first_named < 0 && names[nparams][0] != '\0') first_named = nparams;
    }
    if (first_named < 0) first_named = nparams;

    if (nargs > maxpos && !varpos) {
        PyErr_Format(PyExc_TypeError, "%s() takes %s %d positional argument%s (%zd given)",
                     fn, minpos == maxpos ? "exactly" : "at most", maxpos, rpy_plural(maxpos), nargs);
        return NULL;
    }
    memset(buf, 0, sizeof(PyObject *) * (size_t)nparams);
    Py_ssize_t taken = nargs < maxpos ? nargs : maxpos;
    memcpy(buf, args, sizeof(PyObject *) * (size_t)taken);

    Py_ssize_t nkw = kwnames ? PyTuple_GET_SIZE(kwnames) : (kwargs ? PyDict_GET_SIZE(kwargs) : 0);
    for (Py_ssize_t k = 0; k < nkw; k++) {
        PyObject *name = NULL, *value = NULL;
        rpy_kwarg_at(args, nargs, kwargs, kwnames, k, &name, &value);
        if (!PyUnicode_Check(name)) {
            PyErr_SetString(PyExc_TypeError, "keywords must be strings");
            return NULL;
        }
        Py_ssize_t slot = first_named;
        while (slot < nparams && !PyUnicode_EqualToUTF8(name, names[slot])) slot++;
        if (slot == nparams) {
            PyErr_Format(PyExc_TypeError, "%s() got an unexpected keyword argument '%S'", fn, name);
            return NULL;
        }
        if (buf[slot] != NULL) {
            PyErr_Format(PyExc_TypeError, "argument for %s() given by name ('%s') and position (%zd)",
                         fn, names[slot], slot + 1);
            return NULL;
        }
        buf[slot] = value;
    }

    Py_ssize_t required = minpos + minkw;
    if (required > nparams) required = nparams;
    for (Py_ssize_t slot = 0; slot < required; slot++) {
        if (buf[slot] != NULL) continue;
        if (slot >= first_named && slot >= minpos) {
            PyErr_Format(PyExc_TypeError, "%s() missing required argument '%s' (pos %zd)",
                         fn, names[slot], slot + 1);
        } else if (slot >= nargs) {
            PyErr_Format(PyExc_TypeError, "%s() takes %s %d positional argument%s (%zd given)",
                         fn, minpos == maxpos ? "exactly" : "at least", minpos, rpy_plural(minpos), nargs);
        } else {
            continue;
        }
        return NULL;
    }
    return buf;
}

#define _PyArg_UnpackKeywordsWithVararg _PyArg_UnpackKeywords

/* Vectorcall-style positional parsing: pack into a tuple and reuse PyArg_VaParse. */
static inline int
_PyArg_ParseStack(PyObject *const *args, Py_ssize_t nargs, const char *format, ...)
{
    PyObject *packed = PyTuple_New(nargs);
    if (packed == NULL) return 0;
    for (Py_ssize_t i = 0; i < nargs; i++) PyTuple_SET_ITEM(packed, i, Py_NewRef(args[i]));
    va_list va;
    va_start(va, format);
    int ok = PyArg_VaParse(packed, format, va);
    va_end(va);
    Py_DECREF(packed);
    return ok;
}

#endif
