# capi2 — test_capi failures (C API behaviour)

Task: fix test_capi failures (427 at the start, CPython ft: 0) by sub-module, plus test_embed /
test_cmd_line_script if time allows; later also test_stable_abi_ctypes and test_fileutils.

## Before / after (test_capi, installed snapshot, `-m test test_capi -v`)

Before: run 1531, 427 failures (535 FAIL/ERROR lines incl. subtests), 353 skipped.
After (rev 4a05448 + see below): run 1533 (the 2 extra are `_testcapi.test_immortal_*` from
immortal.c, now built), failures ~40, skipped 353.

| sub-module | before | after |
|---|---|---|
| test_slots | 121 | 0 |
| test_run | 88 | 0 |
| test_watchers | 43 | 0 |
| test_unicode | 35 | 6 (invalid-codepoint strings, MemoryError injection) |
| test_config | 34 | 0 |
| test_float | 32 | 0 |
| test_misc | 29 | 4 (ref tracer x2, FatalError without thread state, heap-type `__del__` refcount) |
| test_getargs | 22 | 0 |
| test_mem | 16 | 16 (PYTHONMALLOC=debug hooks not implemented) |
| test_codecs | 16 | 0 |
| test_exceptions | 15 | 0 |
| test_module | 12 | 0 |
| test_import | 11 | 0 |
| test_bytes | 11 | 11 (bytes/bytearray writer MemoryError injection, concat with memoryview, join) |
| test_immortal | 9 | 0 |
| test_complex | 9 | 0 |
| test_sys, test_object, test_type, test_file, test_capsule, ... | 30 | ~25 (not attempted) |

test_stable_abi_ctypes: 34 errors -> 0 (stable-ABI-only symbols exported, capi/abi_only.rs).
test_fileutils: 35 errors -> 0 (`_testinternalcapi.normalize_path`).
test_embed: all 91 skipped — same as the CPython ft baseline (an installed CPython has no
`Programs/_testembed` either; the test needs the build tree). Nothing to fix.

## Design notes

* `capi/slots.rs`: one walker for PySlot arrays (PyType_FromSlots, PyModule_FromSlotsAndSpec),
  PyType_Spec.slots and PyModuleDef.m_slots: nesting (max 4 levels), compat ids 1..4 per kind,
  unknown/foreign ids (PySlot_OPTIONAL skips), flags on the end marker, NULL rules (error /
  DeprecationWarning under the new API / allowed), PySlot_STATIC requirements, repeats (types:
  data slots error, function slots warn + last wins; modules: error except repeated exec in a
  def and Py_mod_abi). A slot-created module keeps a private synthesized def
  (`PyModuleObject.md_from_slots`; PyModule_GetDef gives NULL).
* `capi/config.rs`: PEP 741 option table (sys attributes, sys.flags fields, fixed values);
  PyConfig_Set validates types, installs a new sys.flags (`sys_full::replace_flag`), updates the
  legacy `Py_*Flag` globals; cpu_count override read by os.cpu_count.
* `capi/watchers.rs`: dict/type/func/code/context watchers. Hot paths only test a relaxed
  counter. Type events found while a type dict is locked are queued and flushed after type
  setattr / PyType_Modified / gc.collect(); IDs carry an epoch so a type watched under a cleared
  registration does not report to the next user of the ID. Type watcher ID 0 is reserved.
* `interp/pep523.rs`: frame-evaluation hooks (`_testinternalcapi.set_eval_frame_record/
  _interp/_default`): `eval_frame` reports entries (and generator creation); the ALL mode makes
  `sys_trace::recompute_tracing` report tracing so compiled code (incl. the callpy machine-code
  path) stands down, plus explicit gates in jit::try_run/osr/call_fast/resume and generator fast
  paths.
* C function results (`methods::check_c_result`): NULL without exception names the function;
  a result with an exception set is a SystemError for extension functions (dladdr: code outside
  the executable); the runtime's own builtins are trusted (no behaviour change for them).
* Small ints are -5..=1024 (was ..=1025); the JIT small-int table and its range checks follow.
* `tools/capi_shim/internal/pycore_long.h` + build_test_exts.sh: immortal.c is built now.

## Shared files touched (small, marked "(capi2)")

types/{module,str,int,dict,funcobject,code,slots,object_type,cfunction}.rs, interp/{eval,mod}.rs,
interp/jit/{mod,tier1}.rs, interp/jit/opt/codegen.rs, interp/specialize.rs, types/genobject.rs,
modules/{sys_full,_contextvars,gc_mod,_posixsubprocess,testinternalcapi}.rs,
modules/posix/proc.rs, modules/tty/curses.rs, modules/_socket/mod.rs, modules/sys_trace.rs,
methods.rs, tools/build_test_exts.sh.

## Left

* test_mem (16): PYTHONMALLOC=debug allocator hooks (guard bytes, "Debug memory block" reports).
* test_bytes (11): writer MemoryError injection (`_testcapi.set_nomemory` allocator hook),
  PyBytes_Concat with buffer objects, PyBytes_Join edge cases.
* test_unicode (6): strings holding code points > 0x10ffff (CPython creates them in release
  builds and `list()` raises SystemError), MemoryError injection.
* ref tracer (PyRefTracer_SetTracer) events; Fatal error for C API calls without thread state.
