# hang1 — suite timeouts (asyncio, os, subprocess, cmd_line_script, capi, multiprocessing)

Task: the modules that hit the 900 s per-module timeout in full suite runs (run 3..8: test_asyncio,
test_os, test_subprocess, test_cmd_line_script; run 8 also test_capi, test_multiprocessing_forkserver,
test_multiprocessing_spawn), plus the test_capi.test_tuple segfault and the test_capi.test_misc
pending-call hang.

## Before / after

Before = run 8 (tools/run_tests.sh, 900 s limit): every module below was TIMEOUT (no totals).
After = tools/run_tests.sh on an installed-layout snapshot of this branch (TEST_JOBS=2, machine shared
with other agents). Baseline = CPython 3.15.2 ft (/opt/baseline/results/ft2).

| module | before | after (rev ee20517): status, run/failures/skipped, seconds | baseline run/skipped, s |
|---|---|---|---|
| test_asyncio | TIMEOUT | FAIL 2907/10/31, 130 s | 2929/31, 137 |
| test_os | TIMEOUT | FAIL 552/1/76, 21 s | 552/66, 24 |
| test_subprocess | TIMEOUT | PASS 366/0/43, 27 s | 366/42, 31 |
| test_cmd_line_script | TIMEOUT | FAIL 51/7/0, 8 s | 51/0, 9 |
| test_capi | TIMEOUT (+ segfault in test_tuple) | FAIL 1531/427/353, 83 s | 1562/353, 18 |
| test_multiprocessing_forkserver | TIMEOUT | FAIL 465/2/60, 110 s | 465/60, 195 |
| test_multiprocessing_spawn | TIMEOUT | FAIL 450/2/71, 130 s | 450/71, 222 |

("failures" is regrtest's count and includes subtests; test_capi was 1981 before the PEP 697
fixes of 3cd4778, measured at ee20517 = 427. test_asyncio's 22 missing tests are test_tools, which
needs `_remote_debugging`. Later commit 8cab86c makes test_signal pass completely (57/57).)

## Root causes found (first hang per module, in order of discovery)

* **test_subprocess** `test_communicate_timeout_large_input` / `..._resume_partial_write`:
  `Popen.communicate()` called again after a `TimeoutExpired` registered stdin only when *this*
  call passed input, so the rest of the earlier input (and the EOF after it) was never written and
  the child waited for stdin forever. Lib/subprocess.py now resumes from `_input_offset`.
* **test_os** `test_fork_at_finalization`: `os.fork()` from a `__del__` during interpreter
  finalization forked a child into a half torn-down runtime (other threads already parked); the
  child hung holding the parent's stdout/stderr pipes. `fork()` (and `preexec_fn`) now raise
  `PythonFinalizationError` once finalization started, as CPython does.
* **test_os** `ScandirThreadingTest.test_close_racing_next`: segfault — `os.scandir` iterator
  shared between threads: `readdir()` raced `closedir()`. readdir/closedir now run under the
  iterator's leaf lock and the entry is copied out before unlocking.
* **test_cmd_line_script** `test_repl_stdout_flush*`: the fallback REPL never flushed `sys.stdout`
  after a statement when stdout is a pipe (block buffered), so a reader waiting for the output line
  hung. The REPL flushes stdout/stderr after each statement (pymain.rs).
* **test_capi** `test_tuple`: `_PyTuple_Resize()` moved the items out of a *shared* tuple (a code
  constant) when called with refcount > 1, leaving NULL items in the constant -> segfault in the
  next test. It (and `PyTuple_SetItem`) now require an exclusively owned tuple (SystemError
  otherwise), `PyTuple_Pack` validates the size first (MemoryError / SystemError).
* **test_capi** `test_misc` pending calls: `run_pending_calls()` looped until the queue was empty,
  but a callback that re-queues itself (it wants another thread) was popped again by the same loop
  forever. Pending calls now run at most the number queued on entry, are not re-entrant per thread,
  and the threads that may serve leftover calls are re-signalled.
