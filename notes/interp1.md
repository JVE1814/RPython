# interp1 — subinterpreters

Status and design of multiple interpreters in one process (`_interpreters`, `_interpqueues`,
`_interpchannels`, `concurrent.interpreters`, `InterpreterPoolExecutor`, the C API).

## Proposed decision D26 — Interpreters are isolation domains over one shared VM

Like several applications/class-loader domains in one JVM: one heap, one collector, one set of
threads and one JIT; Python-level state is per interpreter.

**Finding the interpreter.** Every thread state points to its interpreter
(`PyThreadState.interp`, which the ABI already has; null means the main interpreter).
`subinterp::current()` is the only lookup: while no subinterpreter was ever created
(`subinterp::USED == false`, a relaxed load and a predicted branch) it returns the static
main `Interp` without touching TLS; afterwards it reads the current thread state's
pointer. Threads started from an interpreter inherit it (`_thread` copies the parent's
pointer into the new thread state before any Python code runs).

**Per interpreter** (`subinterp::Interp`): `sys` and `sys.modules`, `builtins` and its
`__import__` (identity check of IMPORT_NAME), the importlib bootstrap module and the
"importlib installed" flag, builtin-module objects (`import.rs` table), `__main__`,
`PyInterpreterState_GetDict`, `PyUnstable_AtExit` callbacks, config (PyInterpreterConfig),
whence, id, running/active state, `_imp._override_multi_interp_extensions_check`,
the reported GIL state, the main-thread ident, pending-call queues.
Module-level statics that hold Python state use `subinterp::PerInterp<T>` (main value
inside the static, others created on first use and dropped when the interpreter is
destroyed): atexit callbacks, warnings filters/onceregistry/defaultaction, the codec
registry, queue/channel end types, pending calls. `subinterp::InterpPtr` is a drop-in
replacement for `static X: AtomicPtr<PyObject>` holding module heap objects (exception
classes etc.): done for binascii, _struct, _csv, _locale, termios, _queue, _io
(UnsupportedOperation), _socket (herror/gaierror).

**Shared**: the heap, allocator, GC (stop-the-world covers every interpreter's threads;
an interpreter's roots are Rust-owned references so trial deletion keeps them alive),
static and immortal objects (static types, small ints, interned strings, None/True/False),
the exception classes of `_interpreters`/`_interpqueues`/`_interpchannels` (created once,
in the main interpreter), process-wide config (`sys.flags`, paths), signal handling (main
interpreter only), audit hooks, gc callbacks, faulthandler, `_datetime`'s types (not yet
converted: its C-API capsule is a process-wide struct).

**Code and JIT.** Code objects are never shared: every value that crosses goes through the
cross-interpreter data layer (`subinterp/xidata.rs`), which copies (marshal) or pickles,
so code is compiled per interpreter. Inline caches and JIT-compiled code key on the actual
dict/type objects and versions they guard, and those objects belong to one interpreter
(or are static and shared, which is safe because they are immutable or internally
synchronised). Hence caches and compiled code need no interpreter key. The two
interpreter-dependent globals the interpreter loop uses (builtins of a frame-less call,
`__import__` identity) are resolved through `current()` at the slow path only.

**Running code in another interpreter** on the same OS thread (`_interpreters.exec/call`,
`PyThreadState_Swap`): the caller's thread state is detached (`InNative`, safe for
stop-the-world), a fresh thread state of the target is attached, and the caller's is
re-attached afterwards (`subinterp::enter/leave`). The temporary state is retired without
the per-OS-thread teardown of `thread_exit` (QSBR, frame pool, allocator caches stay with
the OS thread); its biased-refcount merge queue is drained before reuse. Exceptions cross
as text snapshots (`xidata::Captured`), optionally with a pickle of the exception.

