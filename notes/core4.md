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

### settrace / monitoring (test_sys_settrace 86 -> 17, test_monitoring 52 -> 11)
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

## Testing notes
* Use an installed-layout snapshot for suite modules that spawn `-I` subprocesses (tools/snapshot.sh); the dev
  binary cannot see spec/Lib there.
