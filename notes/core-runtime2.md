# core-runtime2 workstream notes

Status: paused by the coordinator right after the orientation phase. No runtime code was
changed in this round. The branch contains the merge of `core` (which brings in the previous
core-runtime work and notes/core-runtime.md) and the merge of master.

## Findings so far (for whoever picks this up)

### Item 1: interpreter finalization
* `sys_full::finalize()` (crates/runtime/src/modules/sys_full.rs) only does four things:
  `threading._shutdown`, `atexit._run_exitfuncs`, sets FINALIZING, and flushes stdout/stderr.
  It has no GC collection at exit, no module teardown, and no `__del__` at exit.
* Planned sequence, following CPython's documented Py_FinalizeEx order:
  1. `threading._shutdown`, then atexit (already done).
  2. Stop the world (`gc::stop_the_world`, safepoint mechanism in gc.rs), then set the
     finalizing flag. Daemon threads that reach a safepoint or come back from native code
     (`threads::blocking`) after that must park forever instead of running Python code
     (CPython 3.14+ hangs them).
  3. Flush the std streams, then `gc::collect()`.
  4. Module teardown:
     * set `builtins._`, `sys.last_*`, `sys.path_importer_cache`, `meta_path`, etc. to None
     * restore `sys.stdout/stderr` from `__stdout__`/`__stderr__`
     * remove the modules from sys.modules, keeping weakrefs in insertion order
     * collect
     * clear the dicts of modules that are still alive, in reverse order: first names
       starting with `_` (except `__builtins__`), then everything else
     * clear the sys and builtins dicts last, then collect again
  5. Report exceptions from `__del__` through `sys.unraisablehook`
     ("Exception ignored in: ...").
* Tests: test_threading (test_finalize_running_thread, test_locals_at_exit,
  test_join_daemon_thread_in_finalization, test_finalize_daemon_thread_hang, ...),
  test_gc shutdown/legacy-finalizer tests, test_atexit, test_sys.

### Item 3: frozen modules
* `_imp` frozen functions are stubs (crates/runtime/src/modules/_imp.rs, "frozen (none)").
* test_ctypes test_frozentable reads `_PyImport_Frozen{Bootstrap,Stdlib,Test}` as
  `const struct _frozen *`. The struct has the public 24-byte layout from
  spec/Include/cpython/import.h. The tables must list exactly `_imp._frozen_module_names()`,
  in the same order.
* test_importlib/frozen/test_finder.py fixes the loader_state shape
  (`origname`, `filename`), the aliases (`__hello_alias__`, `__phello_alias__`,
  `_frozen_importlib` -> `importlib._bootstrap`) and `__hello_only__`
  (filename REPO_ROOT/Tools/freeze/flag.py).
* `_imp.get_frozen_object("x", b"6'\xd5Cu\x12")` must raise
  ImportError "Frozen object named 'x' is invalid".
* Idea: embed the source of the frozen modules (`include_bytes!`) as the frozen "code" and
  compile it on import.
  * Default `use_frozen_modules` off for dev builds; test_embed expects `not Py_DEBUG`.
  * test_cmd_line only checks explicit `-X frozen_modules` and `PYTHON_FROZEN_MODULES`
    values.
  * Watch the startup cost of compiling a frozen os.py on every start.
  * `__hello__`/`__phello__` source files still have to be written in Lib/.

## Next steps
All of open items 1-6 from the assignment are still open. Note: run4 logs are not verbose;
re-run each module with `-v` to get per-test failures.
