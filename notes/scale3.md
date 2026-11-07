# scale3 — parallel, owner-affine young collections; STW latency; allocator depot

Branch `worktree-agent-a34ca6fdf0716d432` (base: core cdcbad1; core merged again: refs1
borrowed stack references, perf7, cleanroom5). Everything written from scratch.
Harness, counters and terminology: notes/scale1.md (`tools/bench/scaling/`,
`RPY_SCALESTATS=1`, cpu-infl@4 = CPU per unit of work at 4 threads / at 1 thread).

## What was measured first

New per-phase counters (`RPY_SCALESTATS=1`: thread CPU and wall per collector phase,
world-stopped time, objects examined/unreachable, parallel jobs/helps, garbage handed to
owners, allocator depot traffic; `RPY_GCDEBUG=1` prints one line per collection).
On the base build, alloc_cycles at 4 threads (scale 0.5): 65 collections examined 2.7M
objects, i.e. 41k per collection against 2.4k at one thread, at ~330 ns of collector
CPU per object against ~100 ns; the world was stopped 975 ms. Three causes:

1. **Remote memory.** One thread analysed and cleared everybody's garbage: every object
   was a cache miss into another core (the same young set analysed on one thread costs
   ~25% more when it is 15x larger, not 3x).
2. **Snowball.** While one thread cleared, the others allocated, the garbage freed by
   the collector cancelled their allocation counts (allocations minus deallocations),
   requests made during the collection were dropped, and `LAST_LIVE` counted what was
   allocated meanwhile: each collection made the next one later and larger (seen as
   `since=` 3.6k, 18k, 28k, 44k, 73k, 128k, 461k in successive collections). The base
   build typically ends up with one thread doing all collections for 2 s while the
   others finished in 0.26 s.
3. **Serial stop-the-world work and a second stop per collection** (QSBR items were
   taken in an extra stop before every collection with pending items).

## Changes

1. **Per-thread young buffers** (`young.rs`). Tracking an object as young appends it
   to its thread's buffer (a `#[thread_local]` Vec; new gc bit `GC_BIT_YOUNG_BUF` 0x08).
   Young collections take their candidates from the buffers instead of walking every GC
   page (cost follows the young set, not the heap). Entries go stale when objects die;
   blocks of the small-object allocator are never unmapped, so stale entries are read
   safely, and the claim (bit test-and-clear) drops stale entries and duplicates. Large
   objects are never buffered (system allocator; found through the large registry).
   Buffers compact themselves (stale entries only; no object written). Untracking clears
   the bit; `alloc::realloc` re-appends a moved young object. Full collections clear the
   bits during their walk. fork: the buffer registry lock is held across fork.
2. **Parallel analysis by the stopped threads** (`gcpar.rs`). The collector publishes
   the analysis as a job: one task per chunk (2048 entries) of each thread's buffer,
   one root task per thread (its frames), one for `sys`; full collections use tasks of
   8 GC pages. Threads parked in `gc::wait_for_release` join (futex wake-up), take their
   own buffer's and frames' tasks first, then any; threads blocked in native calls do
   not take part (their tasks are done by the others). Phases: claim+promote+init
   owner words / roots + subtract internal references (concurrently: subtracting from an
   object marked later is harmless) / externally referenced objects marked, marking to
   global termination with work packets / restore owner words, collect garbage. Same
   result set as the serial `find_unreachable`. Used when somebody can help and the
   young set has >= 4096 entries; `RPY_GC_PARALLEL=0` disables it.
3. **Owner-side clearing** (`gc::owner_clear`). When the garbage needs nothing but
   tp_clear (no finalizers, no legacy finalizers, no weakref callbacks, not
   DEBUG_SAVEALL, automatic collection), garbage owned by a thread parked at an
   eval-breaker safepoint (`ThreadState::at_safepoint`) is handed to it
   (`ThreadState::gc_clear`, `EVAL_GC_CLEAR`); it clears it in `handle_eval_breaker`
   right after the restart (also at thread exit). The collector clears the rest.
   Effects: frees stay in the owner's cache and allocator free lists, and threads that
   make garbage pay for clearing it (self-throttling: no snowball). gc.collect() and the
   finalizer path are unchanged (serial, collector-cleared, PEP 442 re-check).
   Workers take references only to plainly counted garbage; for sharded/type-id objects
   the collector takes them before unsharding (a delta left in a helper's table for an
   object unsharded and freed afterwards corrupted the heap: misc_import aborted 1 in 3
   runs; tools/stress/gc_import_race.py reproduces it on that build).
