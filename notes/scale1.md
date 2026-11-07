# scale1 — multi-core scaling (cross-thread behaviour)

Branch `worktree-agent-a9654a52af555a4b2`. Everything written from scratch.

## Harness: `tools/bench/scaling/`

```
tools/bench/scaling/run.sh OUTDIR [workload ...]
   env: OURS (default /home/user/RPython/target/scale1/snap/bin/python3), IMPLS ("name=path ..."),
        THREADS ("1 2 4"), REPS (5), SCALE (1), TOL (0.3), TIMEOUT (300)
tools/bench/scaling/summarize.sh OUTDIR/raw.tsv        (STAT=min for best runs)
```

* `driver.py NAME N [SCALE]` (run by the interpreter under test): set-up, warm-up in the main
  thread (JIT tiers see the code first), then N threads released by a barrier, each doing the
  same fixed number of work units; timed from the barrier to the last join. It prints wall,
  this process's CPU time (user+sys from /proc/self/stat) and the whole machine's busy CPU time
  (/proc/stat) for the timed section; `others = busy - cpu` is the CPU other processes took.
* `run.sh` interleaves repetitions (rep → workload → N → interpreter, interpreter order rotated
  each rep) and records every run in `raw.tsv`. A run is *kept* when the other processes left at
  least N CPUs free on average (`others <= (NCPU - N + TOL) * wall`); `summarize.sh` uses the
  medians of kept runs (all runs, marked `*`, when none was kept) and, with `STAT=min`, the best
  runs (CPU: best of all runs — other processes only ever add time).
* Columns: `xN` = throughput at N threads / throughput at 1 thread (ideal N); `cpu-infl@N` =
  CPU time per unit of work at N threads / at 1 thread (ideal 1.00): it rises when threads slow
  each other down (contended atomics, cache-line ping-pong, spinning locks, GC/safepoint work)
  and is barely affected by other processes, so it stays meaningful on a loaded machine where
  the wall-clock speedup does not; `ours/ft wall@N` = absolute time relative to py-ft.
* The baselines (/opt/baseline/py-ft, py-default) are only ever run through run.sh (D15/D22).
* Workloads (`workloads.py`, units calibrated to ~0.4 s per thread on ours):
  - `cpu_*` independent work: nbody (float lists), oo (method calls/attributes on own objects),
    float (small objects with slots + math), gen (nested generators), comp (list/dict/set
    comprehensions), regex, json dumps/loads;
  - `shr_*` shared read-mostly: a dict (str/int keys), a list of tuples, module globals +
    builtins + shared functions, class attributes/properties/isinstance on one shared instance;
  - `alloc_*`: short-lived tuples/lists/instances, cyclic garbage (collector), dicts/strings;
  - `mut_*` contended: one queue.Queue, one dict written by all threads, a threading.Lock,
    append/pop on one list;
  - `jit_*`: a hot numeric loop and hot method calls compiled by the JIT, run by all threads;
  - `misc_*`: class creation, imports of fresh modules, calls into builtin modules.

Diagnostics: `RPY_SCALESTATS=1` prints at exit: non-owner refcount operations by type (runtime
paths), merge-queue hand-offs, objects switched to sharded counting, table evictions,
stop-the-world count / wait / parked time, collections and pause, QSBR batches and items.
`RPY_SHARD_MASK=<hex>` restricts sharded counting to some eligible types (0 = off; heap types are
always sharded) for experiments.

## What was slow (measured) and what changed

Measured with cpu-infl at 4 threads (py-ft: 0.95–1.10 on all `cpu_*`), `RPY_SCALESTATS` and
callgrind per-thread instruction counts (identical per thread at 1 and 4 threads for the
affected workloads: the extra CPU was cache-line traffic and lock spinning, not extra work).

