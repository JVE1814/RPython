# stability3 — test_free_threading clean, races in the object machinery

Goal: `python -m test test_free_threading` passes as on free-threaded CPython (no failures,
no "env changed"), also under load (two copies at once, load average 8-15).

## Before / after

* Before (master d0f295b, 1 run): 1 failure (test_threading) + env changed in test_cprofile,
  test_io, test_races; test_json / test_list env changed in other runs (2/3 runs each).
* After (installed-layout snapshot of 4c80098 = this branch + core with scale1/tail2/perf5):
  57/57 OK in every single run, and the last 20 paired runs under load (load 8-12) all
  clean. Every failure seen in paired runs along the way was root-caused and fixed (below),
  except one unexplained SIGKILL during test_str (see "Left"). core alone fails
  test_free_threading.test_type every run and test_monitoring about half the time (items
  13, 14).

## Root causes and fixes

1. **test_threading `test_tight_stw_loop_does_not_starve_attach`**: `_testinternalcapi.
   test_stop_the_world` was missing; with it, a thread stopping the world in a tight loop kept
   the others out for good (a waiting thread polled RELEASE with sleeps and never saw it
   true). `gc::enter_vm` now counts threads that had to wait (`ENTERING`) until they got in,
   and `stop_the_world` first gives them up to 5 ms to enter (`let_waiters_in`).
   `safepoint_poll` goes through `enter_vm` so parked threads count too. New
   `gc::test_stop_the_world()`.
2. **test_io**: `_pyio.BytesIO.__setstate__` was missing (the C BytesIO had it). Added: takes
   the `__getstate__` dict, copies the buffer.
3. **test_races**:
   * `super().x` with `__class__` cell holding a non-type raised TypeError (two-argument
     message); zero-argument form now raises RuntimeError("super(): __class__ is not a type")
     (`super_::new_super_zero_arg`, eval.rs and jit helpers LOAD_SUPER_ATTR).
   * `_testinternalcapi.has_split_table` always returned False. A dict materialised from
     inline values gets `ob_flags` bit `dict::OBF_FROM_INSTANCE`; split = that bit + only
     str keys + within the inline capacity.
   * **Use-after-free of closure cell contents** (segfaults / "no __dict__ for setting new
     attributes" with gc bits 0 / Rust panics under load): `cell_set` released the old value
     immediately while LOAD_DEREF readers (interpreter, inline tier-1 code) load and incref
     without a lock. While `threads::MULTI_THREADED`, cell reads and writes now take the
     cell's `ob_mutex` (as CPython's critical sections) and the old value is released at
     once, outside the lock; tier-1 inline LoadDeref goes to the helper then. (A first
     version deferred the release through QSBR: that delayed finalizers, e.g. test_capi's
     function-watcher DESTROY event after `events = []`.) Single-threaded: plain accesses.
   * The cached STORE_ATTR instance-dict path used a borrowed `*slot` (racing a
     `__dict__` assignment, and set_item may wait for the dict lock in blocked mode, which
     counts as quiescent) and created the dict with a plain store: now
     `typeobject::instance_dict` (CAS creation, owned reference).
     `instance_dict_existing` loads the slot once.
4. **test_list** (`test_reverse` reader IndexError): `list.reverse` emptied the list
   (take_items/restore_items) while reversing. Now reversed in place under the lock
   (`list::reverse`). Extended-slice deletion (`del l[::2]`) also compacts in place.
5. **test_json** ("dictionary changed size during iteration" from `list(d)` in the worker):
   `list(x)` / `list.extend(x)` now copy exact dicts, sets/frozensets and exact dict views in
   one snapshot under their lock (also ~5x faster for `list(d)`).
6. **test_cprofile** (threading._dangling modified): the profiler stored the bound builtin
   as the entry object; `pr.disable()` made a cycle profiler -> entry -> bound method ->
   profiler, and `list.append` entries kept the threading helper's lists (and threads)
   alive. New entries for builtins store their label string (as reported by getstats).
