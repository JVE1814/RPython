/* Project-written stand-in (public API only). */
#ifndef RPY_SHIM_LOCK_H
#define RPY_SHIM_LOCK_H
typedef struct { uint8_t v; } _PyOnceFlag_shim;
#define _PyOnceFlag _PyOnceFlag_shim
static inline int _PyOnceFlag_CallOnce(_PyOnceFlag *flag, int (*fn)(void *), void *arg) {
    static PyMutex m;
    if (_Py_atomic_load_uint8(&flag->v) == 1) return 0;
    PyMutex_Lock(&m);
    int r = 0;
    if (flag->v != 1) { r = fn(arg); if (r == 0) _Py_atomic_store_uint8(&flag->v, 1); }
    PyMutex_Unlock(&m);
    return r;
}
#endif
