/* Project-written stand-in. Layout of contexts as seen by the test helpers only. */
#define CONTEXT_MAX_WATCHERS 8
struct _pycontextobject { PyObject_HEAD struct _pycontextobject *ctx_prev; PyObject *ctx_vars; PyObject *ctx_weakreflist; int ctx_entered; };
