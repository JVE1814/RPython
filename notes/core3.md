# core3 workstream notes

Branch `worktree-agent-af5b331160aaf2696`. Owner of the runtime core (see AGENT_BRIEF / assignment).

## Done

### Interpreter finalization (`modules/sys_finalize.rs`, new)
Sequence of `finalize()` (also behind `Py_FinalizeEx`, run once):
1. `threading._shutdown()` (errors, incl. KeyboardInterrupt, reported as
   "Exception ignored on threading shutdown"), the atexit callbacks (Rust registry, so
   several `atexit` module objects share it), PyUnstable_AtExit callbacks, then the
   non-daemon thread handles are joined again (threads started by atexit callbacks).
2. `sys.is_finalizing()` becomes True; `gc::park_other_threads()` stops every other
   thread for good: a stop-the-world, then `PARK_ALL` makes any thread that later enters
   the VM (`gc::enter_vm`, `safepoint_poll`) park forever. Bounded (~2 s) so a thread
   that never polls cannot hang the exit.
   While parked: blocking lock acquires, `_ThreadHandle.join` of unfinished threads raise
   PythonFinalizationError; starting threads raises "can't create new thread at
   interpreter shutdown".
3. Flush stdout/stderr (status 120 on failure), collect, report gc.garbage
   (`gc_support::dump_shutdown_stats`: "gc: N uncollectable objects at shutdown" ResourceWarning,
   list with DEBUG_UNCOLLECTABLE).
