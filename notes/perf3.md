# perf3 workstream — library and runtime hot paths

Branch: the perf3 worktree (based on master 991be0c, after jit5). Everything written from
scratch. Goal: the library/runtime-heavy pyperformance benchmarks where jit5's wall-clock
table showed the largest losses against the best CPython build.

## Method
* `tools/bench/perloop.sh` (callgrind instructions per benchmark loop, steady state) for
  ours, py-default and py-ft; per-loop *function* profiles by diffing two callgrind runs
  with different loop counts (scratchpad `dp.sh`), plus micro scripts measuring one
  operation (`(I(N2)-I(N1))/(N2-N1)`).
* Caveat: several benchmarks (gc_traversal, gc_collect, pathlib) vary by up to ~30% between
  runs of the same binary under callgrind (automatic collections and JIT recompiles land in
  different windows); numbers below are single runs unless noted.

## Changes

### binascii / bytes (base64)
* Inputs are borrowed (buffer export or the ASCII str's own bytes) instead of copied into
  a `Vec`; outputs are written straight into a bytes object allocated at the final (or an
  upper-bound, then shrunk in place) size (`emit`).
* Fast loops: base64 encode 3->4 with a fixed table, decode whole quads of alphabet
  characters before falling back to the general state machine (identical behaviour in
  every mode while no padding/invalid char has been seen); base32 groups of 5/8; hex via a
  256-entry digit table; ascii85/base85 groups without per-group vectors.
* `alloc::alloc_object_payload_uninit` + `bytes::try_alloc_bytes_uninit`: large bytes
  payloads that are overwritten anyway skip calloc's clearing (memset was 7% of base64).
  `new_bytes`/`try_new_bytes` use it.
* `bytes.translate`: 256-byte local map, branch-free mapping pass + compare when there is no
  delete set.

### str building / json
* `StrBuilder` collects one byte per character while everything is below U+0100 and only
  widens to `u32` on the first wider character (was always `Vec<u32>` + a max scan).
  Used by repr of containers, json, printf-style formatting, the C API writer.
* json encoder (`_json.rs`): circular-reference check with a private identity stack (set
  above depth 32) when the `markers` dict is empty at call start (the dict stays empty, as
  after any successful encode); exact dicts iterate an item snapshot without building
  2-tuples and a list; exact ints are written digit by digit, floats via `float_repr`
  without a temporary float object; plain-ASCII/Latin-1 strings are copied in runs;
  positional-argument fast paths for `make_encoder(...)` and `encoder(obj, level)`.
* `json.dumps`/`json.loads` (Lib/json/__init__.py): calls with no options go straight to
  the shared encoder/decoder without building option dicts.
* json scanner: key memo is a per-call Rust table (no dict + clear per call), objects are
  filled directly into the dict when there is no `object_pairs_hook`, machine-word ints are
  accumulated without a temporary `String`, positional fast path for `scan_once`.

### sets, min/max
* Lock-free set algebra (`set.rs`): when a table cannot change underneath us (frozensets;
  any set while only one thread exists) `issubset`, `difference`, `intersection`,
  `difference_update`, `update`/merge, copy and iteration walk the tables directly. Keys
  are compared without Python code when they are identical or both exact str/int; the
  first pair that would need `__eq__` makes the operation fall back to the old locking
  snapshot implementation. Results under construction get keys with `insert_unique`
  (no comparisons); `copy_fast` duplicates a table without dummies position by position.
* `min()`/`max()` without key over all-compact-int or all-float items compare natively.
* `collect_iterable` of exact sets/frozensets copies the table directly.

### calls and instantiation
* `type_vectorcall` handles one-argument `str(x)`, `type(x)`, `tuple(x)`, `list(x)`
  (exact-size copies of lists/tuples) and `weakref.ref(x)` without an argument tuple; the
  four types now dispatch through it (`types/mod.rs`).
* Classes whose `__new__` is a Python function (staticmethod, plain `type` metaclass):
  `__new__` and a Python `__init__` are called with the vector arguments
  (`instantiate_with_new`), no args tuple / kwargs dict / `slot_tp_new` getattr.
* LOAD_ATTR/LOAD_METHOD caches on classes (`specialize.rs`, flagged): non-data attributes
  of `type` itself (`__new__`, `__init__`, `mro`, ...) no longer prevent caching a class's
  own attribute of that name (`object.__new__(cls)` missed the cache on every call).
* CALL_FUNCTION_EX (eval and the tier-1/2 helper, flagged small changes): callables without
  vectorcall get the argument tuple itself (`abstract_::call_ex_tuple`), others a stack
  vector; LIST_EXTEND goes through `list::extend`, which copies tuples/lists directly.
* `list::list_to_tuple` (INTRINSIC_LIST_TO_TUPLE, `tuple(list)`, `sequence_tuple`),
  `list.extend(tuple)` without temporary vectors.
* tuple rich comparison compares the item slices directly.
* lru_cache: the argument tuple itself is the key when there are no keywords (as CPython).

### dicts
* Dicts created empty share one never-written table (`empty_table`, marked by capacity 0);
  the first write (anything going through `dict::table`) installs a private table with a
  compare-and-swap (a lock-free reader may race the writer). `find` reads the shared table
  without swapping. An unused `**kwargs` or `{}` no longer allocates a table. Sharing the
  version stamps among empty dicts is sound because their contents are identical.

### GC
* Trial-deletion state lives in the object headers during a collection (`ob_tid` =
  `gc_refs << 3 | reachable << 2 | tag`, owners restored afterwards) instead of index
  arrays; exact lists and tuples are traversed with the visitor inlined.
* The collector's helper sets/maps (`countable`, legacy finalizers, finalization order,
  weakref detaching) use the address hasher instead of SipHash (30% of gc_collect).

