# Workstream "capi": CPython 3.15 free-threaded C API / ABI, extension loading

## Coverage (declared vs exported)
The inventory `crates/runtime/src/capi/declared.txt` is produced by `tools/capi_inventory.sh`
(gcc -E over the public headers in `spec/Include`, both the full and the `Py_LIMITED_API`
view, `cpython/` included, `internal/` excluded). One line per symbol: `name kind flags`,
where flags are `dep` (Py_DEPRECATED), `full` and `limited`.

| | declared | exported | non-deprecated declared | non-deprecated exported |
|---|---|---|---|---|
| functions | 1208 | 1208 | 1174 | 1174 |
| data | 193 | 193 | 174 | 174 |

* `tools/capi_coverage.sh [python3] [-v]` prints these numbers (`-v` lists anything missing).
* `cargo test -p python --test capi_symbols` (crates/launcher/tests/capi_symbols.rs) fails if any
  non-deprecated declared symbol is not in `nm -D` of python3.
* "Exported" means the symbol exists. A few entries are deliberate stubs (see "Missing").

## Files
* `crates/runtime/src/capi/*.rs`: one file per header area. Every entry point is
  `#[unsafe(no_mangle)] extern "C"` and adapts the C conventions (NULL / -1 with an exception
  set; borrowed vs new references) to the runtime's `PyResult`. Helpers live in `mod.rs`
  (`ret`, `ret_i`, `nn`, `null_arg`, ...).
  - refcount, mem (three allocator domains with PyMem_SetAllocator hooks, GC alloc), object,
    typeobj (PyType_Ready, PyType_FromSpec/FromMetaclass/FromSlots with every slot id incl.
    the 3.15 PySlot ids, GetModuleByDef/ByToken, watchers), abstract_ (call/vectorcall,
    number, sequence, mapping, iter, PyIter_Send), varargs (Py_BuildValue, CallFunction*,
    PyOS_snprintf, PySys_Write*), printf (PyUnicode_FromFormat engine, shared with
    PyErr_Format), getargs (every format unit, keywords, PyArg_Parser), unicode (incl.
    PyUnicodeWriter), long (incl. AsNativeBytes, Export/Writer, PyFloat_Pack/Unpack),
    bytes (incl. PyBytesWriter), buffer, containers (tuple/list/set/slice), dict, module
    (multi-phase, PyModExport, Py_mod_* slots), modsupport, capsule, errors, codecs, import,
    funcs (cfunction/method/function/code/frame/descr/member/weakref/contextvars), ceval,
    pystate, pythread, lifecycle (PyConfig_Get/Set, PyInitConfig, Py_GetConstant), misc
    (signals, PyOS_*, sys, PyFile_*, PyTime_*, monitoring), data (exported data and placeholder
    types), compat (deprecated or private symbols that extensions and the test helpers use).
* `crates/runtime/src/capi_ext.rs`: extension loading. Suffixes are
  `.cpython-315t-x86_64-linux-gnu.so`, `.abi3t.so` and `.so`. dlopen uses
  RTLD_NOW|RTLD_LOCAL and is cached per path. Hook names: `PyModExport_<name>` (PEP 793), then
  `PyInit_<name>`, using the punycode `U_` form for non-ASCII names. Single- and multi-phase
  init are supported, with m_size state, `__file__` and `__spec__`. Also provides the `_imp`
  module (create_dynamic, exec_dynamic, extension_suffixes, ...), registered only if no
  other `_imp` exists.
* `tools/build_test_exts.sh [OUTDIR]`: builds `_testcapi`, `_testlimitedcapi`, `_testbuffer`,
  `_testsinglephase` and `_testimportmultiple` from spec/Modules. It cannot build
  `_testinternalcapi`, `_testmultiphase`, `_testclinic` or `_testclinic_limited`, because
  they need generated `Modules/clinic/*.c.h` files that are not in spec/ or need internal
  headers (out of scope, D2). From `_testcapi` it leaves out `bytes.c`, `immortal.c` and
  `monitoring.c` (internal structs), registering stub init functions in their place.
* `tools/capi_shim/`: our own stand-ins for the few internal headers that the spec helper
  sources include, written only against the public API (arg-parsing helpers, `_PyOnceFlag`,
  `_PyNamespace_New`, ...). Only names and test-observed error texts follow CPython (D19).
* `tools/capi_selftest/`: our own extension module plus its driver (`run.sh [python3]`). It
  covers static and heap GC types, number/buffer slots, members/getsets, subclassing, module
  state, PyType_GetModule, getargs errors, Py_BuildValue, threads with
  PyGILState_Ensure and critical sections, capsules, unicode, and vectorcall.
* `tools/capi_minitest/run.py`: a temporary stand-in for unittest/test.support, used to run
  spec/Lib/test/test_capi modules while the real stdlib pieces are missing.

## Decisions
* **Symbols:** the C API is exported from the static python3 executable
  (`-Wl,--export-dynamic`, D13), and extensions do not link libpython. Types owned by
  other workstreams are exported by giving their statics the C name, either directly or
  with `#[unsafe(export_name = ...)]`.