* **test_capi** also aborted in `test_thread_state_ensure_crossinterp/_view`:
  `PyThreadState_Ensure()` from another interpreter reactivated the thread's main-interpreter state
  instead of creating one; while detached it now reattaches the state the thread last detached with
  `PyEval_SaveThread` (if it belongs to the interpreter).
  (test_capi's apparent 120 s "hang" in `test_bug_59313` was only regrtest's `--timeout` being
  per test *file*: the whole package takes ~70-190 s.)
* **test_asyncio** `test_free_threading.test_all_tasks_race`: segfault — weakref dereference raced
  the referent's last decref (check `refcnt() > 0` then incref). Fixed by try-incref with the
  "maybe weakref" shared-count flag; the same fix landed on master from stability2 in parallel and
  the merge kept master's version. Also a `WeakSet._commit_removals` IndexError race (same).
* **test_asyncio** `test_ssl.test_remote_shutdown_receives_trailing_data*`: the server side looped
  forever in `SSLObject.read()`: after *both* sides sent close_notify our `_ssl` read returned
  `b''` instead of raising `SSLZeroReturnError` (b'' is only right while our side has not sent its
  close_notify). crates/bindings/src/ssl/socket.rs.
* **test_asyncio** `test_sendfile` (random test of it hung, plus segfaults): collections.deque
  append/popleft race (asyncio's `call_soon_threadsafe` from the executor thread lost wake-up
  callbacks). Fixed on master by stability2 ("deque under its object lock"); confirmed here after
  merging master (6/6 clean runs, was 3/6 hangs/crashes).
