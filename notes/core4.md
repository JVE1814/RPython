# core4 workstream notes

Branch `worktree-agent-ad08c0afb79cd7a40`. Owner of: compiler crate, interp/ (except jit, cache.rs,
specialize.rs), translate.rs/adaptive.rs, types/code.rs/frameobject.rs/genobject.rs, sys tracing/monitoring,
Lib/importlib/_bootstrap*.py, Lib/traceback.py, Lib/threading.py.

## Done

### test_threading (coordinator's "12 failures after merge")
* 8 of the 12 were environmental: dev-binary runs of `-I` subprocesses cannot see `spec/Lib` (`No module named
  'test'`, `_testcapi`). In an installed-layout snapshot (tools/snapshot.sh) test_threading has 4 failures:
  3 subinterpreter tests + test_no_refcycle_through_target.
* test_no_refcycle_through_target: references a joined thread dropped on objects owned by the main thread are
  queued for the owner's refcount merge (biased RC) and were only merged at the next RESUME/backedge. The
  interpreter now checks the eval breaker after every `CALL` (CPython does the same after calls into C), so the
  merge happens right after `join()` returns. **jit4**: the tier-1/2 call paths should poll the eval breaker after
  calls to non-Python callables too (same reason).
* Lib/threading.py: `serialize_iterator`, `synchronized_iterator`, `concurrent_tee` (own design: a shared linked
  list of produced values, one lock; tee objects expose `.lock`). Now 3 failures (subinterpreters only).

### settrace / monitoring (test_sys_settrace 86 -> 1, test_monitoring 52 -> 5)
* Start event ('call', PY_START) is reported at the RESUME instruction (functions with COPY_FREE_VARS/MAKE_CELL
  before RESUME reported line None).
* Line events use the public location table (`code::frame_line`), not the internal per-word locations (which have
  line 0 for some translated words: e.g. RERAISE in a `finally` reached by close()).
