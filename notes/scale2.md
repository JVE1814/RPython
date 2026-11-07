# scale2 — multi-core scaling, second round

Branch `worktree-agent-a2c05f35d22b13e94` (base: core with scale1 merged; core merged
again twice: stability3, then tail3 + pdb1). Everything written from scratch.
Harness, counters and terminology: notes/scale1.md (`tools/bench/scaling/`,
`RPY_SCALESTATS=1`, "cpu-infl@4" = CPU time per unit of work at 4 threads / at 1 thread).

## Changes (measured cause → fix)

1. **Attribute stores deferred every old value through QSBR once a second thread
   existed** (jit_calls: 8.8M retirements at 0.3 scale; cpu_oo, cpu_float likewise).
   New `ob_flags` bit `XREAD` (bit 4, `qsbr::XREAD`): a thread other than the owner sets
   it (locked `or` + SeqCst fence) before its first lock-free read of an object's
   attribute storage (inline values, `__slots__`/member slots). Writers swap the value
   with a SeqCst RMW and then read the flags: owner + bit clear → release at once
   (Dekker argument in qsbr.rs). Readers that announce: `shape::get`, `shape::get_hinted`,
   the specialiser's SLOT hit, `member_get`, `gc.get_referents`, tier-1 inline
   LoadAttr (INSTANCE_DICT/SLOT: `emit_note_attr_read`), tier-2 `Inst::NoteRead` before
   both value loads. Writers: `shape::set_ix/replace_hinted` (`release_old`),
   member set/delete and the specialiser's SLOT store (`qsbr::swap_attr`), the JIT stores
   below (`qsbr::release_attr` semantics). jit_calls cpu-infl@4 2.2 → 1.0, the
   retirements disappear (8.8M → 36).
   Later extension (coordinator/tail3 request: finalization lagged behind replaced
   fields): a replaced value is also released at once when every other thread is
   quiescent (`qsbr::others_quiescent`: not in VM mode; Dekker with `enter_vm`'s mode
   store). Used by `release_attr`, the JIT stores' deferred path (`h_decref_deferred`)
   and the new `qsbr::swap_field_unlocked` (exception fields, function attributes,
   `obj.__dict__ = ...`); other `swap_field` callers (117, some under object locks)
   still always defer.

2. **JIT fast paths that were single-thread only.**
   * Tier 1 StoreAttr SLOT: `xchg` + inline release decision; INSTANCE_DICT: object
     mutex taken inline (`lock cmpxchg` 0→1, contended → helper), shape re-checked under
     it, plain store, inline unlock (parked waiters woken through the runtime), release
     decision after the unlock. Tier 2 the same with new IR instructions
     `TryLockFlag`, `Xchg`, `ReleaseAttr` (deferred release through a register-preserving
     routine), for replace and add (shape transition) stores.
   * Tier 1 LoadGlobal: the inline entry read now also runs with threads, validated by
     the dict table's sequence lock (`wseq` even and unchanged, keys version re-checked,
     index < nentries); allowed when the table is `shared_read` (replaced values go
     through QSBR) or the reader owns the dict. Stress test: tools/stress/global_rebind_race.py.
   cpu_oo/jit_calls 1-thread time in multi-threaded mode: ratio to true single-thread
   1.5–2.5x → ~1.0–1.3x.

3. **Shared counters written on every execution** (the biggest remaining cpu_oo,
   nbody, float cost after 1–2): tier-1 type profiles did an unconditional `or` into the
   shared profile word on every fast-path execution; the tier-2 call countdown was
   decremented on every call forever (callpy, `bump_invocations`), the tier-1 backedge
   countdown on every backedge, the invocation count atomically on every call /
   generator start and the interpreter's backedge count with `lock xadd`. Profiles are
   now written only when a bit is new (`test8_mi` per bit, write in a stub), countdowns
   stop at expiry (h_backedge parks the backedge counter at -1), invocation counts only
   until tier 1, interpreter counter a plain saturating update. cpu_oo cpu-infl@4
   3.0 → 1.05, nbody 1.34 → 1.08, float 1.9 → 1.2 (same machine state, old/new interleaved).