1. **Atomic refcounts on shared objects** (cpu_oo 5.3x CPU at 4 threads, jit_calls 3.5–5x,
   cpu_comp 2.5–4.8x, nbody 2.3–2.7x): every thread increfs/decrefs the same function, method
   descriptor, module, builtin function, code constant (floats) and class objects — owned by the
   main thread, so every operation was a `lock add`/CAS on one cache line (21M non-owner
   member_descriptor increfs in cpu_oo at 0.3 scale, 3.6M float constant decrefs in nbody).
   **Fix: sharded reference counts** (`crates/runtime/src/sharded.rs`). An eligible object that a
   non-owner thread decrefs a 4th time (counter in `ob_flags` bits 6–7) gets the `SHARDED` bit
   (`ob_flags` bit 5) and a bias of 2^50 in its shared field; from then on non-owner increfs and
   decrefs go to a per-thread table (TLS, 256 buckets x 4 ways, address → delta; a slot whose
   delta is 0 is free, so references taken and dropped by a thread never leave it). Tables are
   folded in when a bucket overflows (one atomic add), at thread exit and at every collection
   (world stopped). Sharded objects are freed only by the collector: tracked ones by trial
   deletion (bias discounted), the others (str/int/float/bytes, untracked tuples, code) through
   a registry checked at each collection for a zero true count. Eligible: exact float, int, str,
   bytes, function, module, builtin function/method, method/classmethod/member/getset/wrapper
   descriptors, property, staticmethod, classmethod, code; tuples when untracked or old; dicts and
   lists only once old (survived a collection) — containers delay their items' release, so short-
   lived ones (thread args) stay exact (tools/stress/join_releases.py). Heap types use the same
   bit and bias (they had the bias already for per-thread instance counts). JIT tier-1/tier-2
   non-owner incref stubs test the bit and update the table inline (r15 = thread pointer, the
   table's TLS offset is fixed); other non-owner increfs keep the `lock add`.
   Before → after (cpu-infl@4, best runs): cpu_oo 3.5 → 1.3, cpu_comp 3.5 → 1.3, jit_calls
   3.2 → 1.6, shr_globals 1.3 → 1.05 (and see the table below).
   Collector interaction: no table may hold a delta for an object that returns to plain
   counting; the collector merges all tables when it stops the world and folds its own entries
   for objects it unshards (`flush_own_entry`; missing this freed live types — enum_composite
   crashed).

2. **QSBR took global locks on every poll** (qsbr.rs): `poll` called `min_seen`, which locked
   `ALL_THREADS` and `RECS` and cloned the thread list, at every eval-breaker visit while
   anything was pending (i.e. every backedge/call of a thread that stores attributes), batches
   of 128 items each did a handshake, and immortal objects were retired too. Rewritten: the
   thread list is mirrored into a fixed array under a sequence lock (`note_threads` takes the
   list), each thread state publishes its sequence in `ThreadState::qsbr_seen`, scans are
   rate-limited (cached minimum, capped by the GLOBAL read before the scan), batches hold up to
   1024 items, immortal objects are never retired, a thread re-arms its eval breaker a bounded
   number of times, and every collection releases everything retired before its stop
   (`take_all_for_gc`). Nothing is released inside `retire` any more (the caller may hold a
   leaf lock; releasing runs deallocators — this deadlocked weakref_race once the old code's
   release-on-full-batch path became frequent). Closing a batch is not a quiescent point (an
   earlier version of this rewrite reported one there: heap corruption).

3. **Dict reads locked the dict** (`ob_mutex` CAS + store per `d[k]`, `k in d`, `d.get`, and
   every LOAD_GLOBAL through `value_at_if`): shared read-mostly dicts (module globals, builtins,
   lookup tables) were a lock ping-pong. **Fix: lock-free dict reads** (types/dict.rs): lookups by
   identity or exact-str key (anything else falls back to the locked path) are validated by a
   per-table sequence lock (`wseq`, odd while a writer changes the index/entry layout); the data
   block now starts with its own (size, cap) and is published through `indices_p`, so a reader
   derives a consistent layout from one pointer; entries are written before the index that
   publishes them; replaced blocks go through QSBR — only once a thread other than the owner has
   touched the table (`shared_read`, set under the lock by readers and writers that are not the
   owner); removed/popped keys and values were already deferred (pop_last and
   `_weakref._remove_dead_weakref` now too). `dict.update` into an empty dict no longer
   overwrites the header with a fresh table (that freed the block under readers:
   test_free_threading.test_dict crashed).

4. **QSBR for thread-private containers**: in multi-threaded mode every list growth and dict
   resize retired the old array/block through QSBR (18,000 batches for 42,000 items in
   cpu_comp). Lists now defer only when not exclusively owned (owner thread, local count 1, no
   shared count: nobody else can read it); dicts use `shared_read`.

5. **JIT/interpreter single-threaded-only fast paths**: the tier-1 inline list append
   (comprehensions) and inline list iteration, and the interpreter's list iterator, took the
   generic/locked path as soon as a second thread existed. Now they test that the list (append)
   or the iterator (iteration) is exclusively owned; iteration reads the list like the lock-free
   subscript (null array/slot → generic path).

6. **Collector side**: garbage is made ownerless (`explicit_merge`) while the world is stopped,
   so the collector's tp_clear/dealloc on other threads' cyclic garbage no longer pushes every
   object through the owners' merge queues (1.15M queue hand-offs, each taking the global
   `ALL_THREADS` lock, in alloc_cycles at 0.3 scale → 37); the weakref detach step builds its
   hash set only when a callback needs it (8% of a cycle-heavy run).

