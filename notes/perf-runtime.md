# perf-runtime workstream — object model, call path, memory, reclamation, scaling

Branch: `worktree-agent-a022a149fbcfc089e`. Everything written from scratch.

## Inline instance attributes ("shapes") — `crates/runtime/src/shape.rs`
* Ordinary classes (heap type, managed `__dict__`, `tp_itemsize == 0`, `tp_alloc ==
  generic_alloc`) get `Py_TPFLAGS_INLINE_VALUES` and a per-type `ShapeTree` stored in
  `PyHeapTypeObject.ht_cached_keys` (`shape::type_enable`, called from `type_new`).
* `generic_alloc` appends a values area after `tp_basicsize`:
  `Values { shape: *Shape, cap: u32, _pad }` followed by `cap` value slots.
  Capacity = clamp(len(`__static_attributes__`), 4, 30), learned upwards when an
  instance overflows (new instances get room for 1.5x the overflowing count).
* The managed-dict word in the pre-header (`alloc::managed_dict_slot`) encodes the
  state: `0` = no dict, `values | 1` = inline values live, otherwise a materialised
  dict (authoritative from then on).
* `Shape` = immutable ordered list of interned names + lock-free transition list;
  shapes live until their type is deallocated (instances keep the type alive;
  `__class__` assignment materialises first). Shape ids are globally unique u32.
* Materialisation (`shape::materialize`) happens on: `__dict__`/`vars()`/
  `typeobject::dict_slot` (every C-API path that hands out the dict pointer:
  `_PyObject_GetDictPtr`, `PyObject_GenericGetDict`, ...), > 30 attributes or a full
  tree (256 shapes), deleting an attribute that is not the last one added, non-str
  attribute names, `__class__` assignment. `del o.__dict__` leaves dict mode.
* Paths that never materialise: generic getattr/setattr, method lookup shadow checks
  (eval.rs `load_method`), the inline caches (specialize.rs), the tier-1 JIT,
  dealloc/traverse/clear (`dict_slot_raw` + `shape::traverse/clear_values`),
  `PyObject_VisitManagedDict`/`PyObject_ClearManagedDict`.
* `shape::has_inline_values(o)` implements `_testinternalcapi.has_inline_values`
  semantics (true while the attributes are inline). Known difference from CPython:
  CPython keeps the values inline after `__dict__` is requested (its dict can point at
  the inline values); here `__dict__` access switches the object to dict mode, so
  test_class.TestInlineValues.test_inspect_dict/test_update_dict would fail once
  `_testinternalcapi` exists.
* Threads: writers lock `ob_mutex` once a second thread exists
  (`qsbr::multi_threaded()`); readers are lock-free; replaced/removed values are
  released through `qsbr::decref_deferred`.

### For jit2 (inline caches)
* The INSTANCE_DICT cache kind now covers both modes: `b` = hint = index in the
  inline values *or* entry index in the dict. specialize.rs uses
  `instance_get_hinted` / `instance_index` / `shape::replace_hinted` / `shape::set_ix`.
* tier1.rs: `inline_values()` emits the values fast path for LoadAttr INSTANCE_DICT
  (managed word tagged → shape → keys[hint] == name → value); `managed_dict()` sends
  tagged words to the slow path (so METHOD_SHADOWABLE / StoreAttr go through helpers
  for values-mode objects — candidates for inline code).
* Guards for tier 2: `shape::shape_of(o)` (null in dict mode), `(*shape).id` (u32,
  unique, never reused), `shape::jit_layout()` gives the byte offsets (values.shape,
  first value slot, shape.id, shape.len, shape.keys). Same shape pointer => same
  attribute at the same index; type-attribute changes are still guarded by the type
  version tag (shapes do not describe class attributes).

## Instantiation fast path — `object_type.rs::type_vectorcall`
`PyType_Type.tp_vectorcall_offset` now points at `tp_vectorcall`, and heap classes get
`tp_vectorcall = type_vectorcall`: for classes with `object.__new__` and a Python
`__init__` the instance is allocated directly and `__init__` is called through
`interp::call_function` with `self` prepended (no args tuple, no kwargs dict, no
`slot_tp_init`). Everything else goes to `type_call` as before (kwnames → dict).

## Calls — `interp/frame.rs`
* `Frame::new_call`: plain call frames borrow `code`/`func` from the caller
  (`owns_fc = false`; `make_owned()` before a frame outlives its call: generator
  creation, `frame_finished` handing the frame to its frame object). Replaced
  `func.__code__` objects are retired, not freed (funcobject.rs). Saves 4 refcount
  operations per call (atomic ones when another thread owns the function).
* Frame pool: plain `#[thread_local]` Vec (no lazy-init/RefCell), frames reset in
  `recycle`, locals+stack released in one pass; drained at thread exit.
