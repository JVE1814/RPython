/* Project-written stand-in (public API only); see pycore_modsupport.h. */
#ifndef RPY_SHIM_NAMESPACE_H
#define RPY_SHIM_NAMESPACE_H
static inline PyObject *_PyNamespace_New(PyObject *kwds) {
    PyObject *types = PyImport_ImportModule("types");
    if (types == NULL) return NULL;
    PyObject *ns = PyObject_GetAttrString(types, "SimpleNamespace");
    Py_DECREF(types);
    if (ns == NULL) return NULL;
    PyObject *args = PyTuple_New(0);
    PyObject *r = PyObject_Call(ns, args, kwds);
    Py_DECREF(args); Py_DECREF(ns);
    return r;
}
#endif
