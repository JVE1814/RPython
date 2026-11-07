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

# Workstream "stability2"

Branch `worktree-agent-a6ad975e2ae61cb5a`. Stress tests: `tools/stress/run.sh PY` (new:
type_refcount_drift, deque_race, weakref_race, shared_objects, join_releases).

## Fixed (root causes)
1. **pandas UAF: heap-type refcount drift (object.rs).** perf2's per-thread type counts
   gave a type with an id a bias of one reference. An instance's type reference is often
   taken per thread (generic_alloc) and dropped with a plain decref (Cython heap-type
   deallocs `Py_DECREF(tp)`, `__class__` assignment), so between collections the fields
   fell by up to the number of such instances and reached zero: `pandas.Timestamp` was freed
   under live Timestamps. The bias is now `TYPE_BIAS = 2^50`; a type with an id is only ever
   freed by the collector (which subtracts the bias, merges counts, then releases the id and
   the bias under a stopped world). `sys.getrefcount`/`Py_REFCNT` hide the bias
   (`object::visible_refcnt`).
2. **subtype_dealloc with an extension *static* base** (numpy ndarray subclasses, Cython
   static types) never dropped the instance's type reference (leak; classes never freed).
   Now dropped after the base dealloc unless the dealloc is our own code
   (`typeobject::is_own_code`, executable segment range from dl_iterate_phdr).
3. **Inherited `__new__` (slots.rs update_one_slot):** with several C bases the tp_new of the
   first MRO entry was used; now the solid base's (tp_base) unless the wrapper's type is a
   subtype of it. pandas `MultiIndexUInt8Engine(BaseMultiIndexCodesEngine, UInt8Engine)` got
   uninitialised C fields (NULL decref crash in every MultiIndex lookup).
4. **Thread safety of library objects.** Many Rust-state objects had no synchronisation at
   all and corrupted memory when shared: `collections.deque` (asyncio's ready queue: the
   asyncio free-threading crash/hang), buffered/text file objects (concurrent `print` to one
   file corrupted the malloc heap: test_io/multiprocessing heap-corruption reports),
   StringIO/BytesIO, array, OrderedDict, itertools (count/cycle/tee...), lru_cache, csv,
   dbm, bz2/lzma, mmap, contextvars.Context, heapq on a shared list.
   * New `objlock.rs`: per-object *recursive* locks in a sharded table, waiting threads are
     in Blocked mode (stop-the-world safe), held across blocking calls (buffered I/O must not
     change state mid-flush), no-op while single-threaded, reset in a forked child.
   * `METH_LOCKSELF` (private ml_flags bit) + `methods::lock_self_methods(tp)`: every method
     of the type runs with `objlock::lock(self)` (taken in `call_methoddef`);
     `locked_slot!` wraps slot functions. Used for the types above.
   * deque: all state under its objlock; searches (index/count/in/remove) hold it across the
     comparisons (test_free_threading.test_collections).
5. **Weakrefs:** dereferencing used `refcnt() > 0` + incref, which resurrected objects that
   another thread was deallocating. Now `object::try_incref` (CAS on the shared field; the
   owner frees directly only when the shared field is exactly 0, which try_incref never
   increments; weakref creation sets MAYBE_WEAKREF via `enable_try_incref`). Also used for
   basic-ref reuse, `__weakref__`, callbacks list, PyUnstable_TryIncRef.
6. **Field replacement races:** setters freed the old value immediately while getters on
   other threads were taking a reference (`gen.__name__`, `f.__dict__ = d`, `e.__dict__`,
   exception fields, `__slots__` members, every `fncommon::set_field` user). New
   `qsbr::swap_field` (atomic swap + QSBR-deferred decref). Lazily created dicts (instance,
   function, exception) use a CAS (lost attributes in test_free_threading.test_dict).
   Specialised `__slots__` stores (interpreter, tier 1, tier 2) take that path when threaded.
7. **`subclasses()`** handed out raw pointers to classes other threads were freeing; it now
   returns `SubType` (try_incref under the subclass lock).
8. **Thread join (coordinator item):** references the joined thread dropped while two threads
   existed sat in QSBR batches; `Thread.join()` + weakref check saw them alive (~1/300 joins
   under load). `_thread` join now processes the merge queue and calls
   `qsbr::quiesce_now()` (releases everything when no other thread is attached).
9. `_thread._local` dealloc dropped its dict map before a subclass's tp_clear used it
   (valgrind); monitoring callbacks were released while holding MON (self-deadlock in
   test_free_threading.test_monitoring); `_weakrefset`/`weakref` removal queues were
   drained with check-then-pop (IndexError under threads); heappop could push None.
11. **Zombie thread states.** `threads::blocking()` and `PyThread_acquire_lock_timed` called
    `threads::current()`, which *attaches a new state* when the thread has none. A thread
    waiting on a PyThread lock while detached (Py_BEGIN_ALLOW_THREADS, e.g.
    `_testcapi.call_in_temporary_c_thread`, and any extension that waits on a lock with
    the GIL released) got a second state, InVm forever, never polling: every later
    stop-the-world timed out (no cycle collections: memory growth, 100 ms stalls) and
    QSBR never released anything (test_no_refcycle_through_target failed in the full
    test_threading run). Both now leave detached threads alone (`current_ptr()`).
    `qsbr::quiesce_now` (join) also waits up to 50 ms for running threads to report.
12. **Extension critical sections = `ob_mutex`** (issue 4): `PyCriticalSection_Begin(op)`
    locks the object's own `ob_mutex` (CPython's bit protocol), so the runtime's leaf lock
    (`dict::ob_lock`, used by dict/list/set/bytearray/... operations) waits for another
    thread's section; the section's own thread passes through (`cs_holds_mutex`, ObGuard
    with a null mutex). Long leaf-lock waits and PyMutex parking happen in Blocked mode.
    bitarray's mutable-exporter test: 10/10 (was flaky). Tier-2 TryLock deopts on a held
    mutex (correct, slower for a thread inside its own section).
13. `_PyTuple_Resize` on a shared tuple moved the items out of it (test_capi passed a code
    constant: crash later in test_tuple); now SystemError unless refcount 1 or empty.
    `PyUnstable_Object_ClearWeakRefsNoCallbacks` called the callbacks. `_BytesIOBuffer`
    getbuffer with NULL view/source (test_capi.test_misc crash).
14. Coordinator run-7 list: (1) deque race — fixed (item 4); (2) multiprocessing manager
    heap corruption — the likely causes (unlocked file objects/deque/Condition waiters) are
    fixed, see results; (3) test_capi crash after "text[\udc80]" — it was test_tuple
    (item 13); test_capi now runs to the end; (4) test_itertools — passes (9 ordinary
    failures); the old binary died with "stack smashing detected".
10. `RPY_TLAB_CHECK=1` (tlab.rs): stamps freed small blocks and aborts on a double free
    (use with RPYTHON_JIT=0). tools/wheels/run_numpy_tests.sh passed its own arguments to
    pytest (fixed).

## Status
* pandas `test_multilevel`: 21 passed (crashed on the first test). Full suite: see below.
* test_asyncio.test_free_threading: 24/24 loops OK with and without the JIT (crashed/hung).
* test_free_threading (57 modules, run one by one): no crash or hang left in the modules
  listed as crashing at the start (collections, context, dbm_gnu, dict, functools,
  generators, monitoring, slots, type, weakref, csv, heapq, mmap). Remaining failures
  are behavioural: bz2 (compressor returns the header on the first call), lzma decompressor,
  tokenize (`tokenize._tokenize` missing), threading (`_testinternalcapi.test_stop_the_world`
  missing).