* `interp::jit::call_compiled` (jit2's file) uses `Frame::new_call` (one-line change).

## Safe memory reclamation — `crates/runtime/src/qsbr.rs` (D5/D6)
Quiescent-state based: `GLOBAL` sequence, per-thread published sequence, batches of
retired items (decref or raw free) with goal `GLOBAL+1`; closing a batch sets
`EVAL_QSBR` (bit 5) in every thread's eval breaker (handshake); threads report at
their next poll (`eval::handle_eval_breaker`); threads in native/blocked mode count as
quiescent; exiting threads hand pending batches to an orphan list. Single-threaded
processes never defer. API: `qsbr::decref_deferred(p)`, `qsbr::free_deferred(p, f)`.

Dict: replaced values, removed keys/values and `clear()` contents go through
`qsbr::decref_deferred` (immediate while single-threaded). Not yet done: deferring
the free of the dict *data block* on resize (master's new storage layout) and list item
arrays — lock-free readers of those can still race a resize.
Semantics note: with >1 attached thread, finalizers of retired objects run a few
eval-breaker polls later (CPython frees immediately and delays only memory reuse; that
needs allocator support — see next steps).

## GC counters — `gc.rs`
Tracked/allocation counters are per-thread deltas published in batches of 32 (two
global atomic RMWs per tracked object before).

## Startup
* `slots::fixup_slots_new_type` (used by `type_new`): with a single base only the slots
  named in the class's own dict are recomputed (the rest were inherited by
  `type_ready`); `fixup_slots` (full) stays for `__bases__` assignment / _datetime.
* `methods::leak_cstr`: method/member/getset names and docs bump-allocated from
  64 KiB chunks instead of one malloc each.

## Bug fixes on the way
* subtype_dealloc released the old type when `__del__` assigned `__class__`
  (test_descr.test_set_class segfault; master fixed it the same way concurrently).

## Measurements (callgrind instructions, whole process; machine load ~40 on 4 cores,
so wall-clock numbers are indicative only)
| metric | before (master 28f7ac8) | after | CPython default | CPython ft |
|---|---|---|---|---|
| `-c pass` | 46.0M | 44.4M (merged build) | 40.5M | 51.0M |
| `-S -c pass` | 20.8M | 20.7M | 27.7M | 34.1M |
| fib(25) | 224.7M | 194.6M | 164.9M | – |
| 50k `P(i,i,i)` instances | 391.7M | 233.7M | – | – |
| objs.py 200k instances: peak RSS | 102.8 MB | 49.3 MB | 35.9 MB | 42.1 MB |
| objs.py wall (ms) | 1904 | 299–900 | 338 | 692 |
| `-c pass` peak RSS | 15.5 MB | 15.7 MB | 9.2 MB | 10.8 MB |
Merged build: t_shapes.py OK (JIT on/off/threshold 0), t_threads.py OK.


Spec smoke (tools/bench/perf/smoke.sh, before → after shapes): identical results except
test_gc 20 → 17 failures; test_descr crashed in both (fixed since); test_class /
test_capi / test_pickle / test_threading do not import on this master (missing
_testinternalcapi / http / SIGINT handling) — not a usable signal yet.
Own tests: tools/bench/perf/t_shapes.py OK (JIT on/off/threshold 0);
tools/bench/perf/t_threads.py failed before the dict-deferral change (use-after-free
of dict values read lock-free by the inline caches) — re-check after the merge build.

## Next steps
1. Re-run quick.sh / smoke.sh on the merged build; fix anything t_threads.py shows.
2. Keep inline values after `__dict__` access (dict view over the values) so
   `self.__dict__.update(kw)` objects stay fast (CPython semantics of has_inline_values).
3. Tier-1 inline code for StoreAttr / METHOD_SHADOWABLE on values-mode objects; shape-id
   guards for jit2's tier 2.
4. D6 for storage: defer freeing dict data blocks and list item arrays on resize; better:
   allocator-level delayed reuse of freed blocks while multi-threaded + try-incref
   readers (immediate finalization semantics like CPython ft).
5. Scaling: GC list spin lock + per-object track/untrack atomics; shared refcounts on
   functions/code/types from non-owner threads (deferred refcounting for those).
6. Startup: snapshot of post-bootstrap state or frozen stdlib code; lazy type readying.
7. RSS: `-c pass` RssFile 10.7 MB vs 6.1 MB (binary text pages touched) and RssAnon
   6.5 MB vs 3.3 MB.

# perf2 workstream (branch worktree-agent-ad5580471f243533d)

## What changed (all from scratch)
* **Link-time code layout** (`crates/launcher/build.rs`, `startup.ld`, `tools/bench/perf/gen_order.sh`):
  functions run at startup / in common micro paths are grouped in `.text.startup_hot` via a
  linker-script fragment (matches `.text.NAME`, `.llvm.*`, `.unlikely.`/`.hot.` variants and
  ICF aliases). Regenerate with `gen_order.sh PY` after larger changes (stale order costs
  ~1 MB RSS). `-z pack-relative-relocs` (RELR) shrinks .rela.dyn 370 KB -> 5 KB.