4. Module teardown: `_`, `sys.last_*`, `ps1/ps2` → None, meta_path/path_hooks emptied,
   path_importer_cache cleared; every module except sys/builtins is set to None in
   sys.modules and remembered by weakref; collect (modules nobody else holds die with
   intact globals: their cycles' `__del__` see working modules); the survivors' dicts are
   cleared newest-first, `_`-names first, `__builtins__` kept; sys.modules emptied except
   sys/builtins; collect; flush; `Py_AtExit` functions.
   sys and builtins dicts are never cleared (late `__del__` can still print/use builtins;
   test_sys_ignores_cleaning_up_user_data, ShutdownTest.test_cleanup).
* Cost: `-c pass` ~ +6M instructions (3 full collections + clearing); CPython does the
  same work. perf2 may want to shave it (e.g. skip the last collection when nothing with
  a finalizer/weakref callback exists).
* GC finalizers now run referrers-first (`gc_support::finalization_order`, Tarjan SCC
  order): a TextIOWrapper is closed before its BufferedWriter and FileIO, so unclosed
  files in garbage cycles (e.g. a dead `__main__`) are flushed.
* Thread states created implicitly by foreign OS threads are retired when that thread
  exits (`threads::AutoAttached` TLS guard). A zombie `InVm` state made every
  stop-the-world time out (100 ms per GC attempt, and the exit hung).

### test_threading hang (root causes)
* `PyThreadState_SetAsyncExc` stored into the C-layout field the eval breaker never read.
* Signal handlers ran at the recursion limit and failed with RecursionError, consuming
  the signal (KeyboardInterrupt lost forever in test_print_exception_gh_102056). Handlers
  now get the recursion headroom.
* `time.sleep` etc. (posix check_signals) now raise pending async exceptions.
* Compiler (crates/compiler/src/assemble.rs, jump threading): conditional jumps were
  threaded into backward jumps, so a loop whose body ends in an `if` never executed a
  backward `Jump` and never polled the eval breaker: no signals, no GC, no safepoints
  (stop-the-world timeouts). Conditional jumps now always jump forward.
* test_threading: 247 tests in ~35 s (was: timeout at 900 s). Remaining failures: the new
  3.15 `threading.serialize_iterator/concurrent_tee/synchronized_iterator` (Lib/threading.py
  is not mine), subinterpreter tests, test_no_refcycle_through_target.

### Frozen modules (`frozen.rs`, new)
* `_PyImport_FrozenBootstrap/_FrozenStdlib/_FrozenTest` exported (24-byte `struct _frozen`,
  NULL-terminated). "code" is the module source (embedded with include_bytes!), since our
  bytecode is not CPython's marshal format; `_imp.get_frozen_object` compiles it with
  `import::bootstrap_code` (cached in `__pycache__/<stem>.rpython-boot` next to the stdlib file).
* `_imp`: find_frozen (withdata → memoryview of the source), get_frozen_object (with data:
  compile or "Frozen object named 'x' is invalid"), is_frozen, is_frozen_package,
  init_frozen, `_frozen_module_names` (list), `_override_frozen_modules_for_tests`.
  `PyImport_ImportFrozenModule[Object]` still in capi (returns 0) — `_imp::import_frozen`
  exists for it.
* Lib/__hello__.py, Lib/__phello__/ (own content).
* Decision: frozen modules are **off by default** (`-X frozen_modules[=on]`,
  `PYTHON_FROZEN_MODULES=on` enable them). Only `_frozen_importlib`/`_external` are always
  frozen (they get origin='frozen' specs; `__origname__` set by exec_bootstrap_module).
  Rationale: our pyc/boot caches already avoid compiling at startup; freezing would only
  help with precompiled code in the binary (build-time compilation not available).
  For perf2: if startup profiling shows stat/open of the ~15 startup modules matters,
  embed precompiled marshal data in the tables and turn the default on.
* Known: test_importlib.frozen filename checks compare with STDLIB_DIR derived from the
  `test` package location (spec/Lib), which differs from sys._stdlib_dir in dev runs.

### Other fixes
* GC: legacy finalizers (tp_del) make their cycles uncollectable (gc.garbage, "gc:
  uncollectable <T 0x..>" lines); weakrefs that are garbage are cleared before tp_clear
  (bpo-38006); tuples of untracked objects are untracked by the collector
  (`_testinternalcapi.get_long_lived_total`); gc callbacks get candidates/duration;
  get_count()[1:] count younger collections.
* exec/eval/imports insert the *dict* as `__builtins__` (the caller's builtins mapping);
  frozendict globals accepted ("cannot assign __builtins__ to frozendict globals").
* Shared immortal empty bytes and 1-byte bytes (new_bytes, PyBytes_FromStringAndSize);
  bytearray never adopts them as storage.
* `-W`/PYTHONWARNINGS import warnings at startup (filters effective at shutdown too).
* Functions built from module-level code (FunctionType(compile(...))) run with their
  globals as locals (top-level await tests); exec(code, closure=...) with COPY_FREE_VARS.
* except*: the wrapper group of a naked exception gets its own traceback at the except*
  statement; PyErr_Display/DisplayException handle non-exceptions (test_traceback crash).
* PyErr_WriteUnraisable/FormatUnraisable go through UnraisableHookArgs and add a
  traceback for the current frame; err_msg None for WriteUnraisable.
* Extension modules: uninitialized PyModuleDef (crash), create slot errors.
* sys: exit((42,)), exit with surrogates through sys.stderr, overflow → -1,
  tracebacklimit keeps the innermost entries, thread_info.lock 'pymutex',
  PYTHONIOENCODING with encoding only → strict, getsizeof defaults, _debugmallocstats,
  `-c` source registered in linecache.
* Types: re-entrant `__bases__` assignment from mro() (refcount underflow in type_dealloc,
  test_descr), type() with OrderedDict/frozendict namespace, surrogate `__doc__`,
  `__module__` assignment drops `__firstlineno__`, any `__type_params__`, bool basicsize,
  None.__ne__, chr() range errors, module dir() via `__dict__`.
* Calls: "Value after * / ** ..." messages, `module.qualname()` in "got multiple values",
  non-str `**` keys reach mapping-taking callables, keys() must return an iterable.
* Traceback hints: attribute suggestions read "Did you mean '.x' instead of '.y'?".
* test_calltip text: object.__new__ doc, TextWrapper doc's first lines (D19: only text a
  test compares).
* `_testinternalcapi.has_inline_values` uses `shape::has_inline_values`.

* Later rounds: ImportError repr/kwargs/reset; SyntaxError constructor/re-init and
  compiler-error text read from the file (PEP 263 cookie, BOM, byte offsets without text);
  finished frames keep f_back; module attribute errors (circular import "from 'origin'",
  submodule being imported, stdlib/third-party shadowing hints using the start-up
  sys.path[0]); cannot-import-name shadowing hint; `_imp._fix_co_filename`;
  `f(**m)` key checks; `{**m}` error propagation; mappingproxy equality; full tp_name in
  comparison errors; sys.implementation is a types.SimpleNamespace; types.CapsuleType;
  tp_del on deallocation; `__bases__` assignment rolls back subclass MROs on failure;
  complex.real/imag members; anext(it, default).

## Results (installed-layout snapshot of this branch, release build, after merging master 4606f2c)
| module | run5 (master) | core3 |
|---|---|---|
| test_threading | TIMEOUT | 247 run, 4 failures (~35-110 s) |
| test_gc | 61/12 | 61/1 (test_get_referents_on_capsule: _socket.CAPI) |
| test_sys | 105/17 | 105/4 (3 SizeofTest layout, supports_isolated_interpreters) |
| test_atexit | 10/6 | 10/4 (subinterpreter tests) |
| test_frozen | 3/0 (FAIL) | pass |
| test_builtin | 157/9 | 157/3 |
| test_descr | 168/22 | 168/14 |
| test_class | 42/3 | 42/1 |
| test_types | 135/8 | 135/1 (subinterpreters) |
| test_exceptions | 115/17 | 115/9 |
| test_import | 128/18 | 128/5 |
| test_extcall / test_unpack_ex | 6/16, 6/9 | pass, pass |
| test_range | 29/36 | 29/1 |
| test_traceback | CRASH | 441/16 |
| test_finalization | 18/4 | pass |
| test_asyncgen | 85 (errors) | pass |
| test_coroutines | 100/7 | 100/3 |
| test_importlib | CRASH | 1236/19 (baseline ft: 20) |
Startup: `-c pass` 44.5M instructions; finalization is ~7.5M of that (three full
collections + clearing survivor modules). perf2: the collections dominate; CPython does
the same three collections.

## Not done / handed over
* test_class TestInlineValues (`c.__dict__` access/update keeps inline values) — needs
  shape.rs (perf2): materializing `__dict__` should share the values array.
* Positions-dependent items (KeyboardInterrupt at a frame's first line → empty caret line,
  settrace/monitoring offsets) → bc-compiler/bc-vm (D24).
* queue/lock timed-wait latency (tk report, ~95 ms in IDLE rpc): could not reproduce —
  Queue round trip 24 µs with timeout, SimpleQueue 6 µs, lock ping-pong 260 µs, a
  select+queue rpc-like loop 0.17 ms. Need IDLE's exact scenario.
* test_io test_write_readline_races aborted once ("malloc_consolidate(): unaligned fastbin
  chunk") — not reproducible in 11 runs on this branch or master; stability/grind-lib.
* Lib/importlib/_bootstrap.py looks recalled from CPython in places (rule 9 review
  candidate: e.g. FrozenImporter._fix_up_module asserts).