* **test_multiprocessing_spawn/forkserver** `test_manager.test_rapid_restart`: glibc abort
  ("malloc(): mismatching next->prev_size") found with valgrind: `_thread._local` subclass dealloc
  dropped the per-thread dict map and *then* called the subclass `tp_clear`, which chains to
  `local_clear` and drained the freed map (use-after-free). Fixed here; master got the identical fix
  from stability2 at the same time (merge kept master's version).
* **test_multiprocessing_* test_processes** Barrier tests: the clean-room `threading.Barrier`
  kept its state in `_phase` (strings) and `_inside`, while `multiprocessing.synchronize.Barrier`
  moves `_state`/`_count` into shared memory via properties -> children saw no state and the
  processes hung/errored until the 300 s timeout. threading.Barrier now uses int `_state` and
  `_count`.

## Other fixes (ordinary failures exposed once the modules completed)

subprocess: `_MAXIMUM_WAIT_TIMEOUT` slices in `wait()`, `CalledProcessError.__str__` for non-int
return codes, byte offsets for non-byte memoryview input, uid/gid range errors (OverflowError for
user/group, ValueError for extra_groups). os/posix: `makedirs(parent_mode=)`, `reload_environ`,
bool-as-fd RuntimeWarning, `utime` floors fractions and accepts Decimal/Fraction (exact, via
`as_integer_ratio`), `sendfile` negative count, rename/link of mixed str/bytes (POSIX does not
check), `NODEV` and major/minor/makedev(NODEV), `SCHED_DEADLINE`, `sched_param` pickling, `execve`
env via keys()/values() snapshots (env mutated by `__fspath__`), a closed stdin stays closed
(launcher; Rust had reopened it on /dev/null), new `timerfd_create/settime/gettime(_ns)`,
`preadv/pwritev` + `RWF_*`, `splice`, `getgrouplist`, `initgroups` (posix/extra.rs).
multiprocessing: forkserver passes sys.argv/sys.path/main path through the key pipe as JSON
(preloaded `__main__` sees argv; no command-line length limit), sharedctypes "bad typecode".
asyncio: StreamWriter.start_tls hands bytes already buffered in the reader (the peer's ClientHello)
to the TLS layer (`loop.start_tls(..., _buffered=)` private kwarg), ssl_shutdown_timeout also bounds
flushing after the peer's EOF. C API: `PyMember_GetOne/SetOne` refuse `Py_RELATIVE_OFFSET`,
PEP 697 checks in PyType_FromSpec (relative member offsets, special offset members,
ITEMS_AT_END) and the extension size is rounded to the maximum alignment (1556 subtests of
TestHeapTypeRelative), `_testinternalcapi.tuple_from_pair(_steal)`.

## Changes outside the assigned modules (small, flagged "(hang1)" in comments)

* crates/runtime/src/types/weakref.rs, Lib/_weakrefset.py, modules/_thread.rs: superseded by
  master's identical fixes (merge took master's side).
* crates/runtime/src/modules/pymain.rs: REPL flush after each statement.
* crates/launcher/src/main.rs: a stdin closed at startup is closed again after Rust's runtime init.
* crates/bindings/src/ssl/socket.rs: SSLZeroReturnError after both close_notifys.
* Lib/threading.py: Barrier state attribute names/values.
* Lib/asyncio/{sslproto,streams,base_events}.py.
* crates/runtime/src/capi/{containers,varargs,funcs,typeobj,pystate}.rs.

## Left (root causes known or suspected)

* test_capi: ~200 ordinary failures besides the fixed ones: the 3.15 `PySlot`/module-slot API
  validation (test_slots 121), test_run (88), PyConfig API (34), float pack/unpack (38), watchers,
  unicode writer, PEP 523 frame-eval API.
* test_multiprocessing_* `test_release_task_refs` (ThreadPool and process pool) — root cause found,
  fix NOT committed (for stability2/perf3, who own qsbr/gc): references a thread drops stay alive
  while that thread is blocked, for two reasons that combine:
  1. `list.pop()`/removals release the list's reference through `qsbr::decref_deferred`, i.e. into
     the popping thread's thread-local QSBR batch, which only that thread processes at its eval
     breaker. A worker that pops a task and then blocks in `queue.get()` keeps the task (and its
     arguments) alive until it wakes up or exits. Minimal reproducer: a thread does
     `args = box.pop(); args = None; event.wait()` — the popped object survives `gc.collect()`.
  2. A reference dropped by thread A to an object owned by thread B goes to B's merge queue
     (biased refcounting); a blocked B never merges, so e.g. result tuples created by a pool's worker
     and dropped by the main thread stay alive; freeing one can queue the next level again.
  Prototype that fixed all reproducers and the 3 test_release_task_refs variants (3/3 runs):
  (a) in `threads::blocking()` (InVm only) hand the thread's open/closed QSBR batches to ORPHANS and
  set EVAL_QSBR on every thread (try_lock on ALL_THREADS); (b) at the start of each collection, with
  the world stopped, take the caller's batches + all orphans (all are due: nobody is inside a
  lock-free read) and release them after restarting; then, in up to 8 rounds, stop the world, merge
  every other thread's queue without deallocating (`merge_queued_stopped`), restart, deallocate the
  objects that reached zero. It was reverted because it made
  test_concurrent_futures.test_interpreter_pool deadlock 2/3 runs (main joins an InterpreterPool
  thread that waits for the global import lock; deallocations moved to the collecting thread /
  another thread's poll run finalizers in a context that holds the import lock). A safe version needs
  the deferred deallocations to run where no runtime lock is held (or the import lock to be
  per-module).
* test_os `test_urandom_failure`: CPython on this machine reads /dev/urandom (EMFILE with
  RLIMIT_NOFILE=1); we use getrandom(), which needs no descriptor.
* test_cmd_line_script: zipimport `-m pkg.module` from a zip on PYTHONPATH (2), null bytes in
  source (parser messages, 2), SyntaxWarning filter by module (2), non-UTF-8 script path (pymain
  opens the lossy UTF-8 form of argv; needs the raw argv bytes end to end).
* test_asyncio: `_remote_debugging` module missing (test_tools), `create_connection(..., context=)`
  passing to `_make_ssl_transport` (test_base_events), reference-cycle tests (`*_refcycles*`,
  `*_no_cycles`, handshake handler leak), decimal context in tasks, future traceback.
* faulthandler: dumping "all threads" when a timeout fires with several threads alive segfaults
  (seen after regrtest --timeout expiries).

## How to find hangs

`tools/snapshot.sh <bin> <worktree> <dir>`; `<dir>/bin/python3 -m test <mod> -v --timeout N`
(N applies to each test *file*, so packages like test_capi need a large N); `-i <pattern>` skips a
found hang to continue; `gdb -p` on the hung process; valgrind memcheck (the runtime's tlab
allocator is invisible to it, but malloc'd blocks are covered) found the `_local` use-after-free.