**Extensions.** Single-phase extension modules are refused in every subinterpreter
(free-threaded build: `check_multi_interp_extensions` cannot be turned off, configs with it
off fail to create, as CPython ft does). Multi-phase modules follow
`Py_mod_multiple_interpreters`. In a subinterpreter an extension's PyInit runs with the main
interpreter attached; a failure is reported there as unraisable and an ImportError is
raised in the subinterpreter. A module without `Py_mod_gil = Py_MOD_GIL_NOT_USED` makes a
*subinterpreter* report `sys._is_gil_enabled() == True` (with the RuntimeWarning); there is
no GIL, and the main interpreter keeps reporting False (unchanged behaviour).

**Cost.** `-c pass`: 66.50M -> 66.62M instructions (+0.17%, callgrind, same stdlib). An
import-in-a-loop microbenchmark (200k `import os` + builtin calls): +0.7%.

## Files
- `crates/runtime/src/subinterp/mod.rs`: `Interp`, registry, `current()`, `PerInterp`,
  `InterpPtr`, create/finalize/destroy, enter/leave, extension checks, config.
- `subinterp/xidata.rs`: cross-interpreter data (plain values via marshal, buffers,
  queues/channels, stateless functions, pickle with the `<fake __main__>` script reload).
- `subinterp/module.rs` (`_interpreters`), `queues.rs` (`_interpqueues`), `channels.rs`
  (`_interpchannels`, ChannelID as a per-interpreter Python class), `testapi.rs`
  (`_testinternalcapi` helpers: create/exec/destroy interpreters, refcounts, settings,
  pending_identify, simple_pending_call, raw XI data).
- `Lib/concurrent/interpreters/{__init__,_queues,_crossinterp}.py` (new, written from the
  documented API), `Lib/concurrent/futures/{__init__,interpreter}.py` (pool enabled).
- Small, flagged (`interp1`) edits in shared runtime files: sys.rs, builtins.rs, import.rs,
  lib.rs (initial sys.path), capi/pystate.rs (interpreter states, per-interpreter pending
  calls), capi/lifecycle.rs (Py_NewInterpreter*, Py_EndInterpreter), capi_ext.rs
  (extension checks), _thread.rs (interpreter inheritance, thread/daemon policy, per
  interpreter shutdown join, main thread ident), posix/proc.rs (fork/exec policy),
  syslog.rs, _imp.rs, _warnings.rs, _codecs.rs, atexit.rs, sys_finalize.rs
  (subinterpreters finalized at exit), sys_full.rs (`_is_gil_enabled`), interp/mod.rs
  (frame-less globals = running `__main__`), testinternalcapi.rs (hook).

## Missing / known issues
- `test_interpreters` is skipped as a package on free-threaded builds (also in CPython ft);
  run with a copy of the package minus that skip: 175 run, 3 failures (gc threshold test:
  our collector's threshold scales with the live set, so `gc_threshold(1)` does not force a
  collection; REPL "Interpreter.close()" message; no-memory stress test).
- `test_crossinterp`: unpickling `__main__` objects relative to a `__main__` whose `__file__`
  was replaced while its `__spec__` is set (the `<fake __main__>` reload only happens when
  `__main__.__spec__` is None, to never re-run a `-m` main module).
- `_datetime`, `gc.callbacks/garbage`, signal module objects, `_contextvars.Token.MISSING`,
  `functools.Placeholder`, tty modules still keep module objects in process-wide statics.
- Destroying an interpreter while daemon threads of it still run keeps its roots (they are
  cleared, not freed) to avoid use-after-free in those threads.
- Interned strings are one process-wide table (test_sys `test_subinterp_intern_dynamically_allocated`
  wants per-interpreter interning of dynamically created strings); a 1-char latin-1 str made by
  decoding is not the cached singleton (`test_subinterp_intern_singleton`).
- `_testcapi.set_nomemory` fault injection is not honoured (test_atexit low-memory test).
- Blocking `Queue.put/get` poll (5 ms) in Python; channel `send(blocking=True)` waits in Rust.
