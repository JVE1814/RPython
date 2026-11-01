# Workstream "stability": the interpreter must never crash, corrupt memory, hang or deadlock

Branch `worktree-agent-a58c24f34aa6d7a46`. Regression tests: `tools/stress/run.sh PY`, which runs
every `tools/stress/*.py` with a timeout. Each script is multi-threaded and takes 1 to 10 s. All of
them pass on this branch.

## Fixed (root causes)
1. **Stop-the-world handshake (gc.rs, threads.rs).**
   * The old check compared a count of parked threads with a separately sampled count of
     running ones, using release/acquire only. A thread leaving a blocking call
     (PyEval_RestoreThread, `threads::blocking`, PyThread lock waits, thread start) could
     therefore run during a collection.
   * Effect: unlinking GC list entries while `find_unreachable` had tagged the `prev` links.
     That is the misaligned-pointer abort in `gc::untrack` seen in test_ssl (report 2), and
     it could also corrupt the lists.
   * New protocol: a store/load handshake, sequentially consistent on both sides.
     * The stopper publishes `RELEASE=false`, then waits until no other thread's mode is
       `InVm`.
     * Every switch to `InVm` goes through `gc::enter_vm`, which publishes the mode, then
       checks `RELEASE` and falls back to a safe mode while the world is stopped.
       `ModeCell::set(InVm)` routes there, so all existing call sites are covered.
     * New thread states start `Blocked` and enter the VM through the handshake.
     * The stopping thread is exempt (`STW_OWNER`).
     * A failed stop (100 ms) leaves the next attempt to later allocations.
   * `fork_child` resets `STW_OWNER` too.
2. **Finalizers no longer run with the world stopped.** `__del__`, weakref callbacks and
   tp_clear used to run while every other thread was parked. A finalizer that needed a
   lock held by a parked thread therefore hung forever (`gc_finalizer_lock.py` hung before).
   The collection now has these phases:
   * stopped: analysis, incref of the garbage, and detaching weakrefs with callbacks
     (`weakref::detach_weakrefs_for_gc`, new; nothing is run);
   * running: callbacks and finalizers;
   * stopped again: the resurrection check, then detaching the remaining weakrefs;
   * running: tp_clear.
   If nothing can run, the whole collection happens in one stop. core-runtime's ordering
   (callback weakrefs before finalizers, plain ones before tp_clear) and DEBUG_SAVEALL/stats
   are kept.
3. **Biased refcount merge queue (object.rs, threads.rs).** A non-owner decref that drove
   the shared count negative queued the object without owning a reference. The owner could
   then free it while it was still queued, and the queue processing touched freed memory
   (heap corruption).
   * Now a decref at shared count ≤ 0 marks the object QUEUED *without decrementing*, so
     the queue entry owns that reference. The merge is `object::merge_queued` (local−1).
   * The owner-gone path merges directly.
   * `thread_exit` stops accepting queue entries (alive=false under the ALL_THREADS lock),
     drains its queue while it still counts as running, then goes `Blocked` before it is
     marked exited.
4. **Lists read without a lock (report 4).**
   * Affected readers: the interpreter's BinarySubscr fast path, the JIT helpers (subscript,
     unpack), the JIT *inline* list subscript and list ForIter, list iterators shared between
     threads (double release of `seq`), and PyList_GetItemRef/SetItem/GetSlice.
   * `threads::MULTI_THREADED` (a u64 set once a second thread state exists) gates them.
     While single-threaded the inline paths stay lock-free. Otherwise the JIT inline code
     jumps to the helpers, and those read under `ob_lock` (`list::get_signed`,
     `list::items_if_len`).
   * Iterators step under the iterator's lock.
   * **For perf-runtime:** when QSBR lands, the list array can be retired instead and the
     inline paths re-enabled. Swap `MULTI_THREADED` for `qsbr::multi_threaded()` and remove
     the guard in `tier1.rs` (`binary_subscr`, `for_iter`).
5. **bytearray free-threading (test_bytes crash).**
   * Writers run under the object lock after argument conversion, never while Python code
     runs.
   * Replaced storage (a bytes object) is released with the new `gc::defer_decref`. It is
     released at the next stop-the-world (`mature_deferred`/`release_mature`), or by
     `gc::flush_deferred` when collections are disabled. A collection is forced after 4096
     deferrals.
   * No in-place realloc while multi-threaded.
   * Exports (buffer pins) are counted under the lock. The getbuffer view and its pin are
     taken atomically.
   * Readers snapshot (start, size) under the lock.
   * The shared bytearray iterator steps and reduces under its lock.
   * `gc::defer_decref` is the D5 "release at the next safepoint" mechanism. perf-runtime's
     QSBR can back it later; the API stays the same.
6. **Allocation aborts → MemoryError (test_io abort).** `errors::try_zeroed_vec` and
   `try_vec_with_capacity` are used for buffer sizes that come from Python:
   * BufferedReader.read1 and read_n, FileIO.read;
   * os.read, os.pread, os.urandom, os.getrandom;
   * socket recv, recvfrom and recvmsg;
   * marshal file reads.
7. **test_picklebuffer abort.** `_struct.Struct` now has `s_size` and `s_len` right after
   the object header, which `_testbuffer.c` reads directly.
8. **test_typing timeout.** `typing.get_type_hints` raises ValueError("wrapper loop ...") on
   a `__wrapped__` cycle instead of looping.

## Checked, fixed elsewhere
test_descr, test_exceptions (context-cycle hang), test_generators (throwing a class whose
`__new__` returns a non-exception), test_isinstance and test_lazy_import all complete with the
core-runtime build. Those fixes are now in master and were merged here.

## Results (release build of this branch)
* `tools/stress/run.sh`: all 6 pass. Before the fixes, gc_finalizer_lock hung,
  list_slice_race segfaulted reliably and bytearray_race segfaulted.
* test_list 2 failures (run4: 3), test_gc 15 (19), test_weakref 1 (3), test_struct 8 (8),
  test_typing completes (31 failures; was TIMEOUT).
* test_bytes, with the test extensions from /home/user/snap on the path: runs to the end, 339
  tests and 13 failures (run4: CRASH).
* test_picklebuffer: passes, 9/9 (run4: CRASH).

## Not done / next steps
* test_subprocess TIMEOUT: hangs in `test_communicate_timeout_large_input`. Not
  investigated (likely subprocess/communicate threading in Lib or selectors).
* Report 1 (glibc "corrupted size vs. prev_size" in test_multiprocessing_forkserver
  test_manager): not reproduced here. Fix 3 (freed objects in merge queues) and fix 1
  (threads running during a collection) are plausible causes, and parallel's SemLock
  sem_close fix also applies. Re-run that test.
* Report 3 (test_enum test_unique_composite under load): `tools/stress/enum_composite.py` did
  not reproduce it with or without the fixes. Fixes 1 and 3 are the likely causes.
* No ASan/TSan builds were made (build time under a load average of 40: about 13 min per
  release build). No sanitizer target dirs exist.
* Remaining lock-free container readers to audit: dict/set reads that hand out borrowed
  values, tuple-from-list conversions in other modules, and the C API `PySequence_Fast`
  users. A thread that calls `threads::current()` after `thread_exit` would create a
  zombie InVm state that makes every stop-the-world time out (not observed; worth an
  assertion).
* `wait_for_release`/`stop_the_world` poll with yield then sleep. A futex would cut
  latency.
