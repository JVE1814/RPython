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

## Fork protocol (crate::fork)
`prepare()`: take the import lock (blocking) -> stop the world (`gc::fork_prepare`: wait for every
other thread to reach a safepoint or be blocked/native; GC lock taken; GC list spinlock held) ->
hold the allocator depot, `ALL_THREADS`, `FREE_STATES`. After fork both sides release them; the
child also resets the collector (no parked threads), retires the other thread states, makes the
forking thread the main thread and resets the import lock. In the parent, if other Python threads
existed, `DeprecationWarning: This process (pid=N) is multi-threaded, use of fork() may lead to
deadlocks in the child.` is emitted (an error with -W error makes os.fork() raise in the parent).