### Bug fixed on the way
* `lru_cache` (bounded): the Rust node list was mutated by concurrent callers without any
  lock — heap corruption ("double free or corruption", "unaligned tcache chunk") in
  test_functools `test_lru_cache_threaded2` (rare before, frequent once the dict changes
  made calls faster). List and matching dict updates now run under the wrapper's lock once
  a second thread exists (released around the user function call).

## Measurements
Callgrind instructions per loop (millions). "before" = master 991be0c built in this
worktree; CPython columns from the same harness.

| bench | before | after | CPython default | ratio after |
|---|---|---|---|---|
| base64 | 1284.3 | 466.5 | 570.9 | 0.82 |
| json_dumps | 171.5 | 93.9 | 102.1 | 0.92 |
| json_loads | 7.23 | 5.48 | 5.70 | 0.96 |
| gc_traversal | 70.8 | 22-33 | 54.0 (ft 13.4) | 0.4-0.6 / 1.6-2.5 vs ft |
| gc_collect | 3.8-4.3 | 2.8 | 9.2 | 0.31 |
| meteor_contest | 1527.9 | 994-1045 | 850.8 | 1.17-1.23 |
| typing_runtime_protocols | (noisy) | 2.70 | 1.73 | 1.56 |
| pathlib | 115-201 (noisy) | 178-192 | 87.2 | ~2 |
| deepcopy | 4.21 | 4.38 | 2.83 | 1.55 |
| xml_etree | 5442 | 5400 | 4075 | 1.33 |

Micro-operations (instructions per operation, ours before -> after, CPython default):
`str(s)` 1287 -> 301 (348); `list(t)` 1651 -> 910 (877); `f(*t)` 2281 -> 1768 (658);
`object.__new__(C)` 1814 -> 1443 (924); `N(s)` with Python `__new__`+`__init__` 6532 -> 5100
(3521); `def f(*a, **k)` call 1758 -> 1468 (903); `inspect.getattr_static` miss 48490 -> 38416
(26371).

## What is left (measured causes)
* Python-level call overhead dominates pathlib, typing_runtime_protocols, deepcopy: a
  non-inlined Python call costs ~500 instructions here vs ~150-200 in CPython (h_call /
  call_function / bind_arguments / frame release / run_frame; jit5's open item "contiguous
  frame stack"). pathlib per path: 12.6K vs 5.7K instructions.
* Allocation: `alloc_object` ~100-110 instructions per object, `free_object`+`free_small`
  ~50; object-heavy benchmarks spend 10-15% there.
* gc_traversal vs py-ft: ft marks everything reachable from the roots (including thread
  stacks) in one pass; we still do two passes over all tracked objects.
