# capi3 — test_capi to zero, test_sys

Start: core 64abb6c (+ merge). Measured on installed-layout snapshots (tools/snapshot.sh).

## Before / after

| | before | after |
|---|---|---|
| test_capi (full package run) | run 1534, 40 failures, 353 skipped | run 1562, 0 failures, 353 skipped (= CPython ft baseline) |
| test_sys | run 105, 2 failing (1 failure + 1 error) | run 105, 0 failures (24 skipped: remote-exec tests, remote1's scope) |

| test_capi sub-module | before | after |
|---|---|---|
| test_mem | 16 | 0 |
| test_bytes | 11 | 0 |
| test_unicode | 6 | 0 |
| test_misc | 4 | 0 |
| test_object | 7 | 0 |
| test_file | 4 | 0 |
| test_abstract | 2 | 0 |
| test_type | 1 | 0 |
| test_watchers (full run) | 2 | 0 |
| never collected (Test_testinternalcapi / Test_PyLock) | 28 missing | 28 run |

## What was done

### PYTHONMALLOC debug hooks (capi/memdebug.rs)
`PyMem_SetupDebugHooks` wraps the raw/mem/object domains (also installed at startup for
PYTHONMALLOC=debug or `*_debug`, and for -X dev unless PYTHONMALLOC names a plain
allocator). Block layout: size word, domain letter ('r'/'m'/'o'), 7 guard bytes, data
(0xCD when fresh, 0xDD when released), 8 guard bytes. Free/realloc check the guards and
the letter and abort with a block report ("Debug memory block at address p=...") and a
fatal error; mem/object calls without an attached thread state are fatal. The wrappers
keep a set of the blocks they produced: our runtime allocates its own objects outside
the domains, and extensions free them through `PyObject_Free` (tp_free), so an unknown
pointer is passed to the wrapped allocator unchecked. `hooked_alloc_refuses` treats
"debug over default" as default, and `hook_installed()` turns off again when a test hook
is removed. With the hooks installed the allocator fast paths are off (`SLOW_PATHS`), so
an object an extension built in PyObject_Malloc memory (PyObject_Init) and releases
through the runtime's generic tp_free goes back through the object domain. `_testinternalcapi.check_pyobject_*_is_freed` probe the fill patterns
(`memdebug::object_looks_freed`, also used by PyObject_Dump).

### Bytes / unicode / object C API
* PyBytes_Concat: any two C-contiguous buffers -> bytes; PyBytes_Join: TypeError for a
  non-bytes separator, SystemError for NULLs, bytes.join itself; PyBytes_FromObject
  never calls `__bytes__` (buffer copy, else iterate); PyBytes_Repr(smartquotes=0).
* PyBytesWriter / PyUnicodeWriter: growth past the capacity (and Finish of a
  non-singleton str) asks an installed failure-injecting hook first (MemoryError).
* _PyBytes_Resize of a uniquely referenced, non-empty, unhashed bytes reallocates in
  place (reference tracer sees DESTROY old + CREATE new; on failure CREATE old again);
  size 0 gives the empty singleton.
* PyObject_SetAttr/SetAttrString/DelAttr with NULL value while an exception is set:
  SystemError chained to it. PyObject_Bytes falls back to PyBytes_FromObject.
  PyObject_Print: `<refcnt N at P>` for refcount <= 0, OSError when the stream fails.
  PyObject_Dump: `<object at P is freed>`, and attaches a thread state with
  PyGILState_Ensure (it is callable detached; the implicit state it used to create
  stayed registered and stalled QSBR, which is what made test_watchers fail in the
  full run: the watched type's deferred decref never ran).
* PyFile_NewStdPrinter: own `stderrprinter` type (capi/stdprinter.rs). Py_fopen: NUL
  check. PyObject_AsFileDescriptor: RuntimeWarning for bool.
* PyUnstable_Object_IsUniqueReferencedTemporary also accepts operands on the current
  frame's value stack (arguments of the running C call).
* PySentinel_New(name, NULL): `__module__` is None.
* PyType_GetFlags: version-tag bits read as clear, `_Py_TPFLAGS_STATIC_BUILTIN` for the
  runtime's own static types (address inside the executable). MATCH_SELF set on the
  twelve builtin types (and inherited), ITEMS_AT_END on `type`.
* PyThreadState_Get without an attached thread state: fatal error (as CPython).

### Reference tracer (PyRefTracer_SetTracer)
CREATE from the allocator's slow paths, DESTROY when an object's memory is released,
TRACKER_REMOVED to a replaced tracer. Installing a tracer turns the allocator fast paths
off (`alloc::SLOW_PATHS` = tracemalloc tracing || tracer installed; replaces the plain
TRACING check in `alloc_object`/`free_object`/asyncgen free list; no cost otherwise).
Not reported: ints/floats the JIT boxes inline.

### `_testinternalcapi` self-tests (modules/testinternalcapi_selftest.rs)
Every `test_*` name CPython's module has now exists and tests *our* primitive:
PyMutex bit protocol and parking (basic, two threads/HAS_PARKED, counters, slow
handoff, `benchmark_locks`), OnceFlag (retry after failure, racing initialisers),
RwMutex (shared readers, writer preference), RecursiveMutex, critical sections
(lock/unlock, nesting, suspension on detach, re-acquisition no-ops, contention from 4
threads, GC inside a contended section, a section taken with the world stopped),
bit helpers through int (to_bytes/from_bytes, bit_count, bit_length, _PyLong_NumBits),
dict + intern table, edit cost of suggestions, bytes.find/rfind/count vs a naive
search, thread-state C API, stack protection, PEP 788 guard counting.
New primitives in capi/pythread.rs: `OnceFlag`, `RwMutex`, `RecursiveMutex`.
Fix found on the way: a thread parked in `mutex_lock` returned to InVm without the
stop-the-world handshake (it could take a lock and run while the world was "stopped");
it now re-enters through `gc::enter_vm`.
PEP 788: guards are real allocations counted per interpreter (`pystate::guard_count`);
PyThreadState_EnsureFromView holds one until the matching Release. Interpreter
teardown does not wait for open guards (left).
PyUnstable_ThreadState_SetStackProtection / Reset: set/restore the thread's stack
bounds (minimum 64 KiB).

### test_sys
* Per-interpreter `sys.intern` (types/str.rs `intern_requested`): see decision D-c3-2.
* sizes: see decision D-c3-1.
* compile() of a str source creates the str's cached UTF-8 form (as CPython; counted
  by `sys.getsizeof`).
* dict.popitem takes an instance dict out of the split (class layout) state
  (test_dict test_splittable_popitem, failing before, passes now).

## Regression batch (snapshot, start vs end)
test_capi test_sys test_ctypes test_stable_abi_ctypes test_embed test_cmd_line_script
test_call test_descr test_class test_gc test_threading test_free_threading test_tracemalloc
test_bytes test_str test_unicode_identifiers test_codecs test_int test_long (+ test_list
test_set test_dict test_enumerate test_range test_generators test_frame test_cmd_line
test__interpreters test_crossinterp test__interpchannels test_import test_types test_pickle),
tools/jit_tests/run.sh, tools/stress/run.sh: no new failures. Unchanged failures (also at
the start): test_descr (2), test_gc (2: capsule tracking of `_socket.CAPI`, tuple untrack
counts); test_int's DoS timing test is load-dependent (failed in one start run too).
test_free_threading segfaulted once in the start run, passed in every later run.

## Decisions to log (proposals)

**D-c3-1 sys.getsizeof / `__basicsize__` of builtin types.** Rule: a type's
`tp_basicsize` is always the size we allocate and never smaller than our struct (C
extensions subclassing a public struct place their fields after it). Where our struct
is smaller than free-threaded CPython's and the objects are cheap, the struct is padded
to CPython's size so `tp_basicsize`, `__basicsize__` and `sys.getsizeof` all agree with
CPython: dict key/value/item iterators (+8), enumerate (+8), PyCapsule (+16), module
(+32), EncodingMap (+40). Where the memory picture differs structurally, `__sizeof__`
reports CPython's object size instead: frame and generator (184: CPython embeds the
interpreter frame, ours is pooled separately), range (the four object fields; our
cached native copies are not counted), an instance `__dict__` still in the class's
attribute layout (8 + 8 per value, CPython's split-table values), a heap type with
inline values (+ the fixed 32+64+42*16 shared-keys block CPython allocates; our shapes
play that role), `list(x)` capacity rounded up to an even slot count (CPython's
allocation granule). `set` had a private `version` field after the public
`PySetObject` layout — an ABI hazard for C subclasses — removed (lookups now restart on
table/mask/entry change, as CPython does).

**D-c3-2 Per-interpreter interning.** One shared intern table (immortal strings, used
for code names, attribute names, shapes and every runtime-internal name in all
interpreters) stays. A string that `sys.intern()` in the *main* interpreter made
canonical is recorded as dynamic; a subinterpreter's `sys.intern()` returns shared
entries that are not dynamic and otherwise keeps its own canonical objects in a
per-interpreter table (released with the interpreter; not marked interned in the
object, so runtime lookups keep canonicalising through the shared table and shapes
never see two pointers for one name within an interpreter). Code constants are
interned by our compiler, so they count as static (CPython's statically allocated
strings) and stay shared. Known gap: if main sys.intern()s a run-time string first and
code later uses the same text as a name, a subinterpreter's sys.intern of that text
differs from its code constant.

**D-c3-3 Strings with code points above U+10FFFF.** As in a CPython release build:
PyUnicode_FromKindAndData (more than one code point) and PyUnicodeWriter_WriteUCS4
store such values as given (UCS-4); a single one is refused (SystemError); iterating
over such a string raises SystemError when it reaches the value. Other operations treat
the value as an opaque UCS-4 unit.

**D-c3-4 PYTHONMALLOC debug hooks under -X dev.** Installed (as CPython), with the
pass-through for blocks the wrappers did not allocate (see above).

## Shared files touched (small, marked "(capi3)")
lib.rs, alloc.rs, methods.rs (`is_extension_code` pub(crate)), interp/eval.rs,
types/{str,str_methods,bytes_methods,list,dict,dict_methods,dictview,set,enumerate,module,
frameobject,genobject,range,object_type,sentinel,asyncgen}.rs,
modules/{sys_full,_tracemalloc,testinternalcapi,ast_bridge,codec_charmap,mod}.rs.

## Left
* PEP 788: interpreter finalization does not wait for open guards.
* Reference tracer: objects boxed inline by the JIT (ints, floats) are not reported.
* `threads::current_mut()` still creates a thread state on demand when C code calls
  into the runtime detached; PyObject_Dump and PyThreadState_Get were fixed, other C
  API entry points may still do it (it stalls QSBR for good).
* test_sys: 24 skips are the remote-exec tests (needs process_vm_readv support, remote1).