* **tlab.rs**: lazy page carving (2 KiB per refill, page header records the carved end),
  allocation *kinds* (0 raw, 1 GC, 2 GC+managed pre-header) on separate pages, 8-byte size
  classes below 96 for GC kinds, segment registry, `for_each_gc_block` heap walk.
  `alloc::buf_calloc/buf_realloc/buf_free`: small container storage from the tlab
  (list item arrays, set tables; frees accept malloc memory too).
* **GC without list links** (gc.rs/alloc.rs): GC objects lose the 16-byte GcHead; track/untrack
  are bit flips; collections walk the GC pages + a registry of large GC objects; old
  generation = `GC_BIT_OLD`; graph index kept in `ob_tid` during a collection. Young
  interval = max(threshold, live/4) (walk cost amortised). No global spin lock any more.
* **Shapes** (shape.rs): managed word = `shape|1`, values at `obj + shape.voff`, capacity in the
  high byte of `ob_flags` (`capacity_offset()`), no 16-byte values header. JIT: tier-1 and
  jit3's tier-2 (`opt/build.rs`) ported; `transitions_adding` kept. has_inline_values follows
  CPython for test_class (dict materialised from inline values counts while <= 30 entries).
* **Code objects**: co_linetable is CPython's location-table format (test_code parser; random
  access via lazy checkpoints), no duplicated Vec<Location>/consts/names; **lazy bodies**:
  marshal code format 3 = header + docstring + length-prefixed self-contained body, loaded on
  first `code_data()` (pyc magic includes `marshal::CODE_FORMAT`). Tier-1 direct calls check
  `data != NULL`. Interning: flat open-addressing table, `intern_latin1` without allocation.
* **Lists (D6)**: lock-free readers (`get_signed`, JIT inline subscript) are safe with threads:
  arrays never shrink/realloc in place while shared, replaced arrays and removed items go
  through QSBR; JIT MULTI_THREADED guard removed from list subscript (iterator guard kept).
* **Per-thread heap-type refcounts** (object.rs): instance alloc/free on non-owner threads
  count in a thread-local array (`unique_id`), merged at collections/thread exit, bias
  discounted by the GC. Removes the shared-refcount hot line on classes.
* Misc: method names/docs NUL-terminated at compile time (`mdefs!` takes literals), buffered
  reads no longer re-zero 128 KiB per refill, slices tracked only with GC members,
  length_hint skips len() for types without a length slot, bytearray slice race panic fixed.

## Measurements (quiet machine except where noted; callgrind = deterministic)
| metric | before (master at start) | after | CPython default | CPython ft |
|---|---|---|---|---|
| `-c pass` instructions | 44.3M | **35.0M** | 40.6M | 50.9M |
| `-c pass` wall best-of-30 (quiet) | 16.8 ms | **13.4 ms** | 14.5 ms | 16.9 ms |
| `-c pass` peak RSS in-process (median) | 15.9 MB | 9.76 MB | 9.08 MB | 10.64 MB |
| `-c pass` RSS at exit (gdb) | 16.8 MB | 9.26 MB | 8.9 MB | – |
| objs.py 200k instances peak RSS | 49.7 MB | **35.2–35.4 MB** | 36.0 MB | 42.2 MB |
| objs.py instructions | – | 1216M (was 1503M before GC interval fix) | 1083M | – |
| hexiom per loop tier-1 only | 89M (regressed) | 55.5M | 58M | – |
| nqueens per loop (tier 2) | 1544M | 1419M | 841M | – |
| fannkuch per loop tier-1 / tier-2 | 9209M (coordinator) | 6457M / 5995M | – | – |
CPU-bound scaling (scal2, 1->4 threads, quiet run): py-ft 1/1.96/2.70/2.40, py-default ~1.0;
ours finishes the same work in ~10 ms (tier 2), too short to time; under load cpu-per-unit
rises <=20% at 4 threads (object-heavy: 0.167 -> 0.192 after per-thread type counts).
Remaining gaps: `-c pass` RSS +0.7 MB vs default (anon 3.54 vs 3.29 MB: dict tables, code
caches, tlab partial pages; RssFile 6.2 vs 5.9 MB); object-model throughput (alloc_object
~100 instr/call, module-level dict-name loops in objs.py, Vec temporaries in tuple/list
builders); wall-clock scaling must be re-measured on an idle machine.

## Next steps
1. Shrink dicts (1/2-byte indices, 16-byte str-key entries); code-cache tables only on 2nd run.
2. alloc_object fast path (specialised per kind, no generic zeroing switch).
3. Deferred refcounts for functions/code (LOAD_GLOBAL increfs from all threads).
4. List iterator lock-free path (JIT `for_iter` still guarded by MULTI_THREADED).
5. Validate lazily loaded code bodies at load (corrupt bodies abort at first call today).