* **Placeholder types:** `capi/data.rs` holds non-instantiable placeholder statics for
  declared types the runtime does not have yet (instancemethod, OrderedDict and its views,
  PickleBuffer, stderrprinter). An owner that exports the real type deletes the
  placeholder; the linker reports the duplicate otherwise.
* **Object allocator:** tlab segments are aligned to `SEGMENT`, and a lock-free bitmap
  (`tlab::owns`) tells PyObject_Free whether a block came from the tlab or from malloc.
  PyObject_Init sets the large-object GC bit for blocks not allocated from the tlab.
* **Thread states:** an extension calling the C API on a thread the runtime does not know
  gets a thread state on first use (GILState). PyEval_SaveThread / Py_BEGIN_ALLOW_THREADS
  detaches the thread (mode InNative, current = NULL), so stop-the-world never waits for it.
  PEP 788 interpreter guards and views are tokens. Sub-interpreters (Py_NewInterpreter,
  PyInterpreterState_*) are identities with their own id, dict and atexit list over the one
  shared runtime; there is no real isolation.
* **Critical sections:** the runtime uses `ob_mutex` as a short, non-reentrant leaf lock
  (types/dict.rs `ob_lock`). An extension's critical section stays open across arbitrary
  C-API calls, so it cannot hold that lock without deadlocking. `PyCriticalSection_Begin(op)`
  therefore takes `op` in a separate lock table keyed by object address, while sections over
  an explicit PyMutex lock that mutex. Re-entering a lock the same thread already holds is a
  no-op. Before a thread blocks on a contended section, and whenever it detaches, it
  suspends all of its sections. A suspended section takes its locks back when it is again
  the innermost one. Each runtime operation stays atomic under its own `ob_lock`, but an
  extension's critical section does not exclude Python-level code running on other threads.
* **PyMutex:** uses the 1-byte layout from the header (bit 0 locked, bit 1 has parked
  waiters), with a hashed condvar table as the parking lot.
* **Deprecated borrowed getters** (PyWeakref_GetObject, PyDict_GetItem, ...) return
  borrowed references that rely on the container or another object keeping the referent
  alive, the same contract as in CPython.

## Test status
* `_testcapi` / `_testlimitedcapi` self-tests (`test_*` functions in the modules): 138 of 146
  pass. The failures:
  - test_reftracer, test_bytes_resize_tracer: PyRefTracer_SetTracer only stores the tracer;
    it is not called from the runtime's allocation and deallocation paths.
  - test_gc_visit_objects_{basic,exit_early,frozen}: PyUnstable_GC_VisitObjects needs a
    registry of all GC objects, which the collector does not expose.
  - test_code_extra (imports the `test` package), test_datetime_capi (no `datetime`),
    test_pep3118_obsolete_write_locks (no `_io._BytesIOBuffer`): missing stdlib.
* tools/capi_selftest: all checks pass.
* spec/Lib/test/test_capi run module by module through tools/capi_minitest (earlier run, before
  the latest master merge): test_build 6/6, test_bytearray 8/8, test_pyatomic 63/63,
  test_hash 3/3, test_time 5/5, test_frame 4/4, test_function 12/13, test_slice 8/9,
  test_eval 10/11, test_long 37/40, test_structmembers 10 OK (the other 10 fail inside the
  Lib warnings machinery). Many other modules are blocked on stdlib modules that were
  missing at the time (contextlib, enum, gc, io, sysconfig, ...). Now that master runs
  `python -m test`, rerun them with the real regrtest.

## numpy (cp315t wheel, 2.5.4)
* All 411 C-API symbols that numpy's extension modules import are exported, and its .so
  files load.
* `import numpy` now gets through the stdlib and stops at **`No module named 'datetime'`**.
  On top of that, `_multiarray_umath` calls PyDateTime_IMPORT and reads the
  `PyDateTime_GET_*` / `PyDateTime_DELTA_GET_*` fields directly. So `datetime` must be a
  native `_datetime` with the struct layouts from spec/Include/datetime.h and a
  `datetime.datetime_CAPI` capsule (PyDateTime_CAPI with the type pointers and
  constructors). A pure-Python datetime is not enough. That is the data workstream's module.
* pip install and running numpy's pytest suite were not attempted, because import fails first.

## Missing / known gaps
* Ref tracer hooks and GC object visiting (above). PyUnstable_Code_New* returns an error
  (code objects cannot be built from raw bytecode). PyGen_New / PyCoro_New from a C frame are
  unsupported. Perf-map and monitoring functions are no-ops. Py_Initialize / Py_Finalize and
  embedding are stubs, because the runtime is always initialised by the launcher.
* Sub-interpreters share all state (identities only).
* Extension critical sections do not exclude other threads' Python-level operations on the
  same object (see Decisions).
* Watchers (dict, type, code, function, context) can be registered, but events are delivered
  partly or not at all (the runtime has no hooks for most of them).
