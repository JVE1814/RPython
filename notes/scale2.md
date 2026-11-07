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

Final run A (18:00–18:15, load ≈ 12–14, `REPS=3 THREADS="1 4"`, before = my base snapshot
1249f42 (core with scale1), ours = 03d2501 without item 8, py-ft) and final run B
(18:19–18:25, load ≈ 7–9, ours = 086feb6+core merge, py-ft). Best runs. Under the very
high load of run A threads rarely ran simultaneously, which hides contention for every
interpreter; run B shows more of it. cpu-infl@4 (ideal 1.00):

| workload | before (A) | ours (A) | py-ft (A) | ours (B) | py-ft (B) | ours/ft wall@1 (B) |
|---|---|---|---|---|---|---|
| cpu_nbody | 1.03 | 0.93 | 1.03 | **1.01** | 1.01 | 0.46 |
| cpu_oo | 1.38 | 0.97 | 1.05 | **1.07** | 1.09 | 0.59 |
| cpu_float | 1.26 | 1.17 | 1.12 | 1.27 | 1.03 | 0.83 |
| cpu_gen | 1.10 | 1.02 | 1.00 | 1.06 | 1.05 | 0.99 |
| cpu_comp | 1.21 | 1.08 | 1.00 | 1.29 | 1.06 | 1.77 |
| cpu_regex | 1.24 | 1.09 | 1.11 | **1.11** | 1.19 | 0.46 |
| cpu_json | 1.02 | 0.98 | 1.02 | **0.98** | 1.08 | 0.81 |
| shr_dict | 0.95 | 1.01 | 1.30 | 2.02 | 1.37 | 0.97 |
| shr_list | 0.76 | 0.74 | 1.01 | **0.94** | 1.44 | 0.83 |
| shr_globals | 0.98 | 1.02 | 1.06 | 1.07 | 1.02 | 0.55 |
| shr_attrs | 1.20 | 1.05 | 1.06 | 2.05 | 1.00 | 0.54 |
| alloc_small | 1.04 | 1.10 | 0.98 | 1.06 | 1.05 | 0.65 |
| alloc_cycles | 1.38 | 1.59 | 0.89 | 1.72 | 0.82 | 0.50 |
| alloc_dicts | 1.02 | 1.04 | 1.04 | 1.05 | 1.00 | 1.53 |
| mut_queue | 1.01 | 1.01 | 1.15 | **1.09** | 2.22 | 1.51 |
| mut_dict | 1.05 | 1.13 | 1.03 | 3.10 | 1.98 | 1.45 |
| mut_lock | 1.02 | 1.13 | 1.02 | 1.58 | 1.34 | 1.15 |
| mut_list | 1.19 | 1.33 | 1.15 | 2.54 | 1.55 | 1.19 |
| jit_loop | 1.02 | 1.01 | 0.98 | 1.04 | 1.03 | 0.10 |
| jit_calls | 1.63 | 1.04 | 1.06 | 1.10 | 1.02 | 0.49 |
| misc_classes | 3.87 | 1.49 | 1.36 | 1.62 | 1.49 | 0.72 |
| misc_import | 0.98 | 0.92 | 0.89 | 1.33 | 1.30 | 1.31 |
| misc_cext | 1.21 | 1.00 | 0.93 | 1.07 | 1.03 | 1.15 |

(bold: at least as good as py-ft in run B.) Same-load before/after pairs measured during
the work (old and new binaries interleaved in one harness run, best runs): jit_calls
2.19 → 1.00, cpu_oo 2.97 → 1.05, cpu_float 1.92 → 1.19, nbody 1.34 → 1.08, mut_list
6.08 → 1.88, misc_classes 4.54 → 1.94, shr_dict 5.84 → 1.94, mut_dict 2.41 → 1.86;
shr_attrs (single runs, 4 threads, 264 units): 2.47 s wall / 7.1 s CPU → 0.38 s / 0.22 s
after item 8.

8. **(late) Tier-2 class attributes read through instances** (`self.LIMIT`,
   `CLASS_ATTR_SHADOWABLE`): handled in tier 2 with the shape-lacks guard used for
   shadowable methods. Methods that read class constants through `self` could not be
   inlined before (the inliner rolled back at that LoadAttr), so every call passed an
   owned reference to the receiver: shr_attrs' shared instance went from 9.0M to 1.6M
   non-owner decrefs per run. jit test: tools/jit_tests/t_shadow.py.

## What is left (measured causes)

* **shr_attrs (cpu-infl 2.05 in run B, py-ft 1.0)**: after item 8 one owned reference to
  the shared instance per iteration remains (1.6M non-owner decrefs at 0.3 scale): `o.prop`
  (a property; tier 2 calls the getter through the tier-1 template with an owned `self`)
  or `isinstance(o, _Base)`. The general fix is borrowed stack/argument references
  (item 3 of the brief; D27's getrefcount emulation could then go) — not done (frames
  would need tagged borrowed slots in the interpreter and both JIT tiers); a smaller step
  is inlining property getters in tier 2.
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
Suite gate (installed-layout snapshot, vs my base snapshot 1249f42): test_threading,
test_thread, test_free_threading, test_weakref, test_list, test_set, test_class,
test_queue, test_collections, test_itertools, test_functools, test_concurrent_futures
(test_interpreter_pool env changed as before), test_finalization, test_monitoring pass;
test_gc (2), test_sys (1), test_dict (1), test_descr (1), test_opcache (1), test_capi
(40 failures/13 errors) fail exactly the same tests as the base build; test_asyncio 2
(the sslproto no_cycles tests above; base snapshot additionally failed others since fixed
on core). The base build also failed test_free_threading (2) and test_class (1).
Single-thread instruction counts (callgrind, tools/bench/micro, 100000 loops, base →
new): attr +0.1%, calls +1.7%, globals_ -0.4%, method +0.3%, gen -0.4%, listdict -0.3%,
floatops -0.7%.

## Files

crates/runtime/src/{qsbr.rs, shape.rs, threads.rs, gc.rs, subinterp/mod.rs, types/descr.rs,
types/dict.rs, types/typeobject.rs, types/object_type.rs, types/exceptions.rs,
types/funcobject.rs, modules/gc_mod.rs, modules/_thread.rs, interp/eval.rs,
interp/specialize.rs, interp/jit/{tier1.rs, x64.rs, helpers.rs, mod.rs, callpy.rs},
interp/jit/opt/{ir.rs, codegen.rs, build.rs, mod.rs}}; tools/stress/{attr_owner_race,
global_rebind_race}.py.