7. **test_capi** (paired runs): `PyImport_AddModuleRef` was get-then-set; concurrent callers
   got different modules. Now setdefault (capi/import.rs, flagged).
8. **test_code** (heap corruption "unaligned fastbin chunk" under load): co_extra was a
   growable Vec resized by concurrent `PyUnstable_Code_SetExtra`. Now a fixed array of 255
   atomic cells installed once with a CAS (capi/funcs.rs, flagged).
9. **test_generators / test_opcache for_iter_gen** (segfault, "double free or corruption"):
   * `jit_send_one` and `jit_send_chain` inspected the generator frame (state, resume) and
     decided before claiming the generator; another thread could finish it and free the
     frame in between. Both now claim first and re-validate (unclaim + JS_SLOW otherwise).
   * `gen.close()` finished (freed) the frame without claiming: claimed now ("already
     executing" if running, as CPython). `gen_throw_impl` set `running` with plain stores
     around the delegation; it now claims (`claim`/`unclaim`).
   * `gi_yieldfrom`/`cr_await` read the frame unclaimed (racing close): now read under a
     short "reader claim" (`yieldfrom_shared`); resumers/closers that find a reader holding
     the claim wait for it (`claim_after_readers`), readers wait for other readers.
10. **test_mmap** `cannot close exported pointers exist` (paired runs): export counters were
    plain `+= 1`/`-= 1` from several threads. mmap (getbuffer also under the object lock),
    memoryview managed buffers and views, and array now count atomically.
11. **gc.get_objects() from `__del__` during a collection**: fixed here with a
    thread-local, then superseded by scale1's identical `COLLECTOR` check in core (merge
    kept scale1's; `test_stop_the_world` and `with_world_stopped` use it too).
12. **Cycles not collected while a second thread exists** (coordinator's late-freed watched
    type; 10-line reproducer: with one extra blocked thread, `gc.collect()` collected
    nothing): `del x` of a global releases the dict value through QSBR, into the dropping
    thread's batch, which matures only at that thread's later polls; the collector saw the
    cycle as referenced. scale1's `take_all_for_gc` (core) makes items due but releases them
    only after the collection (or at each owner's next poll). Now a collection first stops
    the world, takes every thread's retired items and the orphans (`qsbr::take_for_gc`),
    restarts, releases them (`release_taken_now`), then collects. Also covers a thread that
    dropped references and then blocked (tests: references released while the worker is
    still blocked).
13. **test_type attr cache consistency** (core regression; the coordinator suspected tail2's
    tag budget): `type.__setattr__` stored into the class dict first and reset the version
    tags after, so between the two a reader could see the new value in `C.__dict__`
    (lock-free dict read) and then the old one through a TYPE_ATTR cache hit. The store now
    resets the tags first, and `interp::cache::TYPE_WRITERS` (held across the store) keeps
    `assign_type_tag` from handing out a tag meanwhile (rechecked after the CAS). The tail2
    budget is fine (an exhausted class gets tag 0, every entry misses).
14. **test_monitoring test_register_callback** (core regression, ~1/2 runs): scale1's sharded
    refcounts made the replaced callbacks (nested functions dropped repeatedly by other
    threads) collector-freed only. Functions whose code is CO_NESTED are no longer sharded
    (CPython defers counts only for non-nested functions). Also: sharded.rs uses `ob_flags`
    bits 5-7, so the split-table bit moved to bit 4.
15. **test_types segfault (paired runs)**: `lang_util::set_field` (GenericAlias `__parameters__`
    set lazily by several threads, iterator fields) freed the old value at once while
    `field_ref` readers incref without a lock: QSBR swap while multi-threaded.
16. **Objects owned by idle threads never released** (coordinator/tail2: test_release_task_refs,
    concurrent.futures test_free_reference / test_no_stale_references): references other
    threads drop to objects owned by a thread that is blocked go to its merge queue, which it
    only processes when it runs. tail3 need not add its drain fix: I did both parts —
    `threads::blocking_pure(drain, f)` for waits that touch no object (lock acquire, queue
    get, time.sleep, select/poll/epoll, Condition waits): it first drains the merge queue
    (lock/queue/sleep only: no runtime lock held there) and then sets
    `ThreadState::merge_direct` (under the queue mutex) for the duration of the wait; a
    thread dropping a reference to such a thread's object folds it in itself
    (`object::merge_queued_nodealloc` under the owner's queue mutex, dealloc after
    unlocking); the owner clears the flag under the same mutex before touching objects
    again. The GC's stop-the-world never touches detached threads' queues.
17. **faulthandler dump of all threads** (coordinator, ~1/9 segfaults in
    test_dump_traceback_max_threads): other threads' frame chains were walked while those
    threads pushed/popped frames. `faulthandler.dump_traceback(all_threads=True)` now runs
    with the world stopped (`gc::with_world_stopped`), the watchdog
    (`dump_traceback_later`) with a bounded external stop
    (`gc::with_world_stopped_external`). Fatal-signal dumps stay best effort. (Not
    reproduced locally before or after.)

## Interface changes
* `gc::{test_stop_the_world, with_world_stopped, with_world_stopped_external}`,
  `qsbr::{has_pending_for_gc, take_for_gc, release_taken_now, Taken}`,
  `threads::blocking_pure`, `ThreadState::merge_direct`, `object::merge_queued_nodealloc`,
  `interp::cache::TYPE_WRITERS`, `dict::OBF_FROM_INSTANCE`, `list::reverse`,
  `super_::new_super_zero_arg`, `dictview::is_view_exact` now pub.
  `_testinternalcapi.test_stop_the_world`. capi/import.rs and capi/funcs.rs edits are
  flagged "stability3" (capi2's area). tier1.rs: one guard in LoadDeref (perf5's area).

## Checks
* tools/jit_tests/run.sh: all ok. tools/stress/run.sh 5x: all pass.
* Gate modules (test_threading test_io test_list test_json test_dict test_set test_deque
  test_collections test_itertools test_weakref test_gc test_opcache test_super
  test_functools test_capi), against the core binary with the same Lib: identical failure
  lists (test_capi 40 = 40; test_json passes, 2 failures in the starting snapshot).
  test_concurrent_futures: no deadlock (hang1's concern about releasing in other threads);
  the free_reference/no_stale_references/release_task_refs tests now pass;
  test_force_shutdown_during_max_tasks_worker_exit fails on core too.
  test_faulthandler: same 2 failures as core (test_fatal_error*).
* Micro benchmarks vs the starting snapshot: `list(d)` 5x faster; STORE_DEREF loop and
  generator loop unchanged (single-threaded paths keep plain stores).

## Left
* One run in ~16 paired (load 12+) was SIGKILLed during test_free_threading.test_str (seen
  twice, before and after the fixes). Peak RSS of a whole test_free_threading run is ~130 MB;
  not reproduced in 18 concurrent test_str runs nor with `ulimit -v`. Cause unknown (possibly
  the container's OOM killer acting on load from other agents).
* Between collections, QSBR batches of a thread blocked for a long time are released only
  when it runs again or at the next collection (item 12); waits not converted to
  `blocking_pure` (socket recv, os.read, subprocess waits...) still queue merges.
* Once, in a -j2 gate run, a child forked by test_threading.test_reinit_tls_after_fork hung
  for 300 s (fork hooks in the child also print NameError for builtins — on core too); not
  reproduced in 22 targeted runs.
* `take_for_gc`/scale1's `take_all_for_gc` touch other threads' QSBR `Local`s with the world
  stopped; a thread in Blocked mode that retired items while blocked would race (none known).
* `jit_send_drain` sets up the frame before claiming (only for just-created generator
  expressions owned by the consumer).
* gi_frame/gi_suspended/gi_running getters read the frame unclaimed (benign reads, but a
  racing close frees it).