* Code after the start RESUME on the def/class line reports that line (class bodies: `(1, 'line')`).
* SEND finishing a generator/coroutine fires STOP_ITERATION and the legacy 'exception' event at the SEND target
  (CPython's END_SEND instrumentation); FOR_ITER's STOP_ITERATION only for generator iterators.
* PEP 669 C API implemented (`PyMonitoring_EnterScope`, `_PyMonitoring_Fire*Event`, in
  modules/sys_trace_api.rs; the no-op stubs in capi/misc.rs removed). `_testcapi`'s monitoring part is now built
  (tools/build_test_exts.sh no longer excludes it; shim header tools/capi_shim/internal/pycore_instruments.h).
  Previously TestCApiEventGeneration's setUp failed after registering the tool and ~40 later tests cascaded.

### Compiler
* Bare local-name expression statements already compile to `LOAD_FAST_CHECK; POP_TOP` (test_exceptions cleanup-
  name tests pass).
* **Legacy pipeline deleted**: `codegen/`, `assemble.rs`, `dis.rs`, `bytecode::{CodeUnit, Constant}`, the legacy unit
  tests (units.rs) and example. `rpy_compiler::compile` now returns `WordcodeOutput` (`compile_wordcode` kept as an
  alias). Runtime: `public_mode()`/`RPYTHON_WORDCODE` gone, `new_code_from_unit`/legacy `const_to_obj` gone, marshal
  loads only the public layout. Kept: `bytecode::Op/ExceptionEntry/Location/encode/decode/co/fast/...` (internal
  form). Remaining dead-ish code: `CodeMeta.public == false` branches and `CodeParts/new_code_from_parts` in
  types/code.rs (only reachable for legacy objects, which can no longer be created) — can be removed later.

### importlib (rule 9 / D19)
* `Lib/importlib/_bootstrap.py` and `_bootstrap_external.py` were (nearly verbatim, minus comments) recalled CPython
  text. Both rewritten in our own structure: module locks with per-waiter "tickets" and an explicit wait-for graph
  walk, spec/attribute code restructured, SourceLoader.get_code split into `_cached_code` + compile/cache, etc.
  Names/behaviour the tests and the runtime use are unchanged.
* 3.15 behaviour added: `PathFinder.discover()` / `FileFinder.discover()`, `machinery.WindowsRegistryFinder`
  (finds nothing here), deprecated `machinery.DEBUG_BYTECODE_SUFFIXES/OPTIMIZED_BYTECODE_SUFFIXES`, no `__cached__`
  module attribute, unmarshal errors in .pyc payloads propagate, partial writes in `_write_atomic` retried,
  `_resolve_parents` waits for initialising ancestors but imports only the immediate parent, relative import without
  fromlist raises `KeyError("'a.b' not in sys.modules as expected")`, namespace path without parent ->
  ModuleNotFoundError. Builtin `__import__` (import.rs): DeprecationWarning for `__package__ != __spec__.parent`,
  the same KeyError.
* test_importlib 39 -> 25 failures (20 are FrozenImporter filename checks that compare with the test package's
  location; 2 `.abi3.so` in EXTENSION_SUFFIXES (runtime `_imp.extension_suffixes`), 2 test_load_short_name
  (extension module 'x' left in sys.modules), test_magic_number (we use our own magic on purpose: pyc caches must
  never be shared with CPython), test_lazy_module_type_override (Lib/importlib/util.py, not mine)). test_import 19 -> 6.
* Not rewritten (candidates for a rule-9 review): Lib/threading.py (e.g. `_make_invoke_excepthook`) and
  Lib/importlib/util.py look recalled too.

### Launcher / finalization (coordinator requests)
* Non-UTF-8 argv: the launcher reads `args_os()`, parses options on a lossy view and passes the exact bytes
  (`Config.argv_bytes`, `RtConfig.orig_argv_bytes`); sys.argv/sys.orig_argv decode them with surrogateescape.
  An undecodable `-c` command prints "Unable to decode the command from the command line:" + UnicodeEncodeError and
  exits 1 (test_cmd_line.test_undecodable_code expectation on Linux).
* Module globals' `__del__` at shutdown: works (verified); `sys.meta_path` is set to None after module teardown so
  imports from late `__del__`/warnings fail quietly instead of looping on "sys.meta_path is empty" ImportWarnings.
* Forked child: the warnings lock held by a vanished thread is released and other threads' thread-local values are
  dropped before os.fork() returns in the child (test_warnings test_fork_other_thread_holds_lock; test_warnings
  passes). Remaining test_io failures are a BufferedWriter write race and BytesIO MemoryError (osio area).

### Later rounds
* settrace jumps: every unbound local gets None on an f_lineno jump (with the RuntimeWarning; bcvm's must-analysis
  was too narrow for the listcomp/with/for tests); among compatible targets the deepest stack wins (jumps inside the
  exceptional copy of a `finally`). A C-level trace function (PyEval_SetTrace) also sees line events of frames that
  were already running. The class-body prologue of a decorated class is on the first decorator's line.
* monitoring: LINE at jump targets on a different line and at exception handlers (also when a generator resumes
  into one through throw/close); LOAD_SUPER_ATTR reports the super() call (CALL/C_RETURN/C_RAISE).
* frames/generators: frame.clear() raises for suspended frames, finalizes never-started coroutines (never-awaited
  warning), is a no-op for released frames; f_lineno/f_trace_lines/f_trace_opcodes deletion errors; exhausted
  generator frames have no f_back; the delegating generator's frame is on the stack while a custom sub-iterator's
  throw() runs; PyEval_GetFrameLocals returns a snapshot dict for function frames; unawaited coroutines warn through
  `warnings._warn_unawaited_coroutine` (origin tracking); coroutine method docstrings; invalid `__anext__` result ->
  TypeError caused by the original error.
* traceback: the native printer folds repeated entries ("[Previous line repeated N more times]") and reads source
  only while io.open exists; PyTraceBack_Print prints banner + entries without carets; Lib/traceback.py works without
  _colorize during shutdown; the wrapper group of a naked exception gets its traceback entry at CHECK_EG_MATCH;
  `_suggestions._generate_suggestions` requires an exact list.
* compiler positions: comprehension GET_ITER at the iterable, loop back edge at the element (dict: key..value),
  MAP_ADD at key..value, assert builds AssertionError at the statement and raises at the test, DELETE_ATTR at the
  attribute name, PUSH_NULL of a call at the callee, async genexpr implicit return at the expression, of two
  same-line NOPs across blocks the later goes, co_positions() of an empty location table is empty.
* compile(..., module=...) warnings are filtered by exactly that module name (not the path-derived names that
  warnings filter matching otherwise also tries).

## Results (installed-layout snapshot of this branch, dev build, after merging master 16a79e7)
test_sys_settrace 448/1 (test_jump_from_yield), test_monitoring 100/5, test_traceback pass, test_frame pass,
test_coroutines pass, test_asyncgen pass, test_dis 145 pass, test_code pass, test_peepholer pass, test_warnings
pass, test_generators 59/3, test_compile 188/2 ("evil undecodable": tokenizer must say "source code cannot contain
null bytes"), test_exceptions 115/4 (_testcapi no-memory tests, test_file_source caret width), test_importlib
1236/25, test_import 128/6, test_threading 247/4 (3 subinterpreters + test_no_refcycle_through_target),
test_cmd_line 82/24, test_listcomps/setcomps/dictcomps: only listcomps' test_frame_locals errors (module/class
scope) remain, test_descr 168/13 (+10 errors), test_opcache 83/3 (+5 errors).

## Open / for others
* test_no_refcycle_through_target: rarely (~1 in 400) a joined thread's references are released *later* (the
  objects are freed after another thread runs), with and without the JIT. Looks like deferred reclamation (QSBR:
  storage freed late together with the references it holds) — stability/perf-runtime.
* test_monitoring: test_async_for/test_match (BRANCH events of async for / match), test_disable_legal_events,
  TestLoadSuperAttr (the line event of `super().method` must come after the super() CALL event).
* test_sys_settrace test_jump_from_yield (f_lineno assignment on the 'return' event of a yield).
* test_descr: variable-size bases with __slots__ (tuple/int/bytes), __bases__ assignment layout check, vicious
  descriptor/MRO tests — type-system work.
* test_opcache: TestInstanceDict needs `_testinternalcapi.get_object_dict_values` (shape.rs API),
  STORE_ATTR_WITH_HINT race test, test_call_list_append_memory_error.
* test_cmd_line remaining 24 (PYTHONMALLOC/mimalloc names, -X importtime, help texts, ...); test_lazy_import not
  looked at.
* rule 9: Lib/threading.py and Lib/importlib/util.py still look recalled; not rewritten in this round.
* jit4: the tier-1/2 call paths should poll the eval breaker after calls into C as the interpreter now does.

## Testing notes
* Use an installed-layout snapshot for suite modules that spawn `-I` subprocesses (tools/snapshot.sh); the dev
  binary cannot see spec/Lib there.
* The scratchpad directory is shared with other agents (jit5 overwrote generic script names); use prefixed names.