4. **Collection interval.** Allocations made while a collection runs count towards the
   next one (gross count, `ALLOCS_GROSS`), requests dropped during a collection are
   re-issued at its end, and `LAST_LIVE` is the survivor count at the stop. Young
   intervals adapt (`YOUNG_SCALE` 1..16: doubles after a collection that found < 1/64
   garbage, back to 1 after one that found >= 1/4; reset by gc.collect(); not applied to
   a user threshold). cpu_float: 50 collections -> 7 per run. The extra QSBR stop runs
   only for explicit collections.
5. **Parking without polling.** Parked threads sleep on a futex (`RESTARTS`) instead of
   yield/20-200 us sleep loops; the stopping thread sleeps on `ARRIVALS`, which every
   thread becoming safe while a stop is pending bumps (`gc::note_safe`, called from
   `ModeCell::set` and `enter_vm`).
6. **Allocator depot round trips** (`tlab.rs`). Per-slot thread-cache limits adapt:
   a slot that spilled to the depot and then took blocks back doubles its limit (up to
   1 MiB of blocks). Depot batches at one thread: cpu_float 10.8k -> 2.1k, alloc_cycles
   10.2k -> 263, cpu_comp 4.3k -> 133. JIT-inlined frees still stop at 256 and take the
   generic path beyond.
7. **Private iterators skip the object-lock table** (`objlock::lock_held_unless_exclusive`):
   enumerate/reversed `__next__` and dict view iterators took a sharded table lock per
   element once a second thread existed; an iterator whose only reference is the
   caller's (a running for loop) is not reachable by other threads. cpu_comp in
   multi-threaded mode: 1285M -> 1099M instructions (= single-threaded mode).

## Results

(filled in below from the final harness run)

## What is left

* The finalizer path (garbage with __del__/tp_finalize or weakref callbacks) is still
  analysed in parallel but finalized and cleared by the collector alone.
* misc_classes: clearing costs 2.2x the CPU per object at 4 threads (type deallocation
  takes several global locks: TYPE_COUNTS, SUBCLASSES_LOCK, weakref list lock, shape
  table) and the 4-thread instruction count equals 4x one thread's: the rest is cache
  traffic on shared type machinery.
* Garbage of threads not parked at a safepoint (blocked, in native code) is cleared by
  the collector (cross-thread frees -> depot traffic remains at 4 threads).
* Time-to-safepoint for threads in long Rust loops (no poll) is bounded only by the
  100 ms stop deadline.
* cpu_comp/cpu_float/shr_*/mut_list: no GC cause; equal instruction counts at 1x4 and
  4 threads (cache-level effects: refcount lines of shared objects, scale2 notes).

## Correctness

tools/stress/run.sh (22 scripts, new: gc_parallel.py, gc_import_race.py) and
tools/jit_tests/run.sh pass. Gate (installed-layout snapshots, base cdcbad1 vs this
branch with core merged): test_gc test_weakref test_finalization test_capi
test_threading test_thread test_free_threading test_sys test_dict test_list test_set
test_descr test_class test_queue test_concurrent_futures test_asyncio test_generators:
identical failure lists (test_gc 2, test_descr 1, test_asyncio 2, as in the base).
Single-thread: gc_collect / gc_traversal instruction counts unchanged (1423M -> 1421M,
610.5M -> 610.7M for fixed loops); an allocation-heavy micro benchmark +0.9%
(young-buffer appends and claims); a 200k-object live heap -1.5%.

## Files

crates/runtime/src/{young.rs (new), gcpar.rs (new), gc.rs, gc_support.rs, scalestats.rs,
threads.rs, alloc.rs, tlab.rs, fork.rs, lib.rs, objlock.rs, qsbr.rs (diagnostic len),
interp/eval.rs (eval breaker: safepoint_poll_eval, clear_handed), types/weakref.rs
(gc_weak_state), types/enumerate.rs, types/dictview.rs};
tools/stress/{gc_parallel.py, gc_import_race.py}.
