# parallel workstream notes (concurrent.futures, multiprocessing, fork safety, threading)

## Files
- `Lib/concurrent/__init__.py`, `Lib/concurrent/futures/{__init__,_base,thread,process,interpreter}.py`
- `Lib/multiprocessing/` — complete package: `__init__`, `context`, `process`, `util`, `reduction`,
  `connection`, `queues`, `synchronize`, `popen_fork`, `popen_spawn_posix`, `popen_forkserver`,
  `spawn`, `forkserver`, `resource_tracker`, `resource_sharer`, `heap`, `shared_memory`,
  `sharedctypes`, `managers`, `pool`, `dummy/{__init__,connection}`.
- `crates/runtime/src/modules/_multiprocessing.rs` — `_multiprocessing.SemLock` (POSIX named
  semaphores: sem_open/sem_timedwait/sem_post/sem_getvalue), `sem_unlink`, `flags`.
- `crates/runtime/src/fork.rs` — runtime fork protocol (see below); hooks added to `gc.rs`
  (`fork_prepare/fork_parent/fork_child`), `tlab.rs` (`lock_depot_for_fork`), `threads.rs`
  (`FREE_STATES` made `pub(crate)`), `posix/proc.rs` (`os.fork`/`os.forkpty` use `fork_with`).

## Decisions
- Default start method (3.14+ rules): `forkserver` when fd passing works and the temporary
  directory is writable (probed lazily on first use; importing multiprocessing does not import
  tempfile/shutil/subprocess), else `spawn`. `get_all_start_methods()` lists the default first and
  omits `forkserver` when the default fell back to `spawn`.
- resource tracker protocol: one line per message; "CMD:name:rtype" (simple format, names may
  contain ':' — the tracker splits on the first and last colon) or a JSON object (default, and
  always used for names with newlines/non-ASCII). Exit status of the tracker: 0 clean,
  1 leaked resources, 2 cleanup failed, 3 bad message. A plain `os.fork()` child closes its copy of
  the tracker pipe (at-fork hook); multiprocessing's fork start method keeps it (thread-local flag
  set by popen_fork around os.fork()).
- Connection authentication: `#CHALLENGE#{digest}<40 random bytes>`, answer
  `{digest}HMAC(authkey, challenge)`; bare 20-byte challenges get a bare MD5 HMAC (legacy).
- fd passing (`reduction.sendfds/recvfds`) packs the fds with `struct` (no `array` dependency).
- `concurrent.futures.InterpreterPoolExecutor` always resolves (the test suite refers to it at
  class-creation time); without an `_interpreters` module its constructor raises
  NotImplementedError. No subinterpreter support exists in the runtime yet.
- ProcessPoolExecutor (3.15 behaviour): a worker that exits at `max_tasks_per_child` is replaced
  by the manager thread directly (`_spawn_worker`, ignoring stale idle-semaphore tokens); if the
  executor was garbage collected / is being force-shut-down / spawning fails and no worker is
  left, the pending futures fail with BrokenProcessPool ("garbage collected" / "forcefully shut
  down" / "could not be started"). `shutdown(wait=False)` keeps the state replacement needs.
  The wakeup pipe is cleared without the shutdown lock.

## Runtime bugs found and fixed here
- JIT code memory (`interp/jit/mem.rs`) is a MAP_SHARED memfd: after fork parent and child kept
  sharing it, so code compiled later by one process overwrote live machine code of the other
  (random attribute errors / segfaults in both). Children now remap private copies at the same
  addresses (`jit::mem::after_fork_child`, called from `posix::after_fork_child_hook`).
- `_thread` handles: after fork only the forking thread's handle stays joinable
  (`FORK_SURVIVOR`); previously every pre-fork handle (including the surviving main thread's)
  looked finished in the child.
- `faulthandler._sigsegv()` did not crash (Rust's SIGSEGV stack-overflow handler swallowed the
  raised signal) when faulthandler was disabled.
- `_thread.interrupt_main()` in the main thread runs the handler immediately.
- `python -c` code is dedented (3.14).
- `_multiprocessing.SemLock`: owner/count updates ordered for threads sharing one SemLock;
  a SemLock rebuilt from a raw (name-less) handle does not sem_close it.
- `time.sleep` checks pending signals before sleeping (a SIGINT that arrived just before the
  call was otherwise only handled after the full sleep).
- the thread that survives fork gets its new native id.

## Known open issues
- test_threading: finalization tests (daemon threads / locals / `__del__` at interpreter exit)
  fail — the runtime does not run module teardown/finalizers at shutdown like CPython;
  test_print_exception_gh_102056 hangs (KeyboardInterrupt from interrupt_main is lost during
  RecursionError-handler recursion — interpreter issue); test_lazy_import: `os` imports
  `warnings` at startup (the `_collections_abc.ByteString` deprecation).
- One run of test_multiprocessing_forkserver.test_manager aborted with glibc "corrupted size vs.
  prev_size" inside sem_open; the borrowed-handle sem_close fix above is the suspected cause
  (not re-verified at the time of writing).
- InterpreterPoolExecutor / concurrent.interpreters: no subinterpreters in the runtime.

## Gaps outside this workstream that block tests
- `array` (data branch), `xmlrpc` (managers' xmlrpclib serializer tests), `zipimport` and
  running `.pyc` files as scripts (test_multiprocessing_main_handling), `ctypes`
  (sharedctypes, Value/Array: tests skip), `_interpreters` (InterpreterPoolExecutor tests
  error in setUp), a startup `DeprecationWarning` for `collections.abc.ByteString` makes
  `-W error` subprocess tests fail.
- The test harness runs with PYTHONPATH=spec/Lib; tests that start `python -I`/`-E` children
  cannot import `test` (install layout needed).

## Test status (local, with a stub `array` module on PYTHONPATH where needed)
- test_concurrent_futures: test_future, test_shutdown, test_thread_pool, test_wait pass;
  test_process_pool 133/135 (worker replacement failure case fixed afterwards); test_deadlock
  crash cases fixed by the faulthandler fix; InterpreterPool variants error (no _interpreters).
- test_multiprocessing_fork: test_processes / test_threads / test_manager pass except
  xmlrpc-dependent tests; test_misc: remaining failures are the -c dedent (fixed), start-method
  side effect (fixed), manager exception cycles (fixed), SetProxy iteration (fixed).
- test_threading: all pass except the ones fixed in this branch (RLock args, Condition.locked,
  NaN timeouts, interrupt_main, fork join); test_thread, test_threadsignals pass;
  test_threading_local passes with the new `_threading_local`; test_queue: 2 fixes.

## Fork protocol (crate::fork)
`prepare()`: take the import lock (blocking) -> stop the world (`gc::fork_prepare`: wait for every
other thread to reach a safepoint or be blocked/native; GC lock taken; GC list spinlock held) ->
hold the allocator depot, `ALL_THREADS`, `FREE_STATES`. After fork both sides release them; the
child also resets the collector (no parked threads), retires the other thread states, makes the
forking thread the main thread and resets the import lock. In the parent, if other Python threads
existed, `DeprecationWarning: This process (pid=N) is multi-threaded, use of fork() may lead to
deadlocks in the child.` is emitted (an error with -W error makes os.fork() raise in the parent).
