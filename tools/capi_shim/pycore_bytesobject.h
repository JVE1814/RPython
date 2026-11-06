/* Our stand-in for the internal header that _testcapi/bytes.c includes (written against the
 * public API only; see notes/capi.md).
 *
 * bytes.c reports offsetof(PyBytesWriter, obj) as the size of the writer's inline buffer
 * and calls one private constructor. The runtime's writer keeps its data elsewhere; this
 * layout only has to give that offset a plausible value (the tests compare it with 11). */
#ifndef RPY_SHIM_PYCORE_BYTESOBJECT_H
#define RPY_SHIM_PYCORE_BYTESOBJECT_H

struct PyBytesWriter {
    char small_buffer[256];
    PyObject *obj;
};

/* A writer whose Finish functions return a bytearray. */
PyAPI_FUNC(PyBytesWriter *) _PyBytesWriter_CreateByteArray(Py_ssize_t size);

#endif