7. **Contended object locks spun with sched_yield**: `ob_lock`'s slow path yielded up to 2000
   times before sleeping (whole cores burned on contended dicts/lists). It now spins briefly and
   parks with the PyMutex protocol (capi/pythread.rs `mutex_lock`); the guard's unlock and tier
   2's inline Unlock wake parked waiters (a plain store dropped their wake-up).

## Results

RESULTS_PLACEHOLDER

## What is left (measured causes)

* **Borrowed loads**: our interpreter and JIT incref every LOAD_FAST; a user object shared by
  all threads (shr_attrs: 10.5M non-owner decrefs of one `_Shared` instance at 0.3 scale) keeps
  one hot refcount line. CPython 3.14+ borrows such loads. Sharding user instances would make
  their release (weakref callbacks, `__del__`) wait for a collection — rejected.
* **Attribute stores retire the old value through QSBR** in multi-threaded mode even for
  thread-private objects (jit_calls: 8.8M retirements at 0.3 scale; the deferred frees lose
  allocator locality). Fix sketch: a per-object "read by a non-owner" bit set by lock-free
  attribute readers (runtime + both JIT tiers' inline loads, via an owner/bit check) before
  their first read; writers that own the object and see the bit clear (after a SeqCst fence)
  free at once. Needs changes in tier-1 `inline_values`/slot loads and tier-2 `values_word`.
* **Stop-the-world collector is serial**: GC-heavy workloads (alloc_cycles, cpu_float, class
  creation) spend most of their 4-thread time in collections with the other threads parked
  (alloc_cycles: 30 collections, 460 ms pause, 820 ms parked thread time at 0.3 scale). Parallel
  marking/clearing or a per-thread young generation would be needed.
* **Tier-1/tier-2 multi-threaded slow paths still taken**: StoreAttr (slots, inline values) and
  LOAD_GLOBAL inline paths are disabled once a second thread exists (they now hit lock-free
  helpers, but pay a call).
* `queue_object_for_merge` still takes the global `ALL_THREADS` lock per hand-off
  (producer/consumer patterns).

## Correctness

* tools/stress/run.sh: all 18 pass (new: sharded_refcounts.py, dict_lockfree_race.py), repeated.
* tools/jit_tests/run.sh: all pass.
* Suite modules (vs the "before" build): test_threading, test_thread, test_weakref, test_set,
  test_enum, test_functools, test_itertools pass; test_free_threading 6 failed files / 7
  failures, test_gc 1, test_sys 3, test_dict 8, test_descr 2, test_class 1, test_queue 1,
  test_collections 3, test_capi: the same failures as the before build except tests fixed on
  core after my base (dict watchers, bz2/lzma end of stream, monitoring, frozendict pickling,
  sys.getrefcount emulation D27: test_list.test_list_overwrite_local) — none in my areas.

## Files

crates/runtime/src/{sharded.rs (new), scalestats.rs (new), qsbr.rs, object.rs, threads.rs, gc.rs,
types/dict.rs, types/list.rs, interp/jit/tier1.rs, interp/jit/opt/codegen.rs, interp/jit/helpers.rs,
interp/specialize.rs, types/weakref.rs, modules/_weakref.rs, lib.rs}; small call-site changes in
capi/pystate.rs and subinterp/mod.rs (`qsbr::note_threads(&all)`); tools/bench/scaling/*,
tools/stress/{sharded_refcounts,dict_lockfree_race}.py, tools/bench/README.md (one line).
