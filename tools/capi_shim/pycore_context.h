/* Project-written stand-in: only the field name the test helpers dereference. */
#define CONTEXT_MAX_WATCHERS 8
struct _pycontextobject { PyObject_HEAD struct _pycontextobject *ctx_prev; };
