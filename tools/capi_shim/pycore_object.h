/* Project-written stand-in (public API only). */
#ifndef RPY_SHIM_OBJECT_H
#define RPY_SHIM_OBJECT_H
static inline int _PyObject_IsFreed(PyObject *op) { return op == NULL || Py_TYPE(op) == NULL; }
#endif
