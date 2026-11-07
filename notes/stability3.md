# stability3 — test_free_threading clean, races in the object machinery

Goal: `python -m test test_free_threading` passes as on free-threaded CPython (no failures,
no "env changed"), also under load (two copies at once, load average 8-15).

## Before / after

* Before (master d0f295b, 1 run): 1 failure (test_threading) + env changed in test_cprofile,
  test_io, test_races; test_json / test_list env changed in other runs (2/3 runs each).
* After (installed-layout snapshot, merged master 604b958 + later commits): 57/57 OK in
  every single run (5 runs), and in paired runs under load 31/32 clean; the failures seen in
  paired runs were all root-caused and fixed (below), except one unexplained SIGKILL during
  test_str (see "Left").

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
     without a lock. Now an atomic swap with QSBR-deferred release while multi-threaded
     (plain store single-threaded). `cell_get` is an acquire load.
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
11. **gc.get_objects() from `__del__` during a collection** (coordinator, capi2's hang):
    `all_tracked` waited for COLLECTING, held by its own thread. A thread-local
    `IN_COLLECTION` marks the collecting thread; nested calls skip the wait (and the stop if
    the world is already stopped by us). Same for `test_stop_the_world`.
12. **Cycles not collected while a second thread exists** (coordinator's late-freed watched
    type; found with a 10-line reproducer: with one extra blocked thread, `gc.collect()`
    collected nothing): `del x` of a global releases the dict value through QSBR, i.e. into
    the dropping thread's batch, which only matures at that thread's later polls; the
    collector saw the cycle as referenced. A collection now first stops the world, takes
    the collector's own batches and all orphaned batches (all due with the world stopped),
    restarts, releases them, then collects (`qsbr::take_for_gc`/`release_taken`).
    Batches of *other* (blocked) threads are still not reachable (see Left).

## Interface changes
* `gc::test_stop_the_world()`, `qsbr::{has_own_pending, take_for_gc, release_taken, Taken}`,
  `dict::OBF_FROM_INSTANCE`, `list::reverse`, `super_::new_super_zero_arg`,
  `dictview::is_view_exact` now pub. `_testinternalcapi.test_stop_the_world`.

## Checks
* tools/jit_tests/run.sh: all ok. tools/stress/run.sh 5x: all pass.
* Gate modules (test_threading test_io test_list test_json test_dict test_set test_deque
  test_collections test_itertools test_weakref test_gc test_opcache test_super
  test_functools test_capi): same failures as the starting snapshot except test_json now
  passes (2 failures before) and test_capi far fewer (master's capi2 work).
* Micro benchmarks vs the starting snapshot: `list(d)` 5x faster; STORE_DEREF loop and
  generator loop unchanged (single-threaded paths keep plain stores).

## Left
* One run in ~16 paired (load 12+) was SIGKILLed during test_free_threading.test_str (seen
  twice, before and after the fixes). Peak RSS of a whole test_free_threading run is ~130 MB;
  not reproduced in 18 concurrent test_str runs nor with `ulimit -v`. Cause unknown (possibly
  the container's OOM killer acting on load from other agents).
* QSBR batches of threads that block for a long time (threads left over by earlier tests)
  are still released only when that thread runs again: hang1's prototype (hand batches to
  ORPHANS in `threads::blocking`) deadlocked test_concurrent_futures; not retried.
* `jit_send_drain` sets up the frame before claiming (only for just-created generator
  expressions owned by the consumer).
* gi_frame/gi_suspended/gi_running getters read the frame unclaimed (benign reads, but a
  racing close frees it).