4. **Dict reads with non-str keys took the lock** (shr_dict's `i in d` with int keys).
   The lock-free probe now decides any comparison `plain_eq` can (same-type
   str/int/float/tuple) and str vs int/float (never equal) without the lock.

5. **Class creation was quadratic**: `tp_subclasses` was a Vec with linear
   `contains`/`retain`; `object` has every class as a subclass and with 4 threads more
   dead classes wait for the collector (misc_classes: `remove_subclass` 17% of all
   instructions at 4 threads, 3% at 1). Now `SubList` (insertion order, hash index,
   holes compacted). misc_classes 4T workers' instructions = 1T's; cpu-infl 4.5 → ~2.

6. **Merge-queue hand-off took the global `ALL_THREADS` lock** (mut_list: 366k
   hand-offs). The owner is found in the lock-free thread list (`qsbr::find_threads`);
   liveness is checked under the owner's queue lock, under which every exit path now
   clears `alive` before its final drain. mut_list cpu-infl 6.1 → 1.9.

7. **Prompt deallocation (coordinator, top priority):** test_concurrent_futures
   regressed after scale1 (objects whose last reference another thread dropped sat in
   the merge queue of an owner blocked in a wait; a waiting thread kept its QSBR items
   for the whole wait, e.g. a frame holding the executor during `join_thread`).
   After merging stability3 (which fixed the merge-queue half independently with
   `blocking_pure`/`merge_direct` and `qsbr::take_for_gc`), ONE mechanism remains:
   stability3's, plus (a) `merge_direct` folds only for owners of the dropping thread's
   interpreter (`threads::same_interp`; cross-interpreter folding ran subinterpreter
   weakref callbacks on other threads and leaked the global import lock:
   InterpreterPool hang), (b) `blocking_pure(drain=true)` also releases matured QSBR
   batches (`qsbr::before_wait(false)`: no batch close, no spin), (c) thread join and
   subinterpreter finalization (before `finalize_modules`) call `threads::before_wait()`
   (close the batch, poll, wait ≤2 ms for running threads to report), (d) collections
   merge the merge queues of same-interpreter threads while the world is stopped
   (fallback for owners blocked in non-pure waits). My first version (`try_hold`, a
   per-thread handshake executed by the requester) was removed in the unification.

## Results

Measurement caveat (D22): the machine had 2–3 CPUs busy with other agents throughout;
4-thread runs rarely passed the contention filter, so wall speedups are lower bounds and
cpu-infl varies with the load (contended workloads look *better* when threads cannot run
simultaneously). Tables use best runs (`STAT=min`) with old/new interleaved in the same
harness run where a before/after is claimed. See the final table in the agent report
(also below once filled in).

FINAL_TABLE_PLACEHOLDER

## What is left (measured causes)

* **shr_attrs (cpu-infl ~2–7 depending on load, py-ft 1.0)**: every use of the shared
  instance `o` increfs/decrefs it from 4 threads (9M non-owner decrefs at 0.3 scale):
  tier 2 calls `o.compute()` / `o.prop` through the tier-1 call template with an owned
  reference to `o`, and the callee frame owns its arguments. Fix = borrowed stack/argument
  references (item 3 of the brief; D27 would then become unnecessary) or inlining those
  calls; not done (large change across interpreter frames and both JIT tiers).
* **mut_list / mut_dict / shr_dict**: same root cause on the shared container
  (LOAD_FAST incref of the shared list/dict, its refcount and `ob_mutex` share the
  ABI-fixed header line). Sharding young containers was rejected by scale1 (delays the
  items' release until a collection).
* **misc_classes**: collector work is ~20% of all instructions and runs on one thread
  (type deallocation, malloc arena contention for cross-thread frees); parallel marking
  and sweeping (item 4 of the brief) not done.
* **alloc_cycles / cpu_float**: stop-the-world waits (threads descheduled by the loaded
  machine delay every collection's stop: 40–80 ms total per run at 4 threads) and serial
  collection.
* test_asyncio sslproto `*_failed_certificate_no_cycles` still fail: the transport stays
  alive even after the server thread was joined and a full collection ran, with no
  tracked referrer of the Future holding it (a leaked reference, not deferral; same on
  the pre-scale2 build). test_open_connection_happy_eyeball_refcycles passes.

## Correctness

tools/stress/run.sh (20 scripts, new: attr_owner_race.py, global_rebind_race.py) passes
repeatedly; tools/jit_tests/run.sh passes. Running the jit tests with an idle second
thread (all multi-threaded paths) only differs in finalization timing of deferred
objects (t_inline `P.alive`, t_unpack `__del__` log), as on the base build.
Negative control: with the XREAD check disabled, attr_owner_race.py fails (freed values
read: AttributeError/garbage) — the test detects the race the bit prevents.
Suite gate: see GATE_PLACEHOLDER.

## Files

crates/runtime/src/{qsbr.rs, shape.rs, threads.rs, gc.rs, subinterp/mod.rs, types/descr.rs,
types/dict.rs, types/typeobject.rs, types/object_type.rs, types/exceptions.rs,
types/funcobject.rs, modules/gc_mod.rs, modules/_thread.rs, interp/eval.rs,
interp/specialize.rs, interp/jit/{tier1.rs, x64.rs, helpers.rs, mod.rs, callpy.rs},
interp/jit/opt/{ir.rs, codegen.rs, build.rs, mod.rs}}; tools/stress/{attr_owner_race,
global_rebind_race}.py.
