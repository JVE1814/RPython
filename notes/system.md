# System workstream notes (sys, threads, signals, processes, import machinery)

## Layout
- `crates/runtime/src/modules/sys.rs` / `sys_full.rs` — sys module. `RtConfig` (in sys_full.rs) is the
  interpreter configuration filled by the launcher (`lib::Config.sys`); `sys_full::config()` reads it.
  `sys_full::register_system_modules()` registers this workstream's builtin modules (called from
  `modules::register_all`), `post_init()` runs after sys/builtins exist (faulthandler, importlib, site),
  `finalize()` runs `threading._shutdown`, `atexit._run_exitfuncs`, flushes std streams.
- `sys_structseq.rs` — struct sequences (`ss::make_type(name, fields, n_in_sequence, allow_new)`,
  `ss::new_instance`, `ss::get_field`). Separate from a future `types/structseq.rs`; unify later.
- `sys_namespace.rs` — minimal `types.SimpleNamespace` (for sys.implementation, sys.monitoring.events).
  `set_namespace_type()` lets the functional workstream's type take over.
- `sys_trace.rs` (event dispatch) / `sys_trace_api.rs` (settrace/setprofile/monitoring API).
- `_thread.rs` — real OS threads, futex locks (`RawLock`, usable from Rust), RLock, `_ThreadHandle`,
  `_local`, `_excepthook`, set_name/_get_name, interrupt_main. `thread.rs` — async exceptions.
- `signal.rs` (`_signal`), `faulthandler.rs`, `gc_mod.rs` (`gc`), `select.rs`, `_posixsubprocess.rs`,
  `_queue.rs`, `_imp.rs`, `pymain.rs` (running __main__, REPL fallback, exit codes).
- `crates/runtime/src/import.rs` — Rust import entry points; installs importlib at start-up.
- `crates/launcher/src/main.rs` — command line + PYTHON* environment parsing.
- Lib: importlib/ (_bootstrap, _bootstrap_external, __init__, machinery, util, abc, _abc), threading,
  queue, signal, subprocess, selectors, runpy, site, _sitebuiltins, platform, sysconfig/,
  `_sysconfigdata_t_linux_x86_64-linux-gnu.py`.

## Decisions
- The interpreter runs on a spawned 256 MB-stack thread. The OS main thread blocks all asynchronous
  signals so process-directed signals reach the Python main thread (needed for pthread_sigmask
  semantics); the interpreter thread restores the original mask and calls `process::exit` itself
  (so `fork()`ed children exit correctly).
- Signals: the C handler only sets flags, sets EVAL_SIGNALS on the main thread state, writes the
  wakeup fd and futex-wakes the main thread if it is blocked in a lock (`MAIN_WAIT_ADDR`). Blocking
  waits on the main thread also wake at least every 20–50 ms to run handlers.
- Every blocking call goes through `threads::blocking(|| ...)` (mode = Blocked, safepoint poll on
  return) as required by the GC's stop-the-world protocol. Thread exit: `threads::thread_exit()` sets
  `exited`, removes the state from ALL_THREADS and recycles the ThreadState memory (pointers to thread
  states stay valid).
- Import: `_frozen_importlib` = Lib/importlib/_bootstrap.py and `_frozen_importlib_external` =
  _bootstrap_external.py, compiled at start-up with filenames `<frozen importlib._bootstrap...>`.
  If installing the path finders fails, the Rust import implementation is used instead (and meta_path
  is cleared). Fast path: a fully initialised module in sys.modules never enters Python.
  `RPYTHON_NO_IMPORTLIB=1` disables importlib (debugging). Tracebacks are trimmed like CPython's
  remove_importlib_frames.
- `sys.implementation.cache_tag = 'cpython-315'`, `importlib.util.MAGIC_NUMBER` uses our own number
  (3999) because our bytecode differs from CPython's: CPython-written .pyc files are rejected (bad magic)
  and recompiled. .pyc files are written when marshal can dump our code objects.
- Module locks (`_ModuleLock`) keep a strong-ref registry when `_weakref` is unavailable.
  `_find_and_load` resolves (imports or waits for) parent packages before taking the child's lock
  (`_resolve_parents`, as in the 3.15 tests), and a deadlock while waiting for a module returns the
  partially initialised module.
- site.py is imported at start-up unless -S; if that import fails the error is only reported with -v
  (CPython would abort) so a partially available stdlib stays usable.
- Monitoring/settrace offsets are `lasti * 4` (consistent with `frame.f_lasti`).
- Fixed a dict bug in `types/dict.rs::remove_at` (owned by the core workstream): trimming trailing
  holes let insert/delete cycles fill the index table with DUMMY markers, so looking up a missing key
  never terminated (hash-seed dependent hangs in importlib). Holes are now kept until a rebuild;
  `pop_last` (popitem) scans past holes, which makes repeated popitem O(n) per call — the dict owner
  may want a dummy counter instead.
- Fixed a type-system bug in `types/slots.rs::add_operators`: heap types with multiple bases got
  slot wrappers for slots inherited from non-primary bases (infinite recursion, e.g. `__init__`).

## Missing / known gaps
- PEP 810 lazy imports: sys API only (set/get_lazy_imports[_filter], lazy_modules); the interpreter
  imports eagerly.
- frame.f_lineno assignment (jumps) not implemented.
- setprofile c_call/c_return/c_exception and monitoring CALL, JUMP, BRANCH_*, C_RETURN, C_RAISE,
  STOP_ITERATION, EXCEPTION_HANDLED, PY_THROW, RERAISE events are not dispatched (API accepts them).
- importlib.resources / importlib.metadata / importlib.readers not written.
- faulthandler: fatal handler dumps only the current thread (free-threaded behaviour); no C stack.
- gc: generations are reported but there is a single collector; get_count is approximate.
- Most Python modules here depend on modules from other workstreams (os, io, types, collections,
  enum, warnings, ...) and could only be tested partially.
